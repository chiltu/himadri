//! The non-streaming request path: shared pre-flight guards, strategy-based
//! target selection, failover execution, response guardrails, and
//! accounting. Also hosts `embed`, which reuses the same selection/failover
//! machinery.

use std::sync::Arc;

use tracing::{debug, instrument, warn};

use himadri_circuitbreaker::{CircuitBreaker, CircuitBreakerConfig};
use himadri_core::{
    AuthContext, ChatCompletionRequest, ChatCompletionResponse, GatewayError, Target,
};
use himadri_plugin::traits::ResponseAction;
use himadri_provider::traits::Provider;

use crate::strategy::Strategy;

use super::audit::{
    extract_response_text, record_request_outcome, redact_response_text, AuditContext,
    RequestOutcome,
};
use super::{routing_key, target_serves_model, Gateway};

/// Why a failover attempt produced no result. Infrastructure failures are
/// kept distinct from provider errors so each caller can preserve its error
/// surface: `route` flattens everything into `ProviderError` (for audit /
/// usage records), while `route_stream` maps to the richer `GatewayError`
/// variants (`CircuitOpen`, `ProviderNotFound`).
pub(super) enum AttemptError {
    NoTargets,
    CircuitOpen(String),
    ProviderNotFound(String),
    ApiKey(GatewayError),
    Provider(himadri_provider::ProviderError),
}

impl Gateway {
    /// Run the shared request prologue: rate-limit / budget / guardrail /
    /// RBAC checks followed by the before-request plugins. Any new guard
    /// added here applies to both `route` and `route_stream`.
    pub(super) async fn prepare_request(
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
            .map_err(map_plugin_error)?;
        Ok(ctx)
    }

