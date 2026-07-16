//! Plugin-pipeline wire-up: the one place that decides what env-driven
//! processing runs on every request.
//!
//! Two layers, mirroring `wire::providers`: [`PluginSettings::from_env`] is the
//! only place the environment is read (and the inventory of every
//! plugin-related env var), and [`build`] is pure composition over those
//! settings — which is what makes registration and ordering testable without
//! booting the binary.

use std::sync::Arc;

use himadri_core::Config;
use himadri_observability::Metrics;
use himadri_plugin::PluginManager;
use himadri_plugins::{
    BudgetConfig, BudgetPlugin, MaxTokenPlugin, RateLimitConfig, RateLimitPlugin,
    RequestLoggerPlugin, ResponseCachePlugin, WordFilterPlugin,
};
use tracing::info;

/// Everything the environment contributes to plugin composition, parsed.
/// Constructed via [`Self::from_env`] in production; tests build it directly.
#[derive(Debug, Clone, Default)]
pub struct PluginSettings {
    /// `WORD_FILTER_BLOCKLIST` (comma-separated). Empty = no word filter.
    pub word_filter: Vec<String>,
    /// `MAX_TOKENS_LIMIT`.
    pub max_tokens_limit: Option<u32>,
    /// `BUDGET_SPEND_LIMIT_USD`.
    pub budget_spend_limit_usd: Option<f64>,
    /// `BUDGET_INPUT_PER_M_TOKENS`.
    pub budget_input_per_m: Option<f64>,
    /// `BUDGET_OUTPUT_PER_M_TOKENS`.
    pub budget_output_per_m: Option<f64>,
    /// `RATE_LIMIT_KEY_RPM`.
    pub rate_limit_key_rpm: Option<u64>,
    /// `RATE_LIMIT_IP_RPM`.
    pub rate_limit_ip_rpm: Option<u64>,
    /// `CACHE_TTL_SECS` (+ `CACHE_MAX_ENTRIES`, default 10_000).
    pub cache: Option<CacheSettings>,
    /// `GUARDRAILS_PII_*` global request-side defaults.
    #[cfg(feature = "guardrails")]
    pub pii_defaults: Option<himadri_plugins::PiiGuardrailSettings>,
    /// `GUARDRAILS_PII_RESPONSE_MODE` global response-side defaults.
    #[cfg(feature = "guardrails")]
    pub pii_response_defaults: Option<himadri_plugins::PiiResponseSettings>,
}

/// Exact-match response cache sizing (`CACHE_TTL_SECS` / `CACHE_MAX_ENTRIES`).
#[derive(Debug, Clone, Copy)]
pub struct CacheSettings {
    pub ttl_secs: u64,
    pub max_entries: u64,
}

impl PluginSettings {
    pub fn from_env() -> Self {
        Self {
            word_filter: std::env::var("WORD_FILTER_BLOCKLIST")
                .map(|v| himadri_core::env::split_csv(&v))
                .unwrap_or_default(),
            max_tokens_limit: himadri_core::env::parse_var("MAX_TOKENS_LIMIT"),
            budget_spend_limit_usd: himadri_core::env::parse_var("BUDGET_SPEND_LIMIT_USD"),
            budget_input_per_m: himadri_core::env::parse_var("BUDGET_INPUT_PER_M_TOKENS"),
            budget_output_per_m: himadri_core::env::parse_var("BUDGET_OUTPUT_PER_M_TOKENS"),
            rate_limit_key_rpm: himadri_core::env::parse_var("RATE_LIMIT_KEY_RPM"),
            rate_limit_ip_rpm: himadri_core::env::parse_var("RATE_LIMIT_IP_RPM"),
            cache: himadri_core::env::parse_var("CACHE_TTL_SECS").map(|ttl_secs| CacheSettings {
                ttl_secs,
                max_entries: himadri_core::env::parse_var("CACHE_MAX_ENTRIES").unwrap_or(10_000),
            }),
            #[cfg(feature = "guardrails")]
            pii_defaults: himadri_plugins::PiiGuardrailSettings::from_env(),
            #[cfg(feature = "guardrails")]
            pii_response_defaults: himadri_plugins::PiiResponseSettings::from_env(),
        }
    }
}

/// The assembled request pipeline. The response cache rides along because its
/// composition decision is env-driven like the plugins', even though it
/// attaches to the gateway as a field rather than a `PluginManager` stage.
pub struct WiredPlugins {
    pub manager: PluginManager,
    pub response_cache: Option<Arc<ResponseCachePlugin>>,
}

