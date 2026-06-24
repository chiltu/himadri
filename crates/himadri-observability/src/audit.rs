use chrono::{DateTime, Utc};
use serde::Serialize;
use std::path::PathBuf;
use tokio::sync::mpsc;
use tracing::warn;

use super::redact::Redactor;

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
}

pub struct AuditLog {
    sender: mpsc::UnboundedSender<AuditEvent>,
    redactor: Option<Redactor>,
}

impl AuditLog {
    pub fn new(log_dir: Option<PathBuf>, redact_pii: bool) -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();

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

        Self { sender, redactor }
    }

    pub fn log(&self, mut event: AuditEvent) {
        if let Some(ref redactor) = self.redactor {
            for msg in &mut event.messages {
                msg.content = redactor.redact(&msg.content);
            }
            if let Some(ref mut response) = event.response {
                *response = redactor.redact(response);
            }
        }
        let _ = self.sender.send(event);
    }

    async fn write_loop(dir: PathBuf, mut receiver: mpsc::UnboundedReceiver<AuditEvent>) {
        while let Some(event) = receiver.recv().await {
            let _filename = format!(
                "{}-{}.jsonl",
                event.timestamp.format("%Y-%m-%d"),
                dir.display()
            );
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

    async fn tracing_loop(mut receiver: mpsc::UnboundedReceiver<AuditEvent>) {
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
