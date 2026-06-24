use rand::Rng;
use regex::Regex;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use crate::latency_store::{InMemoryLatencyStore, LatencyStore};
use himadri_core::{ChatCompletionRequest, GatewayError, Role, Target};

// Re-export StrategyMode from core
pub use himadri_core::config::StrategyMode;

#[allow(dead_code, private_interfaces)]
#[derive(Default)]
pub enum Strategy {
    #[default]
    Single,
    Fallback(FallbackConfig),
    LoadBalance(LoadBalanceState),
    LeastLatency(LeastLatencyState),
    CostOptimized(CostOptimizedConfig),
    Conditional(ConditionalConfig),
    ContentBased(ContentBasedConfig),
    ABTest(ABTestConfig),
}

#[allow(dead_code)]
pub(crate) struct LoadBalanceState {
    counter: AtomicU64,
}

pub struct LeastLatencyState {
    pub store: Arc<dyn LatencyStore>,
}

#[allow(dead_code)]
pub struct FallbackConfig {
    pub max_retries: u32,
    pub retry_delay_ms: u64,
    pub retryable_status_codes: Vec<u16>,
}

impl Default for FallbackConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            retry_delay_ms: 100,
            retryable_status_codes: vec![502, 503, 529, 429],
        }
    }
}

#[allow(dead_code)]
pub struct CostOptimizedConfig {
    pub unpriced_strategy: UnpricedStrategy,
}

#[allow(dead_code)]
pub enum UnpricedStrategy {
    Fallback, // Use if nothing priced
    Skip,     // Error if no priced option
    Allow,    // Treat as free
}

impl Default for CostOptimizedConfig {
    fn default() -> Self {
        Self {
            unpriced_strategy: UnpricedStrategy::Fallback,
        }
    }
}

#[allow(dead_code)]
pub struct ConditionalConfig {
    pub rules: Vec<ConditionRule>,
    pub fallback: Option<Target>,
}

pub struct ConditionRule {
    pub key: ConditionKey,
    pub value: String,
    pub target: Target,
}

#[allow(dead_code)]
pub enum ConditionKey {
    Model,
    ModelPrefix,
}

#[allow(dead_code)]
pub struct ContentBasedConfig {
    pub rules: Vec<ContentRule>,
    pub fallback: Option<Target>,
}

pub struct ContentRule {
    pub condition_type: ContentConditionType,
    pub value: String,
    pub target: Target,
    pub compiled_regex: Option<Regex>,
}

#[allow(dead_code, clippy::enum_variant_names)]
pub enum ContentConditionType {
    PromptContains,
    PromptNotContains,
    PromptRegex,
}

#[allow(dead_code)]
pub struct ABTestConfig {
    pub variants: Vec<ABTestVariant>,
}

#[allow(dead_code)]
pub struct ABTestVariant {
    pub target: Target,
    pub weight: f64,
    pub label: String,
}

impl Strategy {
    pub fn from_config_mode(mode: &StrategyMode) -> Self {
        Self::from_mode(*mode)
    }

    pub fn from_mode(mode: StrategyMode) -> Self {
        match mode {
            StrategyMode::Single => Strategy::Single,
            StrategyMode::Fallback => Strategy::Fallback(FallbackConfig::default()),
            StrategyMode::LoadBalance => Strategy::LoadBalance(LoadBalanceState {
                counter: AtomicU64::new(0),
            }),
            StrategyMode::LeastLatency => Strategy::LeastLatency(LeastLatencyState {
                store: Arc::new(InMemoryLatencyStore::new()),
            }),
            StrategyMode::CostOptimized => Strategy::CostOptimized(CostOptimizedConfig::default()),
        }
    }

    #[allow(dead_code)]
    pub fn with_latency_store(mode: StrategyMode, store: Arc<dyn LatencyStore>) -> Self {
        match mode {
            StrategyMode::LeastLatency => Strategy::LeastLatency(LeastLatencyState { store }),
            _ => Self::from_mode(mode),
        }
    }