/// Assemble the plugin pipeline (PII guardrail, word filter, max-token,
/// request logger, budget, rate limit) and the response cache from parsed
/// settings. Pure composition: no environment access.
#[allow(unused_variables)] // config/handle/metrics unused without the guardrails feature
pub fn build(
    settings: PluginSettings,
    config: &Config,
    config_handle: Arc<tokio::sync::RwLock<Config>>,
    metrics: &Arc<Metrics>,
) -> WiredPlugins {
    let mut manager = PluginManager::new();

    // PII guardrail: always registered (feature-gated) and resolved per
    // request against the live config, so orgs can be onboarded/opted out
    // via /admin/config reloads without a restart. Runtime activation is
    // the global defaults in `settings` and/or `guardrails.pii` config
    // sections; with neither, the plugin no-ops per request. Registered
    // first so every downstream plugin (word filter, logger, budget) and
    // the response cache see redacted content.
    #[cfg(feature = "guardrails")]
    {
        // Fail closed if guardrails (request or response) are configured.
        let configured = settings.pii_defaults.is_some()
            || settings.pii_response_defaults.is_some()
            || config_mentions_pii(config)
            || config_mentions_pii_response(config);
        match himadri_plugins::RedactCoreEngine::new(himadri_plugins::EngineSecrets::from_env()) {
            Ok(engine) => {
                match &settings.pii_defaults {
                    Some(s) => info!(
                        "Registered PII guardrail (global default mode: {:?}, strategy: {:?})",
                        s.mode, s.options.strategy
                    ),
                    None => info!(
                        "Registered PII guardrail (no global default; config-driven per org/team)"
                    ),
                }
                let engine: Arc<dyn himadri_plugins::PiiEngine> = engine;
                manager.register(himadri_plugins::PiiGuardrailPlugin::with_config(
                    engine.clone(),
                    settings.pii_defaults.clone(),
                    config_handle.clone(),
                    Some(metrics.clone()),
                ));
                // Response-side scanning (mode from `response_mode` config
                // fields / GUARDRAILS_PII_RESPONSE_MODE). Same engine, same
                // per-scope resolution. Off by default.
                manager.register_response_guardrail(
                    himadri_plugins::PiiResponseGuardrail::with_config(
                        engine,
                        settings.pii_response_defaults.clone(),
                        config_handle,
                        Some(metrics.clone()),
                    ),
                );
            }
            // Refuse to start rather than silently run without a guardrail
            // the operator explicitly configured (fail-closed, SPEC §6.3).
            Err(e) if configured => {
                panic!("PII guardrail configured but engine failed to build: {e}")
            }
            Err(e) => {
                tracing::error!("PII guardrail engine failed to build; guardrails unavailable: {e}")
            }
        }
    }

    // Word filter is opt-in (a hardcoded default blocklist used to reject
    // any prompt containing e.g. "password" on every deployment).
    if !settings.word_filter.is_empty() {
        info!(
            "Registered word filter with {} blocked word(s)",
            settings.word_filter.len()
        );
        manager.register(WordFilterPlugin::new(settings.word_filter));
    }

    // Global max_tokens cap is opt-in (used to be a hardcoded 4096 that
    // rejected any larger request).
    if let Some(limit) = settings.max_tokens_limit {
        info!("Registered max-token cap of {}", limit);
        manager.register(MaxTokenPlugin::new(limit));
    }

    manager.register(RequestLoggerPlugin::new());

    // Register the budget plugin when a global spend limit and/or token pricing
    // is configured. Pricing alone is enough: per-principal caps (e.g. a JWT
    // `budget_limit_usd` claim) are enforced against accumulated cost, which
    // requires pricing but not a global limit.
    if settings.budget_spend_limit_usd.is_some()
        || settings.budget_input_per_m.is_some()
        || settings.budget_output_per_m.is_some()
    {
        match BudgetPlugin::new(BudgetConfig {
            spend_limit_usd: Some(settings.budget_spend_limit_usd.unwrap_or(0.0)),
            input_per_m_tokens: settings.budget_input_per_m,
            output_per_m_tokens: settings.budget_output_per_m,
            ..Default::default()
        }) {
            Ok(budget_plugin) => {
                manager.register(budget_plugin);
                match settings.budget_spend_limit_usd {
                    Some(limit) => info!(
                        "Registered budget plugin (global ${:.2} limit; per-principal caps honored)",
                        limit
                    ),
                    None => info!(
                        "Registered budget plugin (no global limit; per-principal caps honored)"
                    ),
                }
            }
            Err(e) => tracing::error!("Budget plugin not registered: {}", e),
        }
    }

    // Register the rate-limit plugin when a per-key and/or per-IP limit is
    // configured. Both scopes share one plugin, and the global limiter stays
    // unset so configuring a key/IP limit doesn't silently impose an
    // unrelated global request cap.
    if settings.rate_limit_key_rpm.is_some() || settings.rate_limit_ip_rpm.is_some() {
        if let Ok(rl_plugin) = RateLimitPlugin::new(RateLimitConfig {
            key_rpm: settings.rate_limit_key_rpm,
            ip_rpm: settings.rate_limit_ip_rpm,
            ..Default::default()
        }) {
            manager.register(rl_plugin);
            if let Some(rpm) = settings.rate_limit_key_rpm {
                info!("Registered rate limit: {} RPM per key", rpm);
            }
            if let Some(rpm) = settings.rate_limit_ip_rpm {
                info!("Registered rate limit: {} RPM per IP", rpm);
            }
        }
    }

    let response_cache = settings.cache.map(|c| {
        info!(
            "Registered response cache ({}s TTL, {} max entries)",
            c.ttl_secs, c.max_entries
        );
        ResponseCachePlugin::new(c.max_entries, std::time::Duration::from_secs(c.ttl_secs))
    });

    WiredPlugins {
        manager,
        response_cache,
    }
}

