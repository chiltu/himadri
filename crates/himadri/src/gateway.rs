use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, instrument, warn};

use futures::Stream;
use himadri_circuitbreaker::{CircuitBreaker, CircuitBreakerConfig, CircuitBreakerTrait};
use himadri_core::{
    AuthContext, AuthScope, ChatCompletionRequest, ChatCompletionResponse, Config, GatewayError,
    OrgConfig, StreamChunk, Target,
};
use himadri_observability::{AuditEvent, AuditLog, AuditMessage, AuditStatus, Metrics};
use himadri_plugin::traits::ResponseAction;
use himadri_plugin::PluginManager;
use himadri_provider::traits::{BoxStream, Provider};
use himadri_ratelimit::RateLimiter;

use crate::strategy::Strategy;

static PROXY_CLIENT: once_cell::sync::Lazy<reqwest::Client> = once_cell::sync::Lazy::new(|| {
    reqwest::Client::builder()
        .pool_max_idle_per_host(32)
        .pool_idle_timeout(std::time::Duration::from_secs(90))
        .build()
        .expect("Failed to create proxy HTTP client")
});

/// Pre-built index for fast model-to-provider lookups.
#[derive(Debug, Default)]
#[allow(dead_code)]
struct ModelLookupIndex {
    exact_providers: std::collections::HashMap<String, Vec<String>>,
}

#[allow(dead_code)]
impl ModelLookupIndex {
    fn new() -> Self {
        Self::default()
    }
    fn rebuild(&mut self, providers: &[(String, Vec<String>)]) {
        self.exact_providers.clear();
        for (name, models) in providers {
            for model in models {
                self.exact_providers
                    .entry(model.clone())
                    .or_default()
                    .push(name.clone());
            }
        }
    }
    fn lookup(&self, model: &str) -> Vec<String> {
        self.exact_providers.get(model).cloned().unwrap_or_default()
    }
}

struct AuditContext<'a> {
    request: &'a ChatCompletionRequest,
    auth: Option<&'a AuthContext>,
    ctx: &'a himadri_plugin::PluginContext,
    result: &'a Result<ChatCompletionResponse, himadri_provider::ProviderError>,
    latency_ms: u64,
    guardrail_actions: &'a [String],
}

/// Why a failover attempt produced no result. Infrastructure failures are
/// kept distinct from provider errors so each caller can preserve its error
/// surface: `route` flattens everything into `ProviderError` (for audit /
/// usage records), while `route_stream` maps to the richer `GatewayError`
/// variants (`CircuitOpen`, `ProviderNotFound`).
enum AttemptError {
    NoTargets,
    CircuitOpen(String),
    ProviderNotFound(String),
    ApiKey(GatewayError),
    Provider(himadri_provider::ProviderError),
}

pub struct Gateway {
    config: RwLock<Config>,
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
    #[allow(dead_code)]
    model_index: RwLock<ModelLookupIndex>,
    usage_store: Arc<himadri_admin::UsageStore>,
    request_log: Arc<dyn himadri_admin::RequestLogStore>,
    response_cache: Option<Arc<himadri_plugins::ResponseCachePlugin>>,
    config_history: RwLock<ConfigHistory>,
}

/// In-memory record of applied config versions, enabling `/admin/config/history`
/// and rollback. Backend-agnostic so it works in every build.
#[derive(Default)]
struct ConfigHistory {
    entries: Vec<himadri_admin::ConfigHistoryEntry>,
    next_version: u32,
}

impl ConfigHistory {
    fn record(&mut self, config: Config, rolled_back_from: Option<u32>) {
        let version = self.next_version.max(1);
        self.next_version = version + 1;
        self.entries.push(himadri_admin::ConfigHistoryEntry {
            version,
            updated_at: chrono::Utc::now(),
            config,
            rolled_back_from,
        });
    }
}

