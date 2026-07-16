//! Audit events and per-request accounting (metrics, usage, request log).
//! The [`RequestAuditor`] trait provides a single seam that both the
//! non-streaming and streaming paths call, replacing the previous pattern
//! of calling several free functions / `impl Gateway` methods inline.

use std::sync::Arc;

use himadri_admin::{RequestLogEntry, RequestLogStore, UsageRecord, UsageStore};
use himadri_core::{AuthContext, ChatCompletionRequest, ChatCompletionResponse, Usage};
use himadri_observability::{AuditEvent, AuditLog, AuditMessage, AuditStatus, Metrics};

/// Full outcome of one request, used by the non-streaming path.
pub(super) struct RequestEvent {
    pub(super) auth: Option<AuthContext>,
    pub(super) request_id: String,
    pub(super) provider: String,
    pub(super) model: String,
    pub(super) messages: Vec<AuditMessage>,
    pub(super) response_text: Option<String>,
    pub(super) latency_ms: u64,
    pub(super) tokens_prompt: u32,
    pub(super) tokens_completion: u32,
    pub(super) tokens_total: u32,
    pub(super) status: AuditStatus,
    pub(super) error: Option<String>,
    pub(super) guardrail_actions: Vec<String>,
    pub(super) stream: bool,
    pub(super) api_key_id: Option<String>,
}

/// Accounting-only data, used by the streaming path's `Drop`-based recorder
/// (which fires at stream end and cannot hold audit-event data).
pub(super) struct AccountOutcome {
    pub(super) provider: String,
    pub(super) model: String,
    pub(super) api_key_id: Option<String>,
    pub(super) usage: Option<Usage>,
    pub(super) error: Option<String>,
    pub(super) latency_ms: u64,
}

/// The audit + accounting seam. Both the non-streaming path (`route`) and
/// the streaming path (`StreamUsageRecorder`) call through this trait
/// instead of importing free functions or accessing `Gateway` fields.
pub(super) trait RequestAuditor: Send + Sync {
    /// Full audit event + metrics + usage + request log. Called once per
    /// non-streaming request after the response is received or the error
    /// is finalised.
    fn record_full(&self, event: RequestEvent);

    /// Accounting-only (metrics + usage + request log), no audit event.
    /// Called from the streaming path's `Drop`-based recorder which has
    /// no access to request/auth/messages data at fire time.
    fn record_accounting(&self, outcome: AccountOutcome);
}

/// Production auditor that writes to all four stores.
pub(super) struct LiveRequestAuditor {
    pub(super) audit_log: Arc<AuditLog>,
    pub(super) metrics: Arc<Metrics>,
    pub(super) usage_store: Arc<UsageStore>,
    pub(super) request_log: Arc<dyn RequestLogStore>,
}

fn record_usage(
    usage_store: &UsageStore,
    metrics: &Metrics,
    request_log: &dyn RequestLogStore,
    provider: &str,
    model: &str,
    api_key_id: Option<&str>,
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
    latency_ms: u64,
    error: Option<&str>,
) {
    let labels = [provider, model];

    metrics.requests_total.with_label_values(&labels).inc();
    metrics
        .request_duration
        .observe(latency_ms as f64 / 1000.0);

    if error.is_none() {
        metrics
            .tokens_input_total
            .with_label_values(&labels)
            .inc_by(prompt_tokens as u64);
        metrics
            .tokens_output_total
            .with_label_values(&labels)
            .inc_by(completion_tokens as u64);

        let cost = usage_store.calculate_cost(model, prompt_tokens, completion_tokens);
        if cost > 0.0 {
            metrics
                .cost_usd_total
                .with_label_values(&labels)
                .inc_by((cost * 1_000_000.0) as u64);
        }

        usage_store.record(UsageRecord {
            request_id: uuid::Uuid::new_v4().to_string(),
            api_key_id: api_key_id.map(str::to_string),
            model: model.to_string(),
            provider: provider.to_string(),
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cost_usd: cost,
            latency_ms,
            created_at: chrono::Utc::now(),
            success: true,
            error_message: None,
        });
    } else {
        metrics
            .provider_errors
            .with_label_values(&labels)
            .inc();

        usage_store.record(UsageRecord {
            request_id: uuid::Uuid::new_v4().to_string(),
            api_key_id: api_key_id.map(str::to_string),
            model: model.to_string(),
            provider: provider.to_string(),
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cost_usd: 0.0,
            latency_ms,
            created_at: chrono::Utc::now(),
            success: false,
            error_message: error.map(str::to_string),
        });
    }

    let _ = request_log.write(RequestLogEntry {
        trace_id: uuid::Uuid::new_v4().to_string(),
        stage: "completed".to_string(),
        model: model.to_string(),
        provider: provider.to_string(),
        prompt_tokens,
        completion_tokens,
        total_tokens,
        error_message: error.map(str::to_string),
        created_at: chrono::Utc::now(),
    });
}

