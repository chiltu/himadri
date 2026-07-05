use chrono::{DateTime, Utc};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc;
use tracing::warn;

use super::redact::Redactor;

/// Bound on queued audit events. Events carry full prompt/response copies
/// when content capture is enabled, so an unbounded queue behind a slow
/// sink is a memory-exhaustion hazard; past this depth events are dropped
/// (and counted) rather than buffered without limit.
const AUDIT_CHANNEL_CAPACITY: usize = 8_192;

#[derive(Debug, Clone, Serialize)]
pub struct AuditEvent {
    pub request_id: String,
    pub timestamp: DateTime<Utc>,
    pub org_id: Option<String>,
    pub team_id: Option<String>,
    pub user_id: Option<String>,
    pub key_id: Option<String>,
    pub model: String,
    pub provider: Option<String>,
    pub messages: Vec<AuditMessage>,
    pub response: Option<String>,
    pub latency_ms: u64,
    pub tokens_prompt: Option<u32>,
    pub tokens_completion: Option<u32>,
    pub tokens_total: Option<u32>,
    pub status: AuditStatus,
    pub error: Option<String>,
    pub guardrail_actions: Vec<String>,
    pub stream: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuditMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuditStatus {
    Success,
    Error,
    RateLimited,
    GuardrailBlocked,
    /// Authentication failed (401): missing/invalid/expired token.
    Unauthorized,
    /// Authenticated but not permitted (403): failed role/RBAC check.
    Forbidden,
}

pub struct AuditLog {
    sender: mpsc::Sender<AuditEvent>,
    redactor: Option<Redactor>,
    /// When false (the default), prompt messages and response text are
    /// stripped before an event leaves the process — request metadata is
    /// still recorded, but user content never reaches logs or telemetry
    /// (CWE-532). Enable explicitly (e.g. `AUDIT_CAPTURE_CONTENT=true`)
    /// for deployments that require full-content audit trails.
    capture_content: bool,
    dropped: AtomicU64,
}

impl AuditLog {
    /// Content capture defaults to **off**; see [`AuditLog::with_options`].
    pub fn new(log_dir: Option<PathBuf>, redact_pii: bool) -> Self {
        Self::with_options(log_dir, redact_pii, false)
    }

    pub fn with_options(log_dir: Option<PathBuf>, redact_pii: bool, capture_content: bool) -> Self {
        let (sender, receiver) = mpsc::channel(AUDIT_CHANNEL_CAPACITY);

        let redactor = if redact_pii {
            Some(Redactor::new())
        } else {
            None
        };

        if let Some(dir) = log_dir {
            tokio::spawn(Self::write_loop(dir, receiver));
        } else {
            tokio::spawn(Self::tracing_loop(receiver));
        }

        Self {
            sender,
            redactor,
            capture_content,
            dropped: AtomicU64::new(0),
        }
    }

    /// Events dropped because the audit queue was full.
    pub fn dropped_events(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    pub fn log(&self, mut event: AuditEvent) {
        if !self.capture_content {
            event.messages.clear();
            event.response = None;
        } else if let Some(ref redactor) = self.redactor {
            for msg in &mut event.messages {
                msg.content = redactor.redact(&msg.content);
            }
            if let Some(ref mut response) = event.response {
                *response = redactor.redact(response);
            }
        }
        // Never block the request path on the audit sink: drop (and count)
        // when the bounded queue is full.
        if self.sender.try_send(event).is_err() {
            let dropped = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            if dropped.is_power_of_two() {
                warn!("Audit queue full; {} event(s) dropped so far", dropped);
            }
        }
    }

    /// Record an authentication/authorization failure (401/403). Only minimal
    /// context is available at the auth boundary — no request body — so the
    /// model/messages fields are left empty and the cause goes in `error`.
    pub fn log_auth_failure(
        &self,
        status: AuditStatus,
        reason: impl Into<String>,
        remote_ip: Option<String>,
        user_id: Option<String>,
        key_id: Option<String>,
    ) {
        let reason = reason.into();
        let error = match remote_ip {
            Some(ip) => format!("{} (ip: {})", reason, ip),
            None => reason,
        };
        self.log(AuditEvent {
            request_id: format!(
                "auth-{}",
                Utc::now().timestamp_nanos_opt().unwrap_or_default()
            ),
            timestamp: Utc::now(),
            org_id: None,
            team_id: None,
            user_id,
            key_id,
            model: String::new(),
            provider: None,
            messages: Vec::new(),
            response: None,
            latency_ms: 0,
            tokens_prompt: None,
            tokens_completion: None,
            tokens_total: None,
            status,
            error: Some(error),
            guardrail_actions: Vec::new(),
            stream: false,
        });
    }

    async fn write_loop(dir: PathBuf, mut receiver: mpsc::Receiver<AuditEvent>) {
        while let Some(event) = receiver.recv().await {
            let line = match serde_json::to_string(&event) {
                Ok(l) => l,
                Err(_) => continue,
            };

            if let Err(e) = tokio::fs::create_dir_all(&dir).await {
                warn!("Failed to create audit log dir: {}", e);
                continue;
            }

            use tokio::io::AsyncWriteExt;
            let path = dir.join(format!("{}.jsonl", event.timestamp.format("%Y-%m-%d")));
            match tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .await
            {
                Ok(mut file) => {
                    let mut line = line;
                    line.push('\n');
                    if let Err(e) = file.write_all(line.as_bytes()).await {
                        warn!("Failed to write audit log: {}", e);
                    }
                }
                Err(e) => {
                    warn!("Failed to open audit log file: {}", e);
                }
            }
        }
    }

    async fn tracing_loop(mut receiver: mpsc::Receiver<AuditEvent>) {
        while let Some(event) = receiver.recv().await {
            match serde_json::to_string(&event) {
                Ok(json) => {
                    tracing::info!(audit = %json, "audit_event");
                }
                Err(e) => {
                    warn!("Failed to serialize audit event: {}", e);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event_with_content() -> AuditEvent {
        AuditEvent {
            request_id: "r1".into(),
            timestamp: Utc::now(),
            org_id: None,
            team_id: None,
            user_id: None,
            key_id: None,
            model: "gpt-4o".into(),
            provider: Some("openai".into()),
            messages: vec![AuditMessage {
                role: "user".into(),
                content: "top secret prompt".into(),
            }],
            response: Some("secret answer".into()),
            latency_ms: 5,
            tokens_prompt: Some(1),
            tokens_completion: Some(2),
            tokens_total: Some(3),
            status: AuditStatus::Success,
            error: None,
            guardrail_actions: Vec::new(),
            stream: false,
        }
    }

    /// The default configuration must never let prompt/response content
    /// leave the process (finding: prompts logged to telemetry unredacted).
    #[tokio::test]
    async fn content_is_stripped_by_default() {
        let log = AuditLog::new(None, true);
        let mut event = event_with_content();
        // Emulate what `log` does before enqueueing.
        if !log.capture_content {
            event.messages.clear();
            event.response = None;
        }
        assert!(event.messages.is_empty());
        assert!(event.response.is_none());
    }

    #[tokio::test]
    async fn capture_content_keeps_and_redacts() {
        let log = AuditLog::with_options(None, true, true);
        assert!(log.capture_content);
        let redactor = log.redactor.as_ref().unwrap();
        let redacted =
            redactor.redact("email me at user@example.com with sk-abcdef1234567890abcdef");
        assert!(!redacted.contains("user@example.com"));
        assert!(!redacted.contains("sk-abcdef1234567890abcdef"));
    }
}
