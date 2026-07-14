//! The `Gateway` orchestrator, split by concern:
//!
//! - [`mod@self`] — the `Gateway` struct, construction, provider/plugin
//!   registration, shared accessors, and API-key resolution
//! - [`route`] — the non-streaming request path (guards → strategy →
//!   failover → plugins/guardrails → accounting), plus `embed`
//! - [`stream`] — the streaming path and its usage recorder / guardrail
//!   stream wrapper
//! - [`policy`] — rate-limit / budget / org-guardrail / RBAC checks
//! - [`audit`] — audit events and the request accounting shared by the
//!   streaming and non-streaming paths
//! - [`config`] — config apply/reload/rollback and version history
//! - [`rebuild`] — rebuilding routing targets from DB models/endpoints
//! - [`providers`] — the provider-client factory for DB endpoints
//! - [`proxy`] — the catch-all `/v1/*` passthrough proxy

mod audit;
mod config;
mod policy;
mod providers;
mod proxy;
mod rebuild;
mod route;
mod stream;

use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use himadri_circuitbreaker::CircuitBreakerTrait;
use himadri_core::{Config, GatewayError, Target};
use himadri_observability::{AuditLog, Metrics};
use himadri_plugin::PluginManager;
use himadri_provider::traits::Provider;
use himadri_ratelimit::RateLimiter;

use crate::strategy::Strategy;

use self::config::ConfigHistory;

/// Whether `target` can serve `model`. `None` in `target.models` means the
/// target advertises no explicit model list and serves any model (env/config
/// targets); otherwise the requested model must appear in the list.
fn target_serves_model(target: &Target, model: &str) -> bool {
    match &target.models {
        None => true,
        Some(models) => models.iter().any(|m| m == model),
    }
}

/// The key under which a target's provider client, circuit breaker, and API key
/// are stored. DB endpoints use their unique endpoint id (`target.id`) so
/// same-type endpoints don't collide; env/config targets fall back to
/// `provider`, matching how their clients are registered at startup.
fn routing_key(target: &Target) -> &str {
    target.id.as_deref().unwrap_or(&target.provider)
}

/// Lock-order invariant: when acquiring more than one of the `RwLock` fields
/// (`strategy`, `config`, `targets`), always acquire in that order —
/// `strategy` → `config` → `targets` — and never take an earlier lock while
/// holding a later one. Both `apply_config` and `rebuild_targets_from_db`
/// follow this order; violating it can deadlock concurrent admin calls
/// (e.g. a config reload racing a provider mutation).
pub struct Gateway {
    /// Arc-shared so long-lived collaborators (e.g. the PII guardrail
    /// plugin) can resolve per-org settings against the *live* config —
    /// admin reloads apply to them without re-wiring.
    config: Arc<RwLock<Config>>,
    providers: DashMap<String, Arc<dyn Provider>>,
    plugin_manager: Arc<PluginManager>,
    strategy: RwLock<Strategy>,
    circuit_breakers: DashMap<String, Arc<dyn CircuitBreakerTrait>>,
    targets: RwLock<Vec<Target>>,
    /// Decrypted API keys for DB-registered providers, keyed by provider name.
    /// Kept out of `Target` so keys never serialize into `/admin/config`
    /// responses or config history.
    provider_keys: DashMap<String, String>,
    rate_limiter: RateLimiter,
    audit_log: Arc<AuditLog>,
    metrics: Arc<Metrics>,
    usage_store: Arc<himadri_admin::UsageStore>,
    request_log: Arc<dyn himadri_admin::RequestLogStore>,
    response_cache: Option<Arc<himadri_plugins::ResponseCachePlugin>>,
    config_history: RwLock<ConfigHistory>,
}