impl RequestAuditor for LiveRequestAuditor {
    fn record_full(&self, event: RequestEvent) {
        // ── Audit log ──────────────────────────────────────────────
        let audit_event = AuditEvent {
            request_id: event.request_id,
            timestamp: chrono::Utc::now(),
            org_id: event.auth.as_ref().and_then(|a| a.org_id.clone()),
            team_id: event.auth.as_ref().and_then(|a| a.team_id.clone()),
            user_id: event.auth.as_ref().and_then(|a| a.user_id.clone()),
            key_id: event.api_key_id.clone(),
            model: event.model.clone(),
            provider: Some(event.provider.clone()),
            messages: event.messages,
            response: event.response_text,
            latency_ms: event.latency_ms,
            tokens_prompt: Some(event.tokens_prompt),
            tokens_completion: Some(event.tokens_completion),
            tokens_total: Some(event.tokens_total),
            status: event.status,
            error: event.error.clone(),
            guardrail_actions: event.guardrail_actions,
            stream: event.stream,
        };
        self.audit_log.log(audit_event);

        // ── Metrics + usage + request log ──────────────────────────
        record_usage(
            &self.usage_store,
            &self.metrics,
            self.request_log.as_ref(),
            &event.provider,
            &event.model,
            event.api_key_id.as_deref(),
            event.tokens_prompt,
            event.tokens_completion,
            event.tokens_total,
            event.latency_ms,
            event.error.as_deref(),
        );
    }

    fn record_accounting(&self, outcome: AccountOutcome) {
        let (prompt, completion, total) = outcome
            .usage
            .as_ref()
            .map(|u| (u.prompt_tokens, u.completion_tokens, u.total_tokens))
            .unwrap_or((0, 0, 0));

        record_usage(
            &self.usage_store,
            &self.metrics,
            self.request_log.as_ref(),
            &outcome.provider,
            &outcome.model,
            outcome.api_key_id.as_deref(),
            prompt,
            completion,
            total,
            outcome.latency_ms,
            outcome.error.as_deref(),
        );
    }
}

// ── Pure helpers (no Gateway / store access) ────────────────────────

/// Flatten a request's messages into audit form.
pub(super) fn audit_messages(request: &ChatCompletionRequest) -> Vec<AuditMessage> {
    request
        .messages
        .iter()
        .map(|m| AuditMessage {
            role: format!("{:?}", m.role).to_lowercase(),
            content: m
                .content
                .as_ref()
                .map(|c| c.flat_text().into_owned())
                .unwrap_or_default(),
        })
        .collect()
}

pub(super) fn extract_response_text(response: &ChatCompletionResponse) -> Option<String> {
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

pub(super) fn redact_response_text(response: &mut ChatCompletionResponse, redacted: &str) {
    for choice in &mut response.choices {
        choice.message.content = Some(redacted.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use himadri_admin::InMemoryRequestLogStore;
    use himadri_core::Usage;
    use himadri_observability::Metrics;

    fn auditor() -> LiveRequestAuditor {
        LiveRequestAuditor {
            audit_log: Arc::new(AuditLog::new(None, false)),
            metrics: Arc::new(Metrics::new()),
            usage_store: Arc::new(UsageStore::new()),
            request_log: Arc::new(InMemoryRequestLogStore::new()),
        }
    }

    #[tokio::test]
    async fn record_accounting_success() {
        let a = auditor();
        a.record_accounting(AccountOutcome {
            provider: "openai".to_string(),
            model: "gpt-4o".to_string(),
            api_key_id: Some("key-1".to_string()),
            usage: Some(Usage {
                prompt_tokens: 10,
                completion_tokens: 20,
                total_tokens: 30,
            }),
            error: None,
            latency_ms: 100,
        });
        let stats = a.usage_store.get_key_stats("key-1");
        assert_eq!(stats.total_tokens, 30);
        assert_eq!(stats.total_requests, 1);
    }

    #[tokio::test]
    async fn record_accounting_error() {
        let a = auditor();
        a.record_accounting(AccountOutcome {
            provider: "openai".to_string(),
            model: "gpt-4o".to_string(),
            api_key_id: None,
            usage: None,
            error: Some("timeout".to_string()),
            latency_ms: 5000,
        });
        let dashboard = a.usage_store.get_dashboard(0);
        assert_eq!(dashboard.total_requests, 1);
        assert!(dashboard.error_rate > 0.0);
    }

    #[tokio::test]
    async fn record_full_includes_audit_event() {
        let a = auditor();
        a.record_full(RequestEvent {
            auth: None,
            request_id: "req-1".to_string(),
            provider: "openai".to_string(),
            model: "gpt-4o".to_string(),
            messages: vec![],
            response_text: None,
            latency_ms: 50,
            tokens_prompt: 5,
            tokens_completion: 10,
            tokens_total: 15,
            status: AuditStatus::Success,
            error: None,
            guardrail_actions: vec![],
            stream: false,
            api_key_id: Some("key-1".to_string()),
        });
        let stats = a.usage_store.get_key_stats("key-1");
        assert_eq!(stats.total_tokens, 15);
    }
}
