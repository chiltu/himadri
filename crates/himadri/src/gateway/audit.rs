//! Audit events and per-request accounting (metrics, usage, request log).
//! `record_request_outcome` is shared by the non-streaming path and the
//! streaming recorder so their semantics can never drift.

use himadri_core::{AuthContext, ChatCompletionRequest, ChatCompletionResponse};
use himadri_observability::{AuditEvent, AuditMessage, AuditStatus, Metrics};

use super::Gateway;

pub(super) struct AuditContext<'a> {
    pub(super) request: &'a ChatCompletionRequest,
    pub(super) auth: Option<&'a AuthContext>,
    pub(super) ctx: &'a himadri_plugin::PluginContext,
    pub(super) result: &'a Result<ChatCompletionResponse, himadri_provider::ProviderError>,
    pub(super) latency_ms: u64,
    pub(super) guardrail_actions: &'a [String],
}

impl Gateway {
    pub(super) async fn log_audit(&self, audit: &AuditContext<'_>) {
        let messages = audit_messages(audit.request);

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

/// One request's final accounting, shared by the non-streaming path
/// (`route`) and the streaming recorder so metrics/usage/request-log
/// semantics can never drift between the two.
pub(super) struct RequestOutcome<'a> {
    pub(super) metrics: &'a Metrics,
    pub(super) usage_store: &'a himadri_admin::UsageStore,
    pub(super) request_log: &'a dyn himadri_admin::RequestLogStore,
    pub(super) provider: &'a str,
    pub(super) model: &'a str,
    pub(super) api_key_id: Option<&'a str>,
    pub(super) usage: Option<himadri_core::Usage>,
    pub(super) error: Option<String>,
    pub(super) latency_ms: u64,
}

pub(super) fn record_request_outcome(outcome: &RequestOutcome<'_>) {
    let labels = [outcome.provider, outcome.model];
    let (prompt_tokens, completion_tokens, total_tokens) = outcome
        .usage
        .as_ref()
        .map(|u| (u.prompt_tokens, u.completion_tokens, u.total_tokens))
        .unwrap_or((0, 0, 0));

    outcome
        .metrics
        .requests_total
        .with_label_values(&labels)
        .inc();
    outcome
        .metrics
        .request_duration
        .observe(outcome.latency_ms as f64 / 1000.0);
    if outcome.error.is_none() {
        outcome
            .metrics
            .tokens_input_total
            .with_label_values(&labels)
            .inc_by(prompt_tokens as u64);
        outcome
            .metrics
            .tokens_output_total
            .with_label_values(&labels)
            .inc_by(completion_tokens as u64);
    } else {
        outcome
            .metrics
            .provider_errors
            .with_label_values(&labels)
            .inc();
    }

    let cost = outcome
        .usage_store
        .calculate_cost(outcome.model, prompt_tokens, completion_tokens);
    if cost > 0.0 {
        outcome
            .metrics
            .cost_usd_total
            .with_label_values(&labels)
            .inc_by((cost * 1_000_000.0) as u64); // micro-USD for precision
    }

    outcome.usage_store.record(himadri_admin::UsageRecord {
        request_id: uuid::Uuid::new_v4().to_string(),
        api_key_id: outcome.api_key_id.map(str::to_string),
        model: outcome.model.to_string(),
        provider: outcome.provider.to_string(),
        prompt_tokens,
        completion_tokens,
        total_tokens,
        cost_usd: cost,
        latency_ms: outcome.latency_ms,
        created_at: chrono::Utc::now(),
        success: outcome.error.is_none(),
        error_message: outcome.error.clone(),
    });

    let _ = outcome.request_log.write(himadri_admin::RequestLogEntry {
        trace_id: uuid::Uuid::new_v4().to_string(),
        stage: "completed".to_string(),
        model: outcome.model.to_string(),
        provider: outcome.provider.to_string(),
        prompt_tokens,
        completion_tokens,
        total_tokens,
        error_message: outcome.error.clone(),
        created_at: chrono::Utc::now(),
    });
}