impl Gateway {
    pub fn new(config: Config, metrics: Arc<Metrics>) -> Self {
        let strategy = Strategy::from_strategy_config(&config.strategy);
        let plugin_manager = Arc::new(PluginManager::new());
        let rate_limiter = RateLimiter::new(
            config.rate_limit.requests_per_second,
            config.rate_limit.burst_size,
        );
        // Audit sink/content-capture from env: AUDIT_LOG_DIR selects a JSONL
        // file sink (otherwise events go to tracing); prompt/response content
        // is captured only when AUDIT_CAPTURE_CONTENT=true, and is always
        // redacted. Metadata-only by default so user content never lands in
        // logs or telemetry.
        let audit_dir = std::env::var("AUDIT_LOG_DIR")
            .ok()
            .map(std::path::PathBuf::from);
        let capture_content = std::env::var("AUDIT_CAPTURE_CONTENT")
            .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
            .unwrap_or(false);
        let audit_log = Arc::new(AuditLog::with_options(audit_dir, true, capture_content));
        let usage_store = Arc::new(himadri_admin::UsageStore::new());
        let request_log: Arc<dyn himadri_admin::RequestLogStore> =
            Arc::new(himadri_admin::InMemoryRequestLogStore::new());

        let mut history = ConfigHistory::default();
        history.record(config.clone(), None);

        Self {
            config: Arc::new(RwLock::new(config.clone())),
            providers: DashMap::new(),
            plugin_manager,
            strategy: RwLock::new(strategy),
            circuit_breakers: DashMap::new(),
            targets: RwLock::new(config.targets),
            provider_keys: DashMap::new(),
            rate_limiter,
            audit_log,
            metrics,
            usage_store,
            request_log,
            response_cache: None,
            config_history: RwLock::new(history),
        }
    }

    /// Enable exact-match response caching. When set, non-streaming completions
    /// are served from cache on a hit and populated on a successful miss.
    pub fn set_response_cache(&mut self, cache: Arc<himadri_plugins::ResponseCachePlugin>) {
        self.response_cache = Some(cache);
    }

    /// Replace the request-log store. Defaults to an in-memory store (lost on
    /// restart); call this with a persistent backend (e.g. Postgres) to durably
    /// retain request logs.
    #[cfg_attr(not(feature = "postgres"), allow(dead_code))]
    pub fn set_request_log_store(&mut self, store: Arc<dyn himadri_admin::RequestLogStore>) {
        self.request_log = store;
    }

    pub fn register_provider(&self, provider: Arc<dyn Provider>) {
        self.providers.insert(provider.name().to_string(), provider);
    }

    /// Register a provider client under an explicit routing key (e.g. an
    /// endpoint id) rather than the client's own `name()`. Lets several
    /// endpoints of the same provider type coexist with distinct creds/URLs.
    fn register_provider_as(&self, key: &str, provider: Arc<dyn Provider>) {
        self.providers.insert(key.to_string(), provider);
    }

    pub fn set_plugin_manager(&mut self, manager: PluginManager) {
        self.plugin_manager = Arc::new(manager);
    }

    /// Handle onto the live config, for collaborators that must observe
    /// admin reloads (e.g. the PII guardrail plugin's per-org settings).
    /// Read-only by convention: all writes go through `apply_config`.
    pub fn config_handle(&self) -> Arc<RwLock<Config>> {
        self.config.clone()
    }

    pub fn list_providers(&self) -> Vec<String> {
        self.providers.iter().map(|r| r.key().clone()).collect()
    }

    pub fn get_provider(&self, name: &str) -> Option<std::sync::Arc<dyn Provider>> {
        self.providers.get(name).map(|r| r.value().clone())
    }

    /// Shared handle to the audit log, for components outside the gateway (e.g.
    /// the auth middleware recording auth failures).
    pub fn audit_log_arc(&self) -> Arc<AuditLog> {
        self.audit_log.clone()
    }

    pub fn metrics(&self) -> Arc<Metrics> {
        self.metrics.clone()
    }

    pub fn usage_store(&self) -> Arc<himadri_admin::UsageStore> {
        self.usage_store.clone()
    }

    pub fn request_log(&self) -> Arc<dyn himadri_admin::RequestLogStore> {
        self.request_log.clone()
    }

    pub fn get_api_key(&self, target: &Target) -> Result<String, GatewayError> {
        if let Some(env_var) = &target.api_key_env {
            std::env::var(env_var).map_err(|_| {
                GatewayError::ServiceUnavailable(format!(
                    "Missing API key environment variable: {}",
                    env_var
                ))
            })
        } else if let Some(key) = self.provider_keys.get(routing_key(target)) {
            Ok(key.clone())
        } else {
            // No stashed key is a legitimate state, not an error: env-registered
            // providers carry their credentials inside the client config, and
            // DB endpoints may be keyless (e.g. a local OpenAI-compatible
            // server). `rebuild_targets_from_db` repopulates `provider_keys`
            // without an empty window, so a keyed endpoint can't land here
            // transiently during a rebuild.
            Ok(String::new())
        }
    }
}