    pub async fn select(
        &self,
        request: &ChatCompletionRequest,
        targets: &[Target],
    ) -> Result<Target, GatewayError> {
        if targets.is_empty() {
            return Err(GatewayError::Internal("No targets configured".to_string()));
        }

        match self {
            Strategy::Single => Ok(targets[0].clone()),
            Strategy::Fallback(_config) => {
                // Try targets in order, return first that supports the model
                if let Some(target) = targets.first() {
                    return Ok(target.clone());
                }
                Ok(targets[0].clone())
            }
            Strategy::LoadBalance(_state) => {
                let total_weight: f64 = targets.iter().map(|t| t.weight).sum();
                let mut rng = rand::thread_rng();
                let mut random = rng.gen_range(0.0..total_weight);

                for target in targets {
                    random -= target.weight;
                    if random <= 0.0 {
                        return Ok(target.clone());
                    }
                }

                Ok(targets.last().unwrap().clone())
            }
            Strategy::LeastLatency(state) => {
                let mut best_target = None;
                let mut best_latency = u64::MAX;

                for target in targets {
                    let avg = state.store.get_avg_latency(&target.provider).await;
                    if avg <= best_latency {
                        best_latency = avg;
                        best_target = Some(target.clone());
                    }
                }

                best_target.ok_or_else(|| GatewayError::Internal("No targets".to_string()))
            }
            Strategy::CostOptimized(_config) => {
                // For now, use weight as cost indicator (lower = cheaper)
                targets
                    .iter()
                    .min_by(|a, b| {
                        a.weight
                            .partial_cmp(&b.weight)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .cloned()
                    .ok_or_else(|| GatewayError::Internal("No targets".to_string()))
            }
            Strategy::Conditional(config) => {
                for rule in &config.rules {
                    if Self::matches_condition(rule, request) {
                        return Ok(rule.target.clone());
                    }
                }
                config
                    .fallback
                    .clone()
                    .or_else(|| targets.first().cloned())
                    .ok_or_else(|| GatewayError::Internal("No targets".to_string()))
            }
            Strategy::ContentBased(config) => {
                for rule in &config.rules {
                    if Self::matches_content_rule(rule, request) {
                        return Ok(rule.target.clone());
                    }
                }
                config
                    .fallback
                    .clone()
                    .or_else(|| targets.first().cloned())
                    .ok_or_else(|| GatewayError::Internal("No targets".to_string()))
            }
            Strategy::ABTest(config) => Self::select_ab_test_variant(config, targets),
        }
    }

    fn matches_condition(rule: &ConditionRule, request: &ChatCompletionRequest) -> bool {
        match rule.key {
            ConditionKey::Model => request.model == rule.value,
            ConditionKey::ModelPrefix => request.model.starts_with(&rule.value),
        }
    }

    fn matches_content_rule(rule: &ContentRule, request: &ChatCompletionRequest) -> bool {
        let user_content = Self::extract_user_content(request);

        match rule.condition_type {
            ContentConditionType::PromptContains => user_content
                .to_lowercase()
                .contains(&rule.value.to_lowercase()),
            ContentConditionType::PromptNotContains => !user_content
                .to_lowercase()
                .contains(&rule.value.to_lowercase()),
            ContentConditionType::PromptRegex => {
                if let Some(re) = &rule.compiled_regex {
                    re.is_match(&user_content)
                } else {
                    false
                }
            }
        }
    }

    fn extract_user_content(request: &ChatCompletionRequest) -> String {
        request
            .messages
            .iter()
            .filter(|m| m.role == Role::User)
            .filter_map(|m| m.content.as_ref())
            .map(|c| match c {
                MessageContent::Text(text) => text.as_str(),
                MessageContent::Parts(parts) => {
                    let joined = parts
                        .iter()
                        .filter_map(|p| match p {
                            himadri_core::ContentPart::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    Box::leak(joined.into_boxed_str())
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn select_ab_test_variant(
        config: &ABTestConfig,
        targets: &[Target],
    ) -> Result<Target, GatewayError> {
        let total_weight: f64 = config
            .variants
            .iter()
            .map(|v| if v.weight <= 0.0 { 1.0 } else { v.weight })
            .sum();

        let mut rng = rand::thread_rng();
        let r: f64 = rng.gen_range(0.0..total_weight);
        let mut cumulative = 0.0;

        for variant in &config.variants {
            let w = if variant.weight <= 0.0 {
                1.0
            } else {
                variant.weight
            };
            cumulative += w;
            if r < cumulative {
                return Ok(variant.target.clone());
            }
        }

        // Floating-point safety net
        config
            .variants
            .last()
            .map(|v| v.target.clone())
            .or_else(|| targets.first().cloned())
            .ok_or_else(|| GatewayError::Internal("No variants".to_string()))
    }
}

use himadri_core::MessageContent;
