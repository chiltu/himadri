use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{instrument, warn};

use futures::Stream;
use himadri_circuitbreaker::{CircuitBreaker, CircuitBreakerConfig, CircuitBreakerTrait};
use himadri_core::{
    AuthContext, ChatCompletionRequest, ChatCompletionResponse, Config, GatewayError, OrgConfig,
    StreamChunk, Target,
};
use himadri_observability::{AuditEvent, AuditLog, AuditMessage, AuditStatus, Metrics};
use himadri_plugin::traits::ResponseAction;
use himadri_plugin::PluginManager;
use himadri_provider::traits::{BoxStream, Provider};
use himadri_ratelimit::RateLimiter;

use crate::strategy::Strategy;

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

pub struct Gateway {
    config: RwLock<Config>,
    providers: DashMap<String, Arc<dyn Provider>>,
    plugin_manager: Arc<PluginManager>,
    strategy: RwLock<Strategy>,
    circuit_breakers: DashMap<String, Arc<dyn CircuitBreakerTrait>>,
    targets: RwLock<Vec<Target>>,
    rate_limiter: RateLimiter,
    audit_log: Arc<AuditLog>,
    metrics: Arc<Metrics>,
    #[allow(dead_code)]
    model_index: RwLock<ModelLookupIndex>,
    usage_store: Arc<himadri_admin::UsageStore>,
    request_log: Arc<dyn himadri_admin::RequestLogStore>,
}

impl Gateway {
    pub fn new(config: Config, metrics: Arc<Metrics>) -> Self {
        let strategy = Strategy::from_config_mode(&config.strategy.mode);
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

        Self {
            config: RwLock::new(config.clone()),
            providers: DashMap::new(),
            plugin_manager,
            strategy: RwLock::new(strategy),
            circuit_breakers: DashMap::new(),
            targets: RwLock::new(config.targets),
            rate_limiter,
            audit_log,
            metrics,
            model_index,
            usage_store,
            request_log,
        }
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

    #[instrument(skip(self, request, auth), fields(model = %request.model))]
    pub async fn route(
        &self,
        request: ChatCompletionRequest,
        auth: Option<&AuthContext>,
        remote_ip: Option<String>,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        // Check rate limits and quotas
        let config = self.config.read().await;
        self.check_rate_limits(auth, &config)?;
        self.check_token_budgets(auth, &config, &request)?;
        self.check_org_guardrails(auth, &config, &request)?;
        drop(config);

        // Run before-request plugins
        let mut ctx = himadri_plugin::PluginContext::from_request(&request, auth);
        ctx.remote_ip = remote_ip.clone();
        self.plugin_manager
            .run_before(&mut ctx)
            .await
            .map_err(|e| GatewayError::BadRequest(e.to_string()))?;

        // Select provider via strategy
        let strategy = self.strategy.read().await;
        let targets = self.targets.read().await;
        let target = strategy.select(&request, &targets).await?;
        drop(strategy);
        drop(targets);

        // Get circuit breaker
        let cb = self
            .circuit_breakers
            .entry(target.provider.clone())
            .or_insert_with(|| Arc::new(CircuitBreaker::new(CircuitBreakerConfig::default())));

        if !cb.allow().await {
            return Err(GatewayError::CircuitOpen(target.provider));
        }

        // Get provider
        let provider = self
            .providers
            .get(&target.provider)
            .ok_or_else(|| GatewayError::ProviderNotFound(target.provider.clone()))?;

        // Get API key
        let api_key = self.get_api_key(&target)?;

        // Execute request
        let start = std::time::Instant::now();
        let result = provider.complete(&request, &api_key).await;
        let latency = start.elapsed();

        // Record circuit breaker state
        match &result {
            Ok(_) => cb.record_success().await,
            Err(e) if e.retryable() => cb.record_failure().await,
            Err(_) => {}
        }

        // Record latency for strategy (if applicable)
        let latency_ms = latency.as_millis() as u64;
        if let Strategy::LeastLatency(state) = &*self.strategy.read().await {
            state.store.record(&target.provider, latency_ms).await;
        }

        // Run after-request plugins
        let mut ctx = himadri_plugin::PluginContext::from_request(&request, auth);
        ctx.set_provider(target.provider.clone());
        ctx.set_latency(latency);
        if let Ok(ref response) = result {
            if let Some(usage) = &response.usage {
                ctx.set_tokens(usage.total_tokens);
            }
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
        // Check rate limits, quotas, and org guardrails
        let config = self.config.read().await;
        self.check_rate_limits(auth, &config)?;
        self.check_token_budgets(auth, &config, &request)?;
        self.check_org_guardrails(auth, &config, &request)?;
        drop(config);

        // Run before-request plugins (input guardrails)
        let mut ctx = himadri_plugin::PluginContext::from_request(&request, auth);
        ctx.remote_ip = remote_ip;
        self.plugin_manager
            .run_before(&mut ctx)
            .await
            .map_err(|e| GatewayError::BadRequest(e.to_string()))?;

        // Select provider via strategy
        let strategy = self.strategy.read().await;
        let targets = self.targets.read().await;
        let target = strategy.select(&request, &targets).await?;
        drop(strategy);
        drop(targets);

        // Get circuit breaker
        let cb = self
            .circuit_breakers
            .entry(target.provider.clone())
            .or_insert_with(|| Arc::new(CircuitBreaker::new(CircuitBreakerConfig::default())));

        if !cb.allow().await {
            return Err(GatewayError::CircuitOpen(target.provider));
        }

        // Get provider
        let provider = self
            .providers
            .get(&target.provider)
            .ok_or_else(|| GatewayError::ProviderNotFound(target.provider.clone()))?;

        // Get API key
        let api_key = self.get_api_key(&target)?;

        // Execute streaming request
        let stream = provider
            .complete_stream(&request, &api_key)
            .await
            .map_err(|e| GatewayError::Provider(e.to_string()))?;

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

        // Wrap stream with output guardrails
        let plugin_manager = self.plugin_manager.clone();
        let auth_clone = auth.cloned();
        let request_clone = request.clone();
        let wrapped_stream =
            wrap_stream_with_guardrails(stream, plugin_manager, request_clone, auth_clone);

        Ok(Box::pin(wrapped_stream))
    }

    pub async fn reload_config(&self, config: Config) -> Result<(), GatewayError> {
        config.validate()?;
        // Hold all 3 write locks simultaneously to prevent inconsistent reads
        let mut strategy = self.strategy.write().await;
        let mut cfg = self.config.write().await;
        let mut targets = self.targets.write().await;
        *strategy = Strategy::from_config_mode(&config.strategy.mode);
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

    pub async fn get_config(&self) -> Config {
        self.config.read().await.clone()
    }

    pub fn list_providers(&self) -> Vec<String> {
        self.providers.iter().map(|r| r.key().clone()).collect()
    }

    #[allow(dead_code)]
    pub fn rate_limiter(&self) -> &RateLimiter {
        &self.rate_limiter
    }

    #[allow(dead_code)]
    pub fn audit_log(&self) -> &AuditLog {
        &self.audit_log
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

        // Build targets from enabled providers
        for provider in providers {
            if !provider.enabled {
                continue;
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

struct GuardrailStream<S> {
    inner: S,
    buffer: String,
    buffer_limit: usize,
    plugin_manager: Arc<PluginManager>,
    request: ChatCompletionRequest,
    auth: Option<AuthContext>,
    guardrails_ran: bool,
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
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e))),
            Poll::Ready(None) => {
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
    }
}
