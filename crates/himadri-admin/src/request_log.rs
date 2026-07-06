use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Request log entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestLogEntry {
    pub trace_id: String,
    pub stage: String,
    pub model: String,
    pub provider: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Query for listing request logs
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct RequestLogQuery {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub stage: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub since: Option<DateTime<Utc>>,
}

/// Maintenance query for deleting logs
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct MaintenanceQuery {
    pub before: Option<DateTime<Utc>>,
    pub stage: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
}

/// List result with pagination
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestLogListResult {
    pub data: Vec<RequestLogEntry>,
    pub total: usize,
}

/// Request log store trait
pub trait RequestLogStore: Send + Sync {
    fn write(&self, entry: RequestLogEntry) -> Result<(), String>;
    fn list(&self, query: RequestLogQuery) -> Result<RequestLogListResult, String>;
    fn delete(&self, query: MaintenanceQuery) -> Result<usize, String>;
    fn count(&self) -> usize;
}

/// Upper bound on retained in-memory request-log entries. Past this, the
/// oldest entries are evicted so a long-running gateway can't leak memory
/// linearly with traffic. (Durable retention is the Postgres store's job.)
const DEFAULT_MAX_ENTRIES: usize = 100_000;

/// In-memory request log store. Bounded ring: eviction is oldest-first.
pub struct InMemoryRequestLogStore {
    entries: RwLock<VecDeque<RequestLogEntry>>,
    max_entries: usize,
}

impl InMemoryRequestLogStore {
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(VecDeque::new()),
            max_entries: DEFAULT_MAX_ENTRIES,
        }
    }
}

impl RequestLogStore for InMemoryRequestLogStore {
    fn write(&self, entry: RequestLogEntry) -> Result<(), String> {
        let mut entries = self.entries.write();
        entries.push_back(entry);
        while entries.len() > self.max_entries {
            entries.pop_front();
        }
        Ok(())
    }

    fn list(&self, query: RequestLogQuery) -> Result<RequestLogListResult, String> {
        let entries = self.entries.read();

        let filtered: Vec<RequestLogEntry> = entries
            .iter()
            .filter(|e| {
                if let Some(ref stage) = query.stage {
                    if e.stage != *stage {
                        return false;
                    }
                }
                if let Some(ref model) = query.model {
                    if e.model != *model {
                        return false;
                    }
                }
                if let Some(ref provider) = query.provider {
                    if e.provider != *provider {
                        return false;
                    }
                }
                if let Some(since) = query.since {
                    if e.created_at < since {
                        return false;
                    }
                }
                true
            })
            .cloned()
            .collect();

        let total = filtered.len();
        let offset = query.offset.unwrap_or(0);
        let limit = query.limit.unwrap_or(100);

        let data: Vec<RequestLogEntry> = filtered.into_iter().skip(offset).take(limit).collect();

        Ok(RequestLogListResult { data, total })
    }

    fn delete(&self, query: MaintenanceQuery) -> Result<usize, String> {
        let mut entries = self.entries.write();
        let before_len = entries.len();

        entries.retain(|e| {
            if let Some(before) = query.before {
                if e.created_at >= before {
                    return true;
                }
            }
            if let Some(ref stage) = query.stage {
                if e.stage == *stage {
                    return false;
                }
            }
            if let Some(ref model) = query.model {
                if e.model == *model {
                    return false;
                }
            }
            if let Some(ref provider) = query.provider {
                if e.provider == *provider {
                    return false;
                }
            }
            true
        });

        Ok(before_len - entries.len())
    }

    fn count(&self) -> usize {
        self.entries.read().len()
    }
}

impl Default for InMemoryRequestLogStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(model: &str, provider: &str) -> RequestLogEntry {
        RequestLogEntry {
            trace_id: uuid::Uuid::new_v4().to_string(),
            stage: "completed".to_string(),
            model: model.to_string(),
            provider: provider.to_string(),
            prompt_tokens: 10,
            completion_tokens: 20,
            total_tokens: 30,
            error_message: None,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn test_request_log_write_and_list() {
        let store = InMemoryRequestLogStore::new();
        store.write(make_entry("gpt-4", "openai")).unwrap();
        store.write(make_entry("claude-3", "anthropic")).unwrap();

        let result = store.list(RequestLogQuery::default()).unwrap();
        assert_eq!(result.total, 2);
        assert_eq!(result.data.len(), 2);
    }

    #[test]
    fn test_request_log_filter_by_model() {
        let store = InMemoryRequestLogStore::new();
        store.write(make_entry("gpt-4", "openai")).unwrap();
        store.write(make_entry("claude-3", "anthropic")).unwrap();

        let result = store
            .list(RequestLogQuery {
                model: Some("gpt-4".to_string()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(result.total, 1);
        assert_eq!(result.data[0].model, "gpt-4");
    }

    #[test]
    fn test_request_log_filter_by_provider() {
        let store = InMemoryRequestLogStore::new();
        store.write(make_entry("gpt-4", "openai")).unwrap();
        store.write(make_entry("gpt-4", "anthropic")).unwrap();

        let result = store
            .list(RequestLogQuery {
                provider: Some("anthropic".to_string()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(result.total, 1);
        assert_eq!(result.data[0].provider, "anthropic");
    }

    #[test]
    fn test_request_log_pagination() {
        let store = InMemoryRequestLogStore::new();
        for i in 0..10 {
            store
                .write(make_entry(&format!("model-{}", i), "provider"))
                .unwrap();
        }

        let result = store
            .list(RequestLogQuery {
                limit: Some(3),
                offset: Some(2),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(result.total, 10);
        assert_eq!(result.data.len(), 3);
    }

    #[test]
    fn test_request_log_delete() {
        let store = InMemoryRequestLogStore::new();
        store.write(make_entry("gpt-4", "openai")).unwrap();
        store.write(make_entry("claude-3", "anthropic")).unwrap();

        let deleted = store
            .delete(MaintenanceQuery {
                model: Some("gpt-4".to_string()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(deleted, 1);

        let result = store.list(RequestLogQuery::default()).unwrap();
        assert_eq!(result.total, 1);
    }

    #[test]
    fn test_request_log_count() {
        let store = InMemoryRequestLogStore::new();
        assert_eq!(store.count(), 0);

        store.write(make_entry("gpt-4", "openai")).unwrap();
        assert_eq!(store.count(), 1);

        store.write(make_entry("claude-3", "anthropic")).unwrap();
        assert_eq!(store.count(), 2);
    }

    #[test]
    fn test_request_log_filter_by_since() {
        let store = InMemoryRequestLogStore::new();
        store
            .write(RequestLogEntry {
                trace_id: "1".to_string(),
                stage: "completed".to_string(),
                model: "gpt-4".to_string(),
                provider: "openai".to_string(),
                prompt_tokens: 10,
                completion_tokens: 20,
                total_tokens: 30,
                error_message: None,
                created_at: Utc::now() - chrono::Duration::hours(2),
            })
            .unwrap();

        store
            .write(RequestLogEntry {
                trace_id: "2".to_string(),
                stage: "completed".to_string(),
                model: "gpt-4".to_string(),
                provider: "openai".to_string(),
                prompt_tokens: 10,
                completion_tokens: 20,
                total_tokens: 30,
                error_message: None,
                created_at: Utc::now(),
            })
            .unwrap();

        let result = store
            .list(RequestLogQuery {
                since: Some(Utc::now() - chrono::Duration::hours(1)),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(result.total, 1);
    }
}