impl Gateway {
    pub fn new(config: Config, metrics: Arc<Metrics>) -> Self {
        let strategy = Strategy::from_strategy_config(&config.strategy);
        let plugin_manager = Arc::new(PluginManager::new());
        let rate_limiter = RateLimiter::new(
            config.rate_limit.requests_per_second,
            config.rate_limit.burst_size,
        );
        let audit_log = Arc::new(AuditLog::new(None, false));
        let model_index = RwLock::new(ModelLookupIndex::new());
        let usage_store = Arc::new(himadri_admin::UsageStore::new());
        let request_log: Arc<dyn himadri_admin::RequestLogStore> =
            Arc::new(himadri_admin::InMemoryRequestLogStore::new());

        let mut history = ConfigHistory::default();
        history.record(config.clone(), None);

        Self {
            config: RwLock::new(config.clone()),
            providers: DashMap::new(),
            plugin_manager,
            strategy: RwLock::new(strategy),
            circuit_breakers: DashMap::new(),
            targets: RwLock::new(config.targets),
            provider_keys: DashMap::new(),
            rate_limiter,
            audit_log,
            metrics,
            model_index,
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
        let name = provider.name().to_string();
        let _models = provider.supported_models();
        self.providers.insert(name.clone(), provider);

        // Rebuild model index (best-effort async)
        let _providers: Vec<(String, Vec<String>)> = self
            .providers
            .iter()
            .map(|r| (r.key().clone(), r.value().supported_models()))
            .collect();
        // Note: In production, use tokio::spawn to rebuild async
        // For now, we rebuild inline (fast enough for registration)
    }

    pub fn set_plugin_manager(&mut self, manager: PluginManager) {
        self.plugin_manager = Arc::new(manager);
    }

    /// Run the shared request prologue: rate-limit / budget / guardrail /
    /// RBAC checks followed by the before-request plugins. Any new guard
    /// added here applies to both `route` and `route_stream`.
    async fn prepare_request(
        &self,
        request: &ChatCompletionRequest,
        auth: Option<&AuthContext>,
        remote_ip: Option<String>,
    ) -> Result<himadri_plugin::PluginContext, GatewayError> {
        let config = self.config.read().await;
        self.check_rate_limits(auth, &config)?;
        self.check_token_budgets(auth, &config, request)?;
        self.check_org_guardrails(auth, &config, request)?;
        self.check_rbac_model(auth, &config, &request.model)?;
        drop(config);

        let mut ctx = himadri_plugin::PluginContext::from_request(request, auth);
        ctx.remote_ip = remote_ip;
        self.plugin_manager
            .run_before(&mut ctx)
            .await
            .map_err(|e| GatewayError::BadRequest(e.to_string()))?;
        Ok(ctx)
    }

    /// Select targets via the active strategy (in priority order for
    /// failover) and filter them by the caller's RBAC grants.
    async fn select_targets(
        &self,
        request: &ChatCompletionRequest,
        auth: Option<&AuthContext>,
    ) -> Result<Vec<Target>, GatewayError> {
        let strategy = self.strategy.read().await;
        let targets = self.targets.read().await;
        let ordered = strategy.select_ordered(request, &targets).await?;
        drop(strategy);
        drop(targets);
        self.filter_targets_by_rbac(auth, ordered).await
    }

    /// Execute an operation against an ordered list of targets, advancing to
    /// the next target when a circuit breaker is open or the operation
    /// returns a retryable error. Returns the target actually used, its
    /// result, and the latency of the final attempt.
    ///
    /// Infrastructure failures stay distinct from provider errors (see
    /// [`AttemptError`]) so each caller can keep its own error surface.
    /// `record_latency` feeds per-attempt latency into the least-latency
    /// routing strategy; streaming passes `false` because time-to-open-stream
    /// is not comparable with full completion latency.
    async fn with_failover<T, F, Fut>(
        &self,
        ordered: &[Target],
        record_latency: bool,
        mut op: F,
    ) -> (Target, Result<T, AttemptError>, std::time::Duration)
    where
        F: FnMut(Arc<dyn Provider>, String) -> Fut,
        Fut: std::future::Future<Output = Result<T, himadri_provider::ProviderError>>,
    {
        let mut last_target = ordered[0].clone();
        let mut last_result = Err(AttemptError::NoTargets);
        let mut last_latency = std::time::Duration::ZERO;

        let last_idx = ordered.len() - 1;
        for (idx, candidate) in ordered.iter().enumerate() {
            let is_last = idx == last_idx;
            last_target = candidate.clone();

            // Circuit breaker for this provider (clone the Arc so we hold no
            // DashMap reference across await points).
            let cb = self
                .circuit_breakers
                .entry(candidate.provider.clone())
                .or_insert_with(|| Arc::new(CircuitBreaker::new(CircuitBreakerConfig::default())))
                .clone();

            if !cb.allow().await {
                last_result = Err(AttemptError::CircuitOpen(candidate.provider.clone()));
                if is_last {
                    break;
                }
                continue;
            }

            // Provider lookup.
            let provider = match self.providers.get(&candidate.provider) {
                Some(p) => p.clone(),
                None => {
                    last_result = Err(AttemptError::ProviderNotFound(candidate.provider.clone()));
                    if is_last {
                        break;
                    }
                    continue;
                }
            };

            // API key.
            let api_key = match self.get_api_key(candidate) {
                Ok(k) => k,
                Err(e) => {
                    last_result = Err(AttemptError::ApiKey(e));
                    if is_last {
                        break;
                    }
                    continue;
                }
            };

            // Execute.
            let start = std::time::Instant::now();
            let result = op(provider, api_key).await;
            last_latency = start.elapsed();

            // Circuit breaker + latency bookkeeping for this attempt.
            match &result {
                Ok(_) => cb.record_success().await,
                Err(e) if e.retryable() => cb.record_failure().await,
                Err(_) => {}
            }
            if record_latency {
                let attempt_ms = last_latency.as_millis() as u64;
                if let Strategy::LeastLatency(state) = &*self.strategy.read().await {
                    state.store.record(&candidate.provider, attempt_ms).await;
                }
            }

            match result {
                Ok(v) => {
                    last_result = Ok(v);
                    break;
                }
                Err(e) if e.retryable() && !is_last => {
                    warn!(
                        "Provider {} failed with retryable error, falling back: {}",
                        candidate.provider, e
                    );
                    last_result = Err(AttemptError::Provider(e));
                    continue;
                }
                Err(e) => {
                    last_result = Err(AttemptError::Provider(e));
                    break;
                }
            }
        }

        (last_target, last_result, last_latency)
    }

    /// Generate embeddings, trying configured targets in order and falling back
    /// when a provider doesn't support embeddings or returns a retryable error.
    #[instrument(skip(self, request, auth), fields(model = %request.model))]
    pub async fn embed(
        &self,
        request: himadri_core::EmbeddingRequest,
        auth: Option<&AuthContext>,
    ) -> Result<himadri_core::EmbeddingResponse, GatewayError> {
        let config = self.config.read().await;
        self.check_rate_limits(auth, &config)?;
        self.check_rbac_model(auth, &config, &request.model)?;
        drop(config);

        let targets = self.targets.read().await.clone();
        if targets.is_empty() {
            return Err(GatewayError::Internal("No targets configured".to_string()));
        }
        let targets = self.filter_targets_by_rbac(auth, targets).await?;

        let mut last_err = GatewayError::Internal("No provider produced embeddings".to_string());

        for target in &targets {
            let provider = match self.providers.get(&target.provider) {
                Some(p) => p.clone(),
                None => continue,
            };
            let api_key = match self.get_api_key(target) {
                Ok(k) => k,
                Err(e) => {
                    last_err = e;
                    continue;
                }
            };
            match provider.embed(&request, &api_key).await {
                Ok(resp) => return Ok(resp),
                Err(himadri_provider::ProviderError::Unsupported(_)) => continue,
                Err(e) if e.retryable() => {
                    last_err = GatewayError::Provider(e.to_string());
                    continue;
                }
                Err(e) => return Err(GatewayError::Provider(e.to_string())),
            }
        }

        Err(last_err)
    }

    #[instrument(skip(self, request, auth), fields(model = %request.model))]
    pub async fn route(
        &self,
        request: ChatCompletionRequest,
        auth: Option<&AuthContext>,
        remote_ip: Option<String>,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        // Guards + before-request plugins (shared with route_stream).
        self.prepare_request(&request, auth, remote_ip).await?;

        // Serve from response cache on an exact-match hit (non-streaming only).
        if !request.stream {
            if let Some(cache) = &self.response_cache {
                if let Some(cached) = cache.get(&request).await {
                    debug!("Response cache hit for model {}", request.model);
                    self.metrics.cache_hits_total.inc();
                    return Ok(cached);
                }
                self.metrics.cache_misses_total.inc();
            }
        }

        let ordered = self.select_targets(&request, auth).await?;

        // Try targets in priority order, falling back on retryable failures or
        // open circuit breakers.
        let request_ref = &request;
        let (target, result, latency) = self
            .with_failover(&ordered, true, |provider, api_key| async move {
                provider.complete(request_ref, &api_key).await
            })
            .await;
        let latency_ms = latency.as_millis() as u64;

        // Flatten infrastructure failures into ProviderError, preserving the
        // messages the audit log and API responses have always carried.
        let result = result.map_err(|e| match e {
            AttemptError::Provider(e) => e,
            AttemptError::CircuitOpen(p) => himadri_provider::ProviderError::Internal(format!(
                "Circuit breaker open for provider {}",
                p
            )),
            AttemptError::ProviderNotFound(p) => {
                himadri_provider::ProviderError::Internal(format!("Provider not found: {}", p))
            }
            AttemptError::ApiKey(e) => himadri_provider::ProviderError::Internal(e.to_string()),
            AttemptError::NoTargets => {
                himadri_provider::ProviderError::Internal("No targets attempted".to_string())
            }
        });

        // Run after-request plugins
        let mut ctx = himadri_plugin::PluginContext::from_request(&request, auth);
        ctx.set_provider(target.provider.clone());
        ctx.set_latency(latency);
        if let Ok(ref response) = result {
            if let Some(usage) = &response.usage {
                ctx.set_tokens(usage.total_tokens);
            }
            // Expose the response so after-request plugins (e.g. budget) can
            // record cost from its usage.
            ctx.set_response(response.clone());
        }
        self.plugin_manager
            .run_after(&mut ctx)
            .await
            .map_err(|e| GatewayError::BadRequest(e.to_string()))?;

        // Run output guardrails on response
        let mut guardrail_actions = Vec::new();
        if let Ok(ref response) = result {
            if let Some(ref content) = extract_response_text(response) {
                match self
                    .plugin_manager
                    .run_response_guardrails(&ctx, content)
                    .await
                {
                    Ok(ResponseAction::Allow) => {}
                    Ok(ResponseAction::Reject(reason)) => {
                        guardrail_actions.push(format!("reject: {}", reason));
                        warn!("Response guardrail rejected: {}", reason);
                        let err_result = Err(himadri_provider::ProviderError::Internal(format!(
                            "Guardrail rejected: {}",
                            reason
                        )));
                        self.log_audit(&AuditContext {
                            request: &request,
                            auth,
                            ctx: &ctx,
                            result: &err_result,
                            latency_ms,
                            guardrail_actions: &guardrail_actions,
                        })
                        .await;
                        return Err(GatewayError::BadRequest(format!(
                            "Response blocked by guardrail: {}",
                            reason
                        )));
                    }
                    Ok(ResponseAction::Redact(redacted)) => {
                        guardrail_actions.push("redact".to_string());
                        warn!("Response guardrail redacted content");
                        let mut redacted_response = response.clone();
                        redact_response_text(&mut redacted_response, &redacted);
                        let redacted_result = Ok(redacted_response.clone());
                        self.log_audit(&AuditContext {
                            request: &request,
                            auth,
                            ctx: &ctx,
                            result: &redacted_result,
                            latency_ms,
                            guardrail_actions: &guardrail_actions,
                        })
                        .await;
                        return Ok(redacted_response);
                    }
                    Err(e) => {
                        guardrail_actions.push(format!("error: {}", e));
                        warn!("Response guardrail error: {}", e);
                    }
                }
            }
        }

        self.log_audit(&AuditContext {
            request: &request,
            auth,
            ctx: &ctx,
            result: &result,
            latency_ms,
            guardrail_actions: &guardrail_actions,
        })
        .await;

        // Update metrics with labels
        let provider_label = target.provider.as_str();
        let model_label = request.model.as_str();

        self.metrics
            .requests_total
            .with_label_values(&[provider_label, model_label])
            .inc();
        self.metrics
            .request_duration
            .observe(latency_ms as f64 / 1000.0);
        if let Ok(ref response) = result {
            if let Some(ref usage) = response.usage {
                self.metrics
                    .tokens_input_total
                    .with_label_values(&[provider_label, model_label])
                    .inc_by(usage.prompt_tokens as u64);
                self.metrics
                    .tokens_output_total
                    .with_label_values(&[provider_label, model_label])
                    .inc_by(usage.completion_tokens as u64);
            }
        } else {
            self.metrics
                .provider_errors
                .with_label_values(&[provider_label, model_label])
                .inc();
        }

        // Record usage for admin stats
        let (prompt_tokens, completion_tokens, total_tokens) = match &result {
            Ok(response) => response
                .usage
                .as_ref()
                .map(|u| (u.prompt_tokens, u.completion_tokens, u.total_tokens))
                .unwrap_or((0, 0, 0)),
            Err(_) => (0, 0, 0),
        };
        let cost =
            self.usage_store
                .calculate_cost(&request.model, prompt_tokens, completion_tokens);

        // Record cost metric
        if cost > 0.0 {
            self.metrics
                .cost_usd_total
                .with_label_values(&[provider_label, model_label])
                .inc_by((cost * 1_000_000.0) as u64); // Store as micro-USD for precision
        }

        let api_key_id = auth.and_then(|a| a.key_id.clone());
        self.usage_store.record(himadri_admin::UsageRecord {
            request_id: uuid::Uuid::new_v4().to_string(),
            api_key_id,
            model: request.model.clone(),
            provider: target.provider.clone(),
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cost_usd: cost,
            latency_ms,
            created_at: chrono::Utc::now(),
            success: result.is_ok(),
            error_message: result.as_ref().err().map(|e| e.to_string()),
        });

        // Record request log
        let trace_id = uuid::Uuid::new_v4().to_string();
        let _ = self.request_log.write(himadri_admin::RequestLogEntry {
            trace_id,
            stage: "completed".to_string(),
            model: request.model.clone(),
            provider: target.provider.clone(),
            prompt_tokens,
            completion_tokens,
            total_tokens,
            error_message: result.as_ref().err().map(|e| e.to_string()),
            created_at: chrono::Utc::now(),
        });

        // Populate the response cache on a successful, non-streaming completion.
        if !request.stream {
            if let (Some(cache), Ok(response)) = (&self.response_cache, &result) {
                cache.insert(&request, response.clone()).await;
            }
        }

        result.map_err(|e| GatewayError::Provider(e.to_string()))
    }

    #[instrument(skip(self, request, auth), fields(model = %request.model))]
    pub async fn route_stream(
        &self,
        request: ChatCompletionRequest,
        auth: Option<&AuthContext>,
        remote_ip: Option<String>,
    ) -> Result<
        BoxStream<'static, Result<StreamChunk, himadri_provider::ProviderError>>,
        GatewayError,
    > {
        // Guards + before-request plugins (shared with route).
        let ctx = self.prepare_request(&request, auth, remote_ip).await?;

        let ordered = self.select_targets(&request, auth).await?;

        // Try targets in priority order until one opens a stream. Failover for
        // streaming only applies before the first chunk is produced — once a
        // stream is established we cannot transparently switch providers, and
        // the circuit breaker records success when the stream opens.
        let request_ref = &request;
        let (target, result, _latency) = self
            .with_failover(&ordered, false, |provider, api_key| async move {
                provider.complete_stream(request_ref, &api_key).await
            })
            .await;

        let stream = result.map_err(|e| match e {
            AttemptError::Provider(e) => GatewayError::Provider(e.to_string()),
            AttemptError::CircuitOpen(p) => GatewayError::CircuitOpen(p),
            AttemptError::ProviderNotFound(p) => GatewayError::ProviderNotFound(p),
            AttemptError::ApiKey(e) => e,
            AttemptError::NoTargets => {
                GatewayError::Internal("No targets produced a stream".to_string())
            }
        })?;

        // Log audit event for stream start
        {
            let messages: Vec<AuditMessage> = request
                .messages
                .iter()
                .map(|m| {
                    let content = match &m.content {
                        Some(himadri_core::MessageContent::Text(t)) => t.clone(),
                        Some(himadri_core::MessageContent::Parts(parts)) => parts
                            .iter()
                            .filter_map(|p| match p {
                                himadri_core::ContentPart::Text { text } => Some(text.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join(""),
                        None => String::new(),
                    };
                    AuditMessage {
                        role: format!("{:?}", m.role).to_lowercase(),
                        content,
                    }
                })
                .collect();

            let event = AuditEvent {
                request_id: ctx.request_id.clone(),
                timestamp: chrono::Utc::now(),
                org_id: auth.and_then(|a| a.org_id.clone()),
                team_id: auth.and_then(|a| a.team_id.clone()),
                user_id: auth.and_then(|a| a.user_id.clone()),
                key_id: auth.and_then(|a| a.key_id.clone()),
                model: request.model.clone(),
                provider: Some(target.provider.clone()),
                messages,
                response: None,
                latency_ms: 0,
                tokens_prompt: None,
                tokens_completion: None,
                tokens_total: None,
                status: AuditStatus::Success,
                error: None,
                guardrail_actions: Vec::new(),
                stream: true,
            };
            self.audit_log.log(event);
        }

        // Wrap stream with output guardrails and usage accounting. The
        // recorder fires at stream end (or client disconnect, via Drop),
        // covering the usage/metrics recording that `route` does inline.
        let recorder = StreamUsageRecorder {
            metrics: self.metrics.clone(),
            usage_store: self.usage_store.clone(),
            request_log: self.request_log.clone(),
            provider: target.provider.clone(),
            model: request.model.clone(),
            api_key_id: auth.and_then(|a| a.key_id.clone()),
            started: std::time::Instant::now(),
            usage: None,
            error: None,
            recorded: false,
        };
        let plugin_manager = self.plugin_manager.clone();
        let auth_clone = auth.cloned();
        let request_clone = request.clone();
        let wrapped_stream = wrap_stream_with_guardrails(
            stream,
            plugin_manager,
            request_clone,
            auth_clone,
            recorder,
        );

        Ok(Box::pin(wrapped_stream))
    }

    /// Validate and apply a config to the live gateway (strategy, targets,
    /// limiter/circuit-breaker state) without touching version history.
    async fn apply_config(&self, config: Config) -> Result<(), GatewayError> {
        config.validate()?;
        // Hold all 3 write locks simultaneously to prevent inconsistent reads
        let mut strategy = self.strategy.write().await;
        let mut cfg = self.config.write().await;
        let mut targets = self.targets.write().await;
        *strategy = Strategy::from_strategy_config(&config.strategy);
        *cfg = config.clone();
        *targets = config.targets;
        drop(strategy);
        drop(cfg);
        drop(targets);
        // Clear stale rate limiter and circuit breaker state
        self.rate_limiter.clear();
        self.circuit_breakers.clear();
        Ok(())
    }

    pub async fn reload_config(&self, config: Config) -> Result<(), GatewayError> {
        self.apply_config(config.clone()).await?;
        self.config_history.write().await.record(config, None);
        Ok(())
    }

    /// Return the recorded config versions, newest first.
    pub async fn config_history(&self) -> Vec<himadri_admin::ConfigHistoryEntry> {
        let mut entries = self.config_history.read().await.entries.clone();
        entries.reverse();
        entries
    }

    /// Roll back to a previously recorded config version. The restored config is
    /// applied and recorded as a new version tagged with the version it was
    /// rolled back from.
    pub async fn rollback_config(&self, version: u32) -> Result<(), GatewayError> {
        let target = self
            .config_history
            .read()
            .await
            .entries
            .iter()
            .find(|e| e.version == version)
            .map(|e| e.config.clone())
            .ok_or_else(|| {
                GatewayError::BadRequest(format!("Config version {} not found", version))
            })?;

        self.apply_config(target.clone()).await?;
        self.config_history
            .write()
            .await
            .record(target, Some(version));
        Ok(())
    }

    pub async fn get_config(&self) -> Config {
        self.config.read().await.clone()
    }

    pub fn list_providers(&self) -> Vec<String> {
        self.providers.iter().map(|r| r.key().clone()).collect()
    }

    pub fn get_provider(&self, name: &str) -> Option<std::sync::Arc<dyn Provider>> {
        self.providers.get(name).map(|r| r.value().clone())
    }

    #[allow(dead_code)]
    pub fn rate_limiter(&self) -> &RateLimiter {
        &self.rate_limiter
    }

    #[allow(dead_code)]
    pub fn audit_log(&self) -> &AuditLog {
        &self.audit_log
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

    /// Rebuild targets from database providers/models.
    /// Called when a provider or model is created, updated, deleted, or toggled.
    pub async fn rebuild_targets_from_db(
        &self,
        providers: &[himadri_admin::Provider],
        models: &[himadri_admin::Model],
    ) {
        let mut targets = self.targets.write().await;
        targets.clear();
        self.provider_keys.clear();

        // Build targets from enabled providers
        for provider in providers {
            if !provider.enabled {
                continue;
            }

            // Stash the (already decrypted) key so get_api_key can use it;
            // it must not travel on the serializable Target.
            if let Some(key) = provider.api_key.as_deref().filter(|k| !k.is_empty()) {
                self.provider_keys
                    .insert(provider.name.clone(), key.to_string());
            }

            // Get enabled models for this provider
            let provider_models: Vec<String> = models
                .iter()
                .filter(|m| m.provider_id == provider.id && m.enabled)
                .map(|m| m.name.clone())
                .collect();

            targets.push(Target {
                provider: provider.name.clone(),
                weight: provider.weight,
                models: if provider_models.is_empty() {
                    None
                } else {
                    Some(provider_models)
                },
                api_key_env: None, // API key is now in provider.api_key
                base_url: provider.base_url.clone(),
            });
        }

        // Update config targets
        let mut config = self.config.write().await;
        config.targets = targets.clone();

        // Clear stale rate limiter and circuit breaker state
        self.rate_limiter.clear();
        self.circuit_breakers.clear();
    }

    #[allow(dead_code)]
    pub async fn get_org_config(&self, org_id: &str) -> Option<OrgConfig> {
        let config = self.config.read().await;
        config.orgs.get(org_id).cloned()
    }

    #[allow(dead_code)]
    pub async fn get_orgs(&self) -> Vec<String> {
        let config = self.config.read().await;
        config.orgs.keys().cloned().collect()
    }

    #[allow(dead_code)]
    pub async fn list_models(&self) -> Vec<himadri_core::ModelObject> {
        let targets = self.targets.read().await;
        let mut models = Vec::new();

        for target in targets.iter() {
            if let Some(ref model_list) = target.models {
                for model_id in model_list {
                    models.push(himadri_core::ModelObject {
                        id: model_id.clone(),
                        object: "model".to_string(),
                        created: 0,
                        owned_by: target.provider.clone(),
                    });
                }
            } else {
                if let Some(provider) = self.providers.get(&target.provider) {
                    for model_id in provider.supported_models() {
                        models.push(himadri_core::ModelObject {
                            id: model_id,
                            object: "model".to_string(),
                            created: 0,
                            owned_by: target.provider.clone(),
                        });
                    }
                }
            }
        }

        models
    }

    pub async fn proxy(
        &self,
        method: &str,
        path: &str,
        headers: &axum::http::HeaderMap,
        body: axum::body::Bytes,
    ) -> Result<
        (
            axum::http::StatusCode,
            axum::http::HeaderMap,
            axum::body::Bytes,
        ),
        GatewayError,
    > {
        let targets = self.targets.read().await;
        let target = targets
            .first()
            .ok_or_else(|| GatewayError::Internal("No targets configured for proxy".to_string()))?;

        let provider = self
            .providers
            .get(&target.provider)
            .ok_or_else(|| GatewayError::ProviderNotFound(target.provider.clone()))?;

        let base_url = target
            .base_url
            .clone()
            .unwrap_or_else(|| match provider.name() {
                "openai" => "https://api.openai.com/v1".to_string(),
                "anthropic" => "https://api.anthropic.com".to_string(),
                "gemini" => "https://generativelanguage.googleapis.com".to_string(),
                _ => "https://api.openai.com/v1".to_string(),
            });

        let api_key = self.get_api_key(target)?;
        let url = format!("{}{}", base_url.trim_end_matches('/'), path);

        let mut req_builder = match method {
            "GET" => PROXY_CLIENT.get(&url),
            "POST" => PROXY_CLIENT.post(&url),
            "PUT" => PROXY_CLIENT.put(&url),
            "DELETE" => PROXY_CLIENT.delete(&url),
            "PATCH" => PROXY_CLIENT.patch(&url),
            _ => {
                let m: reqwest::Method = method
                    .parse()
                    .map_err(|_| GatewayError::BadRequest(format!("Invalid method: {}", method)))?;
                PROXY_CLIENT.request(m, &url)
            }
        };

        for (key, value) in headers.iter() {
            if key == "authorization" || key == "host" || key == "content-length" {
                continue;
            }
            req_builder = req_builder.header(key, value);
        }

        if !api_key.is_empty() {
            req_builder = req_builder.header("authorization", format!("Bearer {}", api_key));
        }

        req_builder = req_builder.body(body);

        let resp = req_builder
            .send()
            .await
            .map_err(|e| GatewayError::Provider(format!("Proxy request failed: {}", e)))?;

        let status = axum::http::StatusCode::from_u16(resp.status().as_u16())
            .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR);

        let mut resp_headers = axum::http::HeaderMap::new();
        for (key, value) in resp.headers().iter() {
            if key == "transfer-encoding" || key == "connection" {
                continue;
            }
            if let (Ok(name), Ok(val)) = (
                axum::http::HeaderName::from_bytes(key.as_str().as_bytes()),
                axum::http::HeaderValue::from_bytes(value.as_bytes()),
            ) {
                resp_headers.insert(name, val);
            }
        }

        let resp_body = resp
            .bytes()
            .await
            .map_err(|e| GatewayError::Provider(format!("Failed to read proxy response: {}", e)))?;

        Ok((status, resp_headers, resp_body))
    }

    /// Enforce role-based model access. Admin-scope principals bypass RBAC.
    /// A principal whose roles grant no access is rejected with `403`.
    fn check_rbac_model(
        &self,
        auth: Option<&AuthContext>,
        config: &Config,
        model: &str,
    ) -> Result<(), GatewayError> {
        if !config.rbac.enabled {
            return Ok(());
        }
        let (roles, is_admin): (&[String], bool) = match auth {
            Some(ctx) => (&ctx.roles, ctx.scope == AuthScope::Admin),
            None => (&[], false),
        };
        config
            .rbac
            .check_model(roles, is_admin, model)
            .map_err(|d| GatewayError::Forbidden(d.to_string()))
    }

    /// Retain only the targets whose provider the principal's roles permit,
    /// preserving priority order. Errors with `403` if RBAC leaves no target.
    async fn filter_targets_by_rbac(
        &self,
        auth: Option<&AuthContext>,
        ordered: Vec<Target>,
    ) -> Result<Vec<Target>, GatewayError> {
        let config = self.config.read().await;
        if !config.rbac.enabled {
            return Ok(ordered);
        }
        let (roles, is_admin): (&[String], bool) = match auth {
            Some(ctx) => (&ctx.roles, ctx.scope == AuthScope::Admin),
            None => (&[], false),
        };
        if is_admin {
            return Ok(ordered);
        }

        let mut last_denial: Option<himadri_core::RbacDenial> = None;
        let allowed: Vec<Target> = ordered
            .into_iter()
            .filter(
                |t| match config.rbac.check_provider(roles, is_admin, &t.provider) {
                    Ok(()) => true,
                    Err(d) => {
                        last_denial = Some(d);
                        false
                    }
                },
            )
            .collect();

        if allowed.is_empty() {
            let reason = last_denial
                .map(|d| d.to_string())
                .unwrap_or_else(|| "no permitted provider for your role".to_string());
            return Err(GatewayError::Forbidden(reason));
        }
        Ok(allowed)
    }

    fn check_org_guardrails(
        &self,
        auth: Option<&AuthContext>,
        config: &Config,
        request: &ChatCompletionRequest,
    ) -> Result<(), GatewayError> {
        let auth_ctx = match auth {
            Some(ctx) => ctx,
            None => return Ok(()),
        };

        let org_id = match &auth_ctx.org_id {
            Some(id) => id.clone(),
            None => return Ok(()),
        };

        let org_config = match config.orgs.get(&org_id) {
            Some(c) => c,
            None => return Ok(()),
        };

        // Check allowed/blocked models
        if let Some(ref allowed) = org_config.allowed_models {
            if !allowed.contains(&request.model) {
                return Err(GatewayError::Forbidden(format!(
                    "Model '{}' not allowed for org '{}'",
                    request.model, org_id
                )));
            }
        }
        if let Some(ref blocked) = org_config.blocked_models {
            if blocked.contains(&request.model) {
                return Err(GatewayError::Forbidden(format!(
                    "Model '{}' blocked for org '{}'",
                    request.model, org_id
                )));
            }
        }

        // Check team-level model restrictions
        if let Some(ref team_id) = auth_ctx.team_id {
            if let Some(team_config) = org_config.teams.get(team_id) {
                if let Some(ref allowed) = team_config.allowed_models {
                    if !allowed.contains(&request.model) {
                        return Err(GatewayError::Forbidden(format!(
                            "Model '{}' not allowed for team '{}'",
                            request.model, team_id
                        )));
                    }
                }
                if let Some(ref blocked) = team_config.blocked_models {
                    if blocked.contains(&request.model) {
                        return Err(GatewayError::Forbidden(format!(
                            "Model '{}' blocked for team '{}'",
                            request.model, team_id
                        )));
                    }
                }
            }
        }

        // Check org guardrail config
        if org_config.guardrails.enabled {
            // Check blocked words
            if !org_config.guardrails.blocked_words.is_empty() {
                for message in &request.messages {
                    if let Some(content) = &message.content {
                        let text = match content {
                            himadri_core::MessageContent::Text(t) => t.clone(),
                            himadri_core::MessageContent::Parts(parts) => parts
                                .iter()
                                .filter_map(|p| match p {
                                    himadri_core::ContentPart::Text { text } => Some(text.as_str()),
                                    _ => None,
                                })
                                .collect::<Vec<_>>()
                                .join(""),
                        };
                        let lower_text = text.to_lowercase();
                        for word in &org_config.guardrails.blocked_words {
                            if lower_text.contains(&word.to_lowercase()) {
                                return Err(GatewayError::Forbidden(format!(
                                    "Blocked word '{}' detected in request",
                                    word
                                )));
                            }
                        }
                    }
                }
            }

            // Check max tokens per request from guardrails
            if let Some(max) = org_config.guardrails.max_tokens_per_request {
                if let Some(requested) = request.max_tokens {
                    if requested > max {
                        return Err(GatewayError::Forbidden(format!(
                            "max_tokens {} exceeds org guardrail limit of {}",
                            requested, max
                        )));
                    }
                }
            }
        }

        Ok(())
    }

    fn check_token_budgets(
        &self,
        auth: Option<&AuthContext>,
        config: &Config,
        request: &ChatCompletionRequest,
    ) -> Result<(), GatewayError> {
        if let Some(auth_ctx) = auth {
            // Check per-org max_tokens_per_request
            if let Some(ref org_id) = auth_ctx.org_id {
                if let Some(org_config) = config.orgs.get(org_id) {
                    if let Some(ref budget) = org_config.token_budget {
                        if let Some(max) = budget.max_tokens_per_request {
                            if let Some(requested) = request.max_tokens {
                                if requested > max {
                                    return Err(GatewayError::Forbidden(format!(
                                        "max_tokens {} exceeds org limit of {}",
                                        requested, max
                                    )));
                                }
                            }
                        }
                    }
                    // Check per-team max_tokens_per_request
                    if let Some(ref team_id) = auth_ctx.team_id {
                        if let Some(team_config) = org_config.teams.get(team_id) {
                            if let Some(ref budget) = team_config.token_budget {
                                if let Some(max) = budget.max_tokens_per_request {
                                    if let Some(requested) = request.max_tokens {
                                        if requested > max {
                                            return Err(GatewayError::Forbidden(format!(
                                                "max_tokens {} exceeds team limit of {}",
                                                requested, max
                                            )));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub fn get_api_key(&self, target: &Target) -> Result<String, GatewayError> {
        if let Some(env_var) = &target.api_key_env {
            std::env::var(env_var).map_err(|_| {
                GatewayError::ServiceUnavailable(format!(
                    "Missing API key environment variable: {}",
                    env_var
                ))
            })
        } else if let Some(key) = self.provider_keys.get(&target.provider) {
            Ok(key.clone())
        } else {
            Ok(String::new())
        }
    }

    fn check_rate_limits(
        &self,
        auth: Option<&AuthContext>,
        config: &Config,
    ) -> Result<(), GatewayError> {
        if !config.rate_limit.enabled {
            return Ok(());
        }

        // Global rate limit
        if !self.rate_limiter.check_global() {
            return Err(GatewayError::RateLimited {
                retry_after_secs: 1,
            });
        }

        if let Some(auth_ctx) = auth {
            // Per-key rate limit (uses override from API key if set)
            if let Some(ref key_id) = auth_ctx.key_id {
                let (rate, burst) = match &auth_ctx.rate_limit_override {
                    Some(override_cfg) => {
                        (override_cfg.requests_per_second, override_cfg.burst_size)
                    }
                    None => (None, None),
                };
                if !self.rate_limiter.check_key(key_id, rate, burst) {
                    return Err(GatewayError::RateLimited {
                        retry_after_secs: 1,
                    });
                }
            }

            // Per-org rate limit
            if let Some(ref org_id) = auth_ctx.org_id {
                if let Some(org_config) = config.orgs.get(org_id) {
                    if let Some(ref org_rate_limit) = org_config.rate_limit {
                        if org_rate_limit.enabled {
                            let rate = org_rate_limit.requests_per_second;
                            let burst = org_rate_limit.burst_size;
                            if !self.rate_limiter.check_org(org_id, Some(rate), Some(burst)) {
                                return Err(GatewayError::RateLimited {
                                    retry_after_secs: 1,
                                });
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    async fn log_audit(&self, audit: &AuditContext<'_>) {
        let messages: Vec<AuditMessage> = audit
            .request
            .messages
            .iter()
            .map(|m| {
                let content = match &m.content {
                    Some(himadri_core::MessageContent::Text(t)) => t.clone(),
                    Some(himadri_core::MessageContent::Parts(parts)) => parts
                        .iter()
                        .filter_map(|p| match p {
                            himadri_core::ContentPart::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join(""),
                    None => String::new(),
                };
                AuditMessage {
                    role: format!("{:?}", m.role).to_lowercase(),
                    content,
                }
            })
            .collect();

        let (status, error, response_text, tokens) = match audit.result {
            Ok(response) => {
                let text = extract_response_text(response);
                let tokens = response
                    .usage
                    .as_ref()
                    .map(|u| (u.prompt_tokens, u.completion_tokens, u.total_tokens));
                (AuditStatus::Success, None, text, tokens)
            }
            Err(e) => (AuditStatus::Error, Some(e.to_string()), None, None),
        };

        let event = AuditEvent {
            request_id: audit.ctx.request_id.clone(),
            timestamp: chrono::Utc::now(),
            org_id: audit.auth.and_then(|a| a.org_id.clone()),
            team_id: audit.auth.and_then(|a| a.team_id.clone()),
            user_id: audit.auth.and_then(|a| a.user_id.clone()),
            key_id: audit.auth.and_then(|a| a.key_id.clone()),
            model: audit.request.model.clone(),
            provider: audit.ctx.provider.clone(),
            messages,
            response: response_text,
            latency_ms: audit.latency_ms,
            tokens_prompt: tokens.map(|t| t.0),
            tokens_completion: tokens.map(|t| t.1),
            tokens_total: tokens.map(|t| t.2),
            status,
            error,
            guardrail_actions: audit.guardrail_actions.to_vec(),
            stream: audit.request.stream,
        };

        self.audit_log.log(event);
    }
}

fn extract_response_text(response: &ChatCompletionResponse) -> Option<String> {
    let mut parts = Vec::new();
    for choice in &response.choices {
        if let Some(ref content) = choice.message.content {
            parts.push(content.as_str());
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(""))
    }
}

fn redact_response_text(response: &mut ChatCompletionResponse, redacted: &str) {
    for choice in &mut response.choices {
        choice.message.content = Some(redacted.to_string());
    }
}

use futures::StreamExt;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Records usage, metrics and a request-log entry for a streamed request,
/// mirroring what `route` records for non-streaming ones. Usage is taken
/// from the last stream chunk that carries it (OpenAI-style final-chunk
/// usage, Anthropic `message_delta`). Recording happens once, at stream end
/// or — via `Drop` — when the client disconnects mid-stream.
struct StreamUsageRecorder {
    metrics: Arc<Metrics>,
    usage_store: Arc<himadri_admin::UsageStore>,
    request_log: Arc<dyn himadri_admin::RequestLogStore>,
    provider: String,
    model: String,
    api_key_id: Option<String>,
    started: std::time::Instant,
    usage: Option<himadri_core::Usage>,
    error: Option<String>,
    recorded: bool,
}

impl StreamUsageRecorder {
    fn observe_chunk(&mut self, chunk: &StreamChunk) {
        if let Some(usage) = &chunk.usage {
            self.usage = Some(usage.clone());
        }
    }

    fn observe_error(&mut self, e: &himadri_provider::ProviderError) {
        self.error = Some(e.to_string());
    }

    fn finish(&mut self) {
        if self.recorded {
            return;
        }
        self.recorded = true;

        let latency_ms = self.started.elapsed().as_millis() as u64;
        let (prompt_tokens, completion_tokens, total_tokens) = self
            .usage
            .as_ref()
            .map(|u| (u.prompt_tokens, u.completion_tokens, u.total_tokens))
            .unwrap_or((0, 0, 0));
        let labels = [self.provider.as_str(), self.model.as_str()];

        self.metrics.requests_total.with_label_values(&labels).inc();
        self.metrics
            .request_duration
            .observe(latency_ms as f64 / 1000.0);
        if self.error.is_none() {
            self.metrics
                .tokens_input_total
                .with_label_values(&labels)
                .inc_by(prompt_tokens as u64);
            self.metrics
                .tokens_output_total
                .with_label_values(&labels)
                .inc_by(completion_tokens as u64);
        } else {
            self.metrics
                .provider_errors
                .with_label_values(&labels)
                .inc();
        }

        let cost = self
            .usage_store
            .calculate_cost(&self.model, prompt_tokens, completion_tokens);
        if cost > 0.0 {
            self.metrics
                .cost_usd_total
                .with_label_values(&labels)
                .inc_by((cost * 1_000_000.0) as u64); // micro-USD, as in `route`
        }

        self.usage_store.record(himadri_admin::UsageRecord {
            request_id: uuid::Uuid::new_v4().to_string(),
            api_key_id: self.api_key_id.clone(),
            model: self.model.clone(),
            provider: self.provider.clone(),
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cost_usd: cost,
            latency_ms,
            created_at: chrono::Utc::now(),
            success: self.error.is_none(),
            error_message: self.error.clone(),
        });

        let _ = self.request_log.write(himadri_admin::RequestLogEntry {
            trace_id: uuid::Uuid::new_v4().to_string(),
            stage: "completed".to_string(),
            model: self.model.clone(),
            provider: self.provider.clone(),
            prompt_tokens,
            completion_tokens,
            total_tokens,
            error_message: self.error.clone(),
            created_at: chrono::Utc::now(),
        });
    }
}

impl Drop for StreamUsageRecorder {
    fn drop(&mut self) {
        self.finish();
    }
}

struct GuardrailStream<S> {
    inner: S,
    buffer: String,
    buffer_limit: usize,
    plugin_manager: Arc<PluginManager>,
    request: ChatCompletionRequest,
    auth: Option<AuthContext>,
    guardrails_ran: bool,
    recorder: StreamUsageRecorder,
}

const DEFAULT_STREAM_BUFFER_LIMIT: usize = 1024 * 1024; // 1MB

impl<S> Stream for GuardrailStream<S>
where
    S: Stream<Item = Result<StreamChunk, himadri_provider::ProviderError>> + Unpin,
{
    type Item = Result<StreamChunk, himadri_provider::ProviderError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.inner.poll_next_unpin(cx) {
            Poll::Ready(Some(Ok(chunk))) => {
                self.recorder.observe_chunk(&chunk);
                // Accumulate response text from chunks, but cap at buffer limit
                if self.buffer.len() < self.buffer_limit {
                    for choice in &chunk.choices {
                        if let Some(ref delta) = choice.delta.content {
                            self.buffer.push_str(delta);
                        }
                    }
                }
                Poll::Ready(Some(Ok(chunk)))
            }
            Poll::Ready(Some(Err(e))) => {
                self.recorder.observe_error(&e);
                Poll::Ready(Some(Err(e)))
            }
            Poll::Ready(None) => {
                self.recorder.finish();
                // Stream ended — run guardrails on accumulated content
                if !self.buffer.is_empty() && !self.guardrails_ran {
                    self.guardrails_ran = true;
                    let pm = self.plugin_manager.clone();
                    let ctx = himadri_plugin::PluginContext::from_request(
                        &self.request,
                        self.auth.as_ref(),
                    );
                    let buffer = self.buffer.clone();

                    // Spawn guardrail check — if rejected, we still need to return None
                    // since the stream already sent partial chunks. Log the rejection.
                    let rt = tokio::runtime::Handle::current();
                    rt.spawn(async move {
                        match pm.run_response_guardrails(&ctx, &buffer).await {
                            Ok(ResponseAction::Reject(reason)) => {
                                warn!("Stream response guardrail rejected: {}", reason);
                            }
                            Ok(ResponseAction::Redact(_)) => {
                                warn!("Stream response guardrail redacted content (partial stream already sent)");
                            }
                            _ => {}
                        }
                    });
                }
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

fn wrap_stream_with_guardrails<S>(
    stream: S,
    plugin_manager: Arc<PluginManager>,
    request: ChatCompletionRequest,
    auth: Option<AuthContext>,
    recorder: StreamUsageRecorder,
) -> GuardrailStream<S>
where
    S: Stream<Item = Result<StreamChunk, himadri_provider::ProviderError>> + Unpin + Send + 'static,
{
    GuardrailStream {
        inner: stream,
        buffer: String::new(),
        buffer_limit: DEFAULT_STREAM_BUFFER_LIMIT,
        plugin_manager,
        request,
        auth,
        guardrails_ran: false,
        recorder,
    }
}

#[cfg(test)]
mod stream_usage_tests {
    use super::*;
    use futures::stream;
    use himadri_observability::Metrics;

    fn chunk(content: &str, usage: Option<himadri_core::Usage>) -> StreamChunk {
        StreamChunk {
            id: "c1".to_string(),
            object: "chat.completion.chunk".to_string(),
            created: 0,
            model: "gpt-4o".to_string(),
            choices: vec![himadri_core::StreamChoice {
                index: 0,
                delta: himadri_core::Delta {
                    role: None,
                    content: Some(content.to_string()),
                    tool_calls: None,
                },
                finish_reason: None,
            }],
            usage,
            system_fingerprint: None,
        }
    }

    fn recorder(
        usage_store: &Arc<himadri_admin::UsageStore>,
        request_log: &Arc<himadri_admin::InMemoryRequestLogStore>,
    ) -> StreamUsageRecorder {
        StreamUsageRecorder {
            metrics: Arc::new(Metrics::new()),
            usage_store: usage_store.clone(),
            request_log: request_log.clone() as Arc<dyn himadri_admin::RequestLogStore>,
            provider: "openai".to_string(),
            model: "gpt-4o".to_string(),
            api_key_id: Some("key-1".to_string()),
            started: std::time::Instant::now(),
            usage: None,
            error: None,
            recorded: false,
        }
    }

    fn request() -> ChatCompletionRequest {
        serde_json::from_value(serde_json::json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true,
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn records_usage_from_final_chunk_at_stream_end() {
        let usage_store = Arc::new(himadri_admin::UsageStore::new());
        let request_log = Arc::new(himadri_admin::InMemoryRequestLogStore::new());

        let chunks: Vec<Result<StreamChunk, himadri_provider::ProviderError>> = vec![
            Ok(chunk("hel", None)),
            Ok(chunk(
                "lo",
                Some(himadri_core::Usage {
                    prompt_tokens: 7,
                    completion_tokens: 5,
                    total_tokens: 12,
                }),
            )),
        ];
        let wrapped = wrap_stream_with_guardrails(
            stream::iter(chunks),
            Arc::new(PluginManager::new()),
            request(),
            None,
            recorder(&usage_store, &request_log),
        );
        let out: Vec<_> = wrapped.collect().await;
        assert_eq!(out.len(), 2);

        let dashboard = usage_store.get_dashboard(0);
        assert_eq!(dashboard.total_requests, 1);
        assert_eq!(dashboard.total_tokens, 12);
        let stats = usage_store.get_key_stats("key-1");
        assert_eq!(stats.total_tokens, 12);
    }

    #[tokio::test]
    async fn records_error_when_client_disconnects_mid_stream() {
        let usage_store = Arc::new(himadri_admin::UsageStore::new());
        let request_log = Arc::new(himadri_admin::InMemoryRequestLogStore::new());

        let chunks: Vec<Result<StreamChunk, himadri_provider::ProviderError>> = vec![
            Ok(chunk("partial", None)),
            Err(himadri_provider::ProviderError::Network(
                "reset".to_string(),
            )),
        ];
        let mut wrapped = Box::pin(wrap_stream_with_guardrails(
            stream::iter(chunks),
            Arc::new(PluginManager::new()),
            request(),
            None,
            recorder(&usage_store, &request_log),
        ));
        // Consume both items, then drop without polling to completion —
        // Drop must still record, marking the request as failed.
        let _ = wrapped.next().await;
        let _ = wrapped.next().await;
        drop(wrapped);

        let dashboard = usage_store.get_dashboard(0);
        assert_eq!(dashboard.total_requests, 1);
        assert!(dashboard.error_rate > 0.0);
    }
}

#[cfg(test)]
mod config_history_tests {
    use super::*;
    use himadri_core::Config;
    use himadri_observability::Metrics;

    fn gateway() -> Gateway {
        Gateway::new(Config::default(), Arc::new(Metrics::new()))
    }

    #[tokio::test]
    async fn history_seeded_with_initial_version() {
        let gw = gateway();
        let history = gw.config_history().await;
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].version, 1);
        assert!(history[0].rolled_back_from.is_none());
    }

    #[tokio::test]
    async fn reload_appends_a_version() {
        let gw = gateway();
        let mut cfg = Config::default();
        cfg.strategy.fallback_timeout_ms = 12345;
        gw.reload_config(cfg).await.unwrap();

        let history = gw.config_history().await;
        assert_eq!(history.len(), 2);
        // Newest first.
        assert_eq!(history[0].version, 2);
        assert_eq!(history[0].config.strategy.fallback_timeout_ms, 12345);
    }

    #[tokio::test]
    async fn rollback_restores_and_records_new_version() {
        let gw = gateway();

        // v2 with a distinctive value.
        let mut cfg = Config::default();
        cfg.strategy.fallback_timeout_ms = 999;
        gw.reload_config(cfg).await.unwrap();
        assert_eq!(gw.get_config().await.strategy.fallback_timeout_ms, 999);

        // Roll back to v1 (default timeout 30000).
        gw.rollback_config(1).await.unwrap();
        assert_eq!(gw.get_config().await.strategy.fallback_timeout_ms, 30000);

        let history = gw.config_history().await;
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].version, 3);
        assert_eq!(history[0].rolled_back_from, Some(1));
    }

    #[tokio::test]
    async fn rollback_unknown_version_errors() {
        let gw = gateway();
        assert!(gw.rollback_config(999).await.is_err());
    }
}