/// Does the config configure request-side PII scanning? Feeds the fail-closed
/// check: an org/team `pii` section counts **even when currently disabled** —
/// the operator clearly intends to manage guardrails via config, so an engine
/// build failure must be fatal, not silent.
#[cfg(feature = "guardrails")]
fn config_mentions_pii(config: &Config) -> bool {
    config.guardrails.pii.enabled
        || config.orgs.values().any(|org| {
            org.guardrails.pii.is_some()
                || org.teams.values().any(|team| team.guardrails.pii.is_some())
        })
}

/// Same, for response-side scanning (`response_mode != off`).
#[cfg(feature = "guardrails")]
fn config_mentions_pii_response(config: &Config) -> bool {
    use himadri_core::PiiResponseModeConfig;
    let scoped = |g: &himadri_core::OrgGuardrailConfig| {
        g.pii
            .as_ref()
            .is_some_and(|pii| pii.response_mode != PiiResponseModeConfig::Off)
    };
    config.guardrails.pii.response_mode != PiiResponseModeConfig::Off
        || config.orgs.values().any(|org| {
            scoped(&org.guardrails) || org.teams.values().any(|team| scoped(&team.guardrails))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use himadri_plugin::traits::Stage;

    fn build_with(settings: PluginSettings) -> WiredPlugins {
        let config = Config::default();
        let handle = Arc::new(tokio::sync::RwLock::new(config.clone()));
        build(settings, &config, handle, &Arc::new(Metrics::new()))
    }

    /// The minimal pipeline: only the unconditional plugins. Under the
    /// guardrails feature the PII guardrail always registers (it no-ops per
    /// request when unconfigured), and it must come first so downstream
    /// plugins see redacted content — an ordering that used to be enforced
    /// only by a comment.
    #[test]
    fn default_settings_compose_the_minimal_pipeline() {
        let wired = build_with(PluginSettings::default());

        let expected: Vec<&str> = if cfg!(feature = "guardrails") {
            vec!["pii-guardrail"]
        } else {
            vec![]
        };
        assert_eq!(wired.manager.stage_names(Stage::BeforeRequest), expected);
        // The request logger observes completed requests, not incoming ones.
        assert_eq!(
            wired.manager.stage_names(Stage::AfterRequest),
            vec!["request-logger"]
        );
        assert!(wired.response_cache.is_none());

        #[cfg(feature = "guardrails")]
        assert_eq!(
            wired.manager.response_guardrail_names(),
            vec!["pii-response-guardrail"]
        );
    }

    /// Every opt-in feature on: full before-request order pinned, budget's
    /// dual registration lands in after-request, cache is built to size.
    #[test]
    fn full_settings_compose_every_plugin_in_order() {
        let wired = build_with(PluginSettings {
            word_filter: vec!["blocked".to_string()],
            max_tokens_limit: Some(4096),
            budget_spend_limit_usd: Some(100.0),
            budget_input_per_m: Some(1.0),
            budget_output_per_m: Some(2.0),
            rate_limit_key_rpm: Some(60),
            rate_limit_ip_rpm: Some(120),
            cache: Some(CacheSettings {
                ttl_secs: 30,
                max_entries: 500,
            }),
            ..Default::default()
        });

        let mut expected: Vec<&str> = if cfg!(feature = "guardrails") {
            vec!["pii-guardrail"]
        } else {
            vec![]
        };
        expected.extend(["word-filter", "max-token", "budget", "rate-limit"]);
        assert_eq!(wired.manager.stage_names(Stage::BeforeRequest), expected);

        // After-request: the logger (its primary stage), then budget's opt-in
        // second run that records cost from the response.
        assert_eq!(
            wired.manager.stage_names(Stage::AfterRequest),
            vec!["request-logger", "budget"]
        );

        assert!(wired.response_cache.is_some());
    }

    /// Pricing alone (no global limit) is enough to enable the budget plugin —
    /// per-principal caps need cost accounting without a global cap.
    #[test]
    fn pricing_alone_enables_the_budget_plugin() {
        let wired = build_with(PluginSettings {
            budget_input_per_m: Some(1.0),
            ..Default::default()
        });
        assert!(wired
            .manager
            .stage_names(Stage::BeforeRequest)
            .contains(&"budget"));
    }
}
