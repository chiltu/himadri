use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Per-request usage record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageRecord {
    pub request_id: String,
    pub api_key_id: Option<String>,
    pub model: String,
    pub provider: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub cost_usd: f64,
    pub latency_ms: u64,
    pub created_at: DateTime<Utc>,
    pub success: bool,
    pub error_message: Option<String>,
}

/// Aggregated usage stats per API key
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageStats {
    pub api_key_id: String,
    pub total_requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub total_prompt_tokens: u64,
    pub total_completion_tokens: u64,
    pub total_tokens: u64,
    pub total_cost_usd: f64,
    pub avg_latency_ms: f64,
    pub last_request_at: Option<DateTime<Utc>>,
    pub models_used: Vec<String>,
}

/// Dashboard summary
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardSummary {
    pub total_keys: usize,
    pub total_requests: u64,
    pub total_tokens: u64,
    pub total_cost_usd: f64,
    pub avg_latency_ms: f64,
    pub error_rate: f64,
    pub top_models: Vec<ModelUsage>,
    pub top_providers: Vec<ProviderUsage>,
    pub recent_errors: Vec<UsageRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelUsage {
    pub model: String,
    pub requests: u64,
    pub tokens: u64,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderUsage {
    pub provider: String,
    pub requests: u64,
    pub tokens: u64,
    pub cost_usd: f64,
}

/// Model pricing configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPricing {
    pub model: String,
    pub input_per_m_tokens: f64,
    pub output_per_m_tokens: f64,
}

/// In-memory usage store
pub struct UsageStore {
    records: Arc<dashmap::DashMap<String, UsageRecord>>,
    pricing: Arc<Vec<ModelPricing>>,
}

impl UsageStore {
    pub fn new() -> Self {
        Self {
            records: Arc::new(DashMap::new()),
            pricing: Arc::new(Self::default_pricing()),
        }
    }

    fn default_pricing() -> Vec<ModelPricing> {
        vec![
            ModelPricing {
                model: "gpt-4".to_string(),
                input_per_m_tokens: 30.0,
                output_per_m_tokens: 60.0,
            },
            ModelPricing {
                model: "gpt-4o".to_string(),
                input_per_m_tokens: 2.5,
                output_per_m_tokens: 10.0,
            },
            ModelPricing {
                model: "gpt-4o-mini".to_string(),
                input_per_m_tokens: 0.15,
                output_per_m_tokens: 0.6,
            },
            ModelPricing {
                model: "gpt-3.5-turbo".to_string(),
                input_per_m_tokens: 0.5,
                output_per_m_tokens: 1.5,
            },
            ModelPricing {
                model: "claude-3-5-sonnet-20241022".to_string(),
                input_per_m_tokens: 3.0,
                output_per_m_tokens: 15.0,
            },
            ModelPricing {
                model: "claude-3-opus-20240229".to_string(),
                input_per_m_tokens: 15.0,
                output_per_m_tokens: 75.0,
            },
            ModelPricing {
                model: "claude-3-haiku-20240307".to_string(),
                input_per_m_tokens: 0.25,
                output_per_m_tokens: 1.25,
            },
            ModelPricing {
                model: "gemini-2.0-flash".to_string(),
                input_per_m_tokens: 0.075,
                output_per_m_tokens: 0.3,
            },
            ModelPricing {
                model: "gemini-1.5-pro".to_string(),
                input_per_m_tokens: 1.25,
                output_per_m_tokens: 5.0,
            },
        ]
    }

    /// Record a request
    pub fn record(&self, record: UsageRecord) {
        self.records.insert(record.request_id.clone(), record);
    }

    /// Calculate cost for a request
    pub fn calculate_cost(&self, model: &str, prompt_tokens: u32, completion_tokens: u32) -> f64 {
        let pricing = self.pricing.iter().find(|p| p.model == model);
        match pricing {
            Some(p) => {
                (prompt_tokens as f64 / 1_000_000.0) * p.input_per_m_tokens
                    + (completion_tokens as f64 / 1_000_000.0) * p.output_per_m_tokens
            }
            None => 0.0, // Unknown model = free
        }
    }

    /// Get usage stats for an API key
    pub fn get_key_stats(&self, api_key_id: &str) -> UsageStats {
        let mut total_requests = 0u64;
        let mut successful = 0u64;
        let mut failed = 0u64;
        let mut total_prompt = 0u64;
        let mut total_completion = 0u64;
        let mut total_cost = 0.0;
        let mut total_latency = 0u64;
        let mut last_request = None;
        let mut models_used = std::collections::HashSet::new();

        for record in self.records.iter() {
            if record.api_key_id.as_deref() == Some(api_key_id) {
                total_requests += 1;
                if record.success {
                    successful += 1;
                } else {
                    failed += 1;
                }
                total_prompt += record.prompt_tokens as u64;
                total_completion += record.completion_tokens as u64;
                total_cost += record.cost_usd;
                total_latency += record.latency_ms;
                models_used.insert(record.model.clone());
                if last_request.is_none_or(|t: DateTime<Utc>| record.created_at > t) {
                    last_request = Some(record.created_at);
                }
            }
        }

        UsageStats {
            api_key_id: api_key_id.to_string(),
            total_requests,
            successful_requests: successful,
            failed_requests: failed,
            total_prompt_tokens: total_prompt,
            total_completion_tokens: total_completion,
            total_tokens: total_prompt + total_completion,
            total_cost_usd: total_cost,
            avg_latency_ms: if total_requests > 0 {
                total_latency as f64 / total_requests as f64
            } else {
                0.0
            },
            last_request_at: last_request,
            models_used: models_used.into_iter().collect(),
        }
    }

    /// Get dashboard summary
    pub fn get_dashboard(&self, key_count: usize) -> DashboardSummary {
        let mut total_requests = 0u64;
        let mut total_tokens = 0u64;
        let mut total_cost = 0.0;
        let mut total_latency = 0u64;
        let mut errors = 0u64;
        let mut model_stats: std::collections::HashMap<String, (u64, u64, f64)> =
            std::collections::HashMap::new();
        let mut provider_stats: std::collections::HashMap<String, (u64, u64, f64)> =
            std::collections::HashMap::new();

        for record in self.records.iter() {
            total_requests += 1;
            total_tokens += record.total_tokens as u64;
            total_cost += record.cost_usd;
            total_latency += record.latency_ms;
            if !record.success {
                errors += 1;
            }

            let entry = model_stats
                .entry(record.model.clone())
                .or_insert((0, 0, 0.0));
            entry.0 += 1;
            entry.1 += record.total_tokens as u64;
            entry.2 += record.cost_usd;

            let pentry = provider_stats
                .entry(record.provider.clone())
                .or_insert((0, 0, 0.0));
            pentry.0 += 1;
            pentry.1 += record.total_tokens as u64;
            pentry.2 += record.cost_usd;
        }

        let mut top_models: Vec<ModelUsage> = model_stats
            .into_iter()
            .map(|(model, (requests, tokens, cost))| ModelUsage {
                model,
                requests,
                tokens,
                cost_usd: cost,
            })
            .collect();
        top_models.sort_by_key(|b| std::cmp::Reverse(b.requests));
        top_models.truncate(10);

        let mut top_providers: Vec<ProviderUsage> = provider_stats
            .into_iter()
            .map(|(provider, (requests, tokens, cost))| ProviderUsage {
                provider,
                requests,
                tokens,
                cost_usd: cost,
            })
            .collect();
        top_providers.sort_by_key(|b| std::cmp::Reverse(b.requests));
        top_providers.truncate(10);

        let recent_errors: Vec<UsageRecord> = self
            .records
            .iter()
            .filter(|r| !r.success)
            .take(10)
            .map(|r| r.value().clone())
            .collect();

        DashboardSummary {
            total_keys: key_count,
            total_requests,
            total_tokens,
            total_cost_usd: total_cost,
            avg_latency_ms: if total_requests > 0 {
                total_latency as f64 / total_requests as f64
            } else {
                0.0
            },
            error_rate: if total_requests > 0 {
                errors as f64 / total_requests as f64
            } else {
                0.0
            },
            top_models,
            top_providers,
            recent_errors,
        }
    }

    /// Get total record count
    pub fn count(&self) -> usize {
        self.records.len()
    }
}

impl Default for UsageStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_record(model: &str, provider: &str, tokens: u32, cost: f64) -> UsageRecord {
        UsageRecord {
            request_id: uuid::Uuid::new_v4().to_string(),
            api_key_id: Some("key-1".to_string()),
            model: model.to_string(),
            provider: provider.to_string(),
            prompt_tokens: tokens / 2,
            completion_tokens: tokens / 2,
            total_tokens: tokens,
            cost_usd: cost,
            latency_ms: 100,
            created_at: Utc::now(),
            success: true,
            error_message: None,
        }
    }

    #[test]
    fn test_usage_store_record_and_stats() {
        let store = UsageStore::new();
        store.record(make_record("gpt-4", "openai", 100, 0.01));
        store.record(make_record("gpt-4", "openai", 200, 0.02));

        let stats = store.get_key_stats("key-1");
        assert_eq!(stats.total_requests, 2);
        assert_eq!(stats.total_tokens, 300);
        assert!((stats.total_cost_usd - 0.03).abs() < 0.001);
    }

    #[test]
    fn test_usage_store_calculate_cost() {
        let store = UsageStore::new();
        // gpt-4: $30/1M input, $60/1M output
        let cost = store.calculate_cost("gpt-4", 1000, 500);
        // (1000/1M * 30) + (500/1M * 60) = 0.03 + 0.03 = 0.06
        assert!((cost - 0.06).abs() < 0.001);
    }

    #[test]
    fn test_usage_store_unknown_model() {
        let store = UsageStore::new();
        let cost = store.calculate_cost("unknown-model", 1000, 500);
        assert_eq!(cost, 0.0);
    }

    #[test]
    fn test_usage_store_dashboard() {
        let store = UsageStore::new();
        store.record(make_record("gpt-4", "openai", 100, 0.01));
        store.record(make_record("gpt-4o", "openai", 200, 0.02));

        let dashboard = store.get_dashboard(5);
        assert_eq!(dashboard.total_keys, 5);
        assert_eq!(dashboard.total_requests, 2);
        assert_eq!(dashboard.top_models.len(), 2);
    }

    #[test]
    fn test_usage_store_error_tracking() {
        let store = UsageStore::new();
        store.record(UsageRecord {
            request_id: "1".to_string(),
            api_key_id: Some("key-1".to_string()),
            model: "gpt-4".to_string(),
            provider: "openai".to_string(),
            prompt_tokens: 10,
            completion_tokens: 0,
            total_tokens: 10,
            cost_usd: 0.0,
            latency_ms: 50,
            created_at: Utc::now(),
            success: false,
            error_message: Some("Rate limited".to_string()),
        });

        let stats = store.get_key_stats("key-1");
        assert_eq!(stats.failed_requests, 1);
        assert_eq!(stats.successful_requests, 0);
    }
}