    /// Select targets via the active strategy (in priority order for
    /// failover) and filter them by the caller's RBAC grants.
    pub(super) async fn select_targets(
        &self,
        request: &ChatCompletionRequest,
        auth: Option<&AuthContext>,
    ) -> Result<Vec<Target>, GatewayError> {
        let strategy = self.strategy.read().await;
        let targets = self.targets.read().await;

        // Only consider targets that actually serve the requested model.
        // Without this, the strategy would rank *every* configured provider for
        // *any* model and could route e.g. `openrouter/free` to a provider that
        // doesn't have it — the upstream then rejects it with a confusing error.
        // A target with `models: None` is a wildcard (serves any model), which
        // preserves the behavior of env/config targets that don't enumerate
        // models; DB-configured targets always carry an explicit model list.
        let eligible: Vec<Target> = targets
            .iter()
            .filter(|t| target_serves_model(t, &request.model))
            .cloned()
            .collect();
        drop(targets);

        if eligible.is_empty() {
            drop(strategy);
            return Err(GatewayError::NotFound(format!(
                "No provider is configured to serve model '{}'",
                request.model
            )));
        }

        let ordered = strategy.select_ordered(request, &eligible).await?;
        drop(strategy);
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
    pub(super) async fn with_failover<T, F, Fut>(
        &self,
        ordered: &[Target],
        record_latency: bool,
        mut op: F,
    ) -> Option<(Target, Result<T, AttemptError>, std::time::Duration)>
    where
        F: FnMut(Arc<dyn Provider>, String) -> Fut,
        Fut: std::future::Future<Output = Result<T, himadri_provider::ProviderError>>,
    {
        // Callers guarantee a non-empty list today, but keep this function
        // total: indexing an empty slice here would panic a worker.
        let mut last_target = ordered.first()?.clone();
        let mut last_result = Err(AttemptError::NoTargets);
        let mut last_latency = std::time::Duration::ZERO;

        // Resolve the latency store once instead of re-locking the strategy
        // on every attempt.
        let latency_store = if record_latency {
            match &*self.strategy.read().await {
                Strategy::LeastLatency(state) => Some(state.store.clone()),
                _ => None,
            }
        } else {
            None
        };

        let last_idx = ordered.len() - 1;
        for (idx, candidate) in ordered.iter().enumerate() {
            let is_last = idx == last_idx;
            last_target = candidate.clone();

            // Circuit breaker for this endpoint (clone the Arc so we hold no
            // DashMap reference across await points). Keyed by routing key so
            // one endpoint's failures don't trip a sibling of the same type.
            let cb = self
                .circuit_breakers
                .entry(routing_key(candidate).to_string())
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
            let provider = match self.providers.get(routing_key(candidate)) {
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
                // A 429 means "slow down", not "the provider is down" —
                // opening the circuit on it would turn a soft limit into a
                // full outage for that provider. It still triggers failover.
                Err(himadri_provider::ProviderError::RateLimited { .. }) => {}
                Err(e) if e.retryable() => cb.record_failure().await,
                Err(_) => {}
            }
            if let Some(store) = &latency_store {
                store
                    .record(&candidate.provider, last_latency.as_millis() as u64)
                    .await;
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

        Some((last_target, last_result, last_latency))
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
            let provider = match self.providers.get(routing_key(target)) {
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
        let mut ctx = self.prepare_request(&request, auth, remote_ip).await?;

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
            .await
            .ok_or_else(|| GatewayError::Internal("No targets configured".to_string()))?;
        let latency_ms = latency.as_millis() as u64;

        // Flatten infrastructure failures into ProviderError, preserving the
        // messages for the audit log. Unavailability-shaped failures (open
        // circuit, missing/unresolvable provider or key) flatten to a 503-
        // status Api error so the client sees "unavailable", not a 500.
        let result = result.map_err(|e| match e {
            AttemptError::Provider(e) => e,
            AttemptError::CircuitOpen(p) => himadri_provider::ProviderError::Api {
                status: 503,
                message: format!("Circuit breaker open for provider {}", p),
            },
            AttemptError::ProviderNotFound(p) => himadri_provider::ProviderError::Api {
                status: 503,
                message: format!("Provider not found: {}", p),
            },
            AttemptError::ApiKey(e) => himadri_provider::ProviderError::Api {
                status: 503,
                message: e.to_string(),
            },
            AttemptError::NoTargets => {
                himadri_provider::ProviderError::Internal("No targets attempted".to_string())
            }
        });

        // Run after-request plugins against the same context the
        // before-request plugins saw: request_id stays stable across the
        // request lifecycle and before-plugin metadata is visible here.
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
        self.plugin_manager.run_after(&mut ctx).await;

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

        // Metrics + usage + request log, shared with the streaming path.
        record_request_outcome(&RequestOutcome {
            metrics: &self.metrics,
            usage_store: &self.usage_store,
            request_log: self.request_log.as_ref(),
            provider: &target.provider,
            model: &request.model,
            api_key_id: auth.and_then(|a| a.key_id.as_deref()),
            usage: result.as_ref().ok().and_then(|r| r.usage.clone()),
            error: result.as_ref().err().map(|e| e.to_string()),
            latency_ms,
        });

        // Populate the response cache on a successful, non-streaming completion.
        if !request.stream {
            if let (Some(cache), Ok(response)) = (&self.response_cache, &result) {
                cache.insert(&request, response.clone()).await;
            }
        }

        // Structured mapping preserves upstream semantics (429 stays 429,
        // 4xx stays a client error) instead of flattening everything to 500.
        result.map_err(GatewayError::from)
    }
}

/// Map a plugin rejection to the gateway error (and thus HTTP status) that
/// matches its intent: rate limits surface as 429 so clients back off,
/// budget exhaustion as 429 quota, RBAC-style rejections as 403 — not the
/// generic 400 everything used to collapse into.
fn map_plugin_error(e: himadri_plugin::PluginError) -> GatewayError {
    use himadri_plugin::RejectKind;
    match &e {
        himadri_plugin::PluginError::Rejected { kind, reason, .. } => match kind {
            RejectKind::RateLimited { retry_after_secs } => GatewayError::RateLimited {
                retry_after_secs: *retry_after_secs,
            },
            RejectKind::BudgetExceeded => GatewayError::QuotaExceeded(reason.clone()),
            RejectKind::Forbidden => GatewayError::Forbidden(reason.clone()),
            RejectKind::BadRequest => GatewayError::BadRequest(e.to_string()),
        },
        himadri_plugin::PluginError::Internal(_) => GatewayError::Internal(e.to_string()),
    }
}
