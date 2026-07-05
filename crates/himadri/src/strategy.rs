use rand::Rng;
use regex::Regex;
use std::sync::Arc;

use crate::latency_store::{InMemoryLatencyStore, LatencyStore};
use himadri_core::{ChatCompletionRequest, GatewayError, Role, Target};

// Re-export StrategyMode from core
pub use himadri_core::config::StrategyMode;

#[allow(private_interfaces)]
#[derive(Default)]
pub enum Strategy {
    #[default]
    Single,
    /// Try targets strictly in configured order. Retry/backoff behavior is
    /// owned by the gateway's failover loop (`Gateway::with_failover`), not
    /// per-strategy configuration.
    Fallback,
    LoadBalance,
    LeastLatency(LeastLatencyState),
    CostOptimized,
    Conditional(ConditionalConfig),
    ContentBased(ContentBasedConfig),
    ABTest(ABTestConfig),
}

pub struct LeastLatencyState {
    pub store: Arc<dyn LatencyStore>,
}

pub struct ConditionalConfig {
    pub rules: Vec<ConditionRule>,
    pub fallback: Option<Target>,
}

pub struct ConditionRule {
    pub key: ConditionKey,
    pub value: String,
    pub target: Target,
}

pub enum ConditionKey {
    Model,
    ModelPrefix,
}

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

#[allow(clippy::enum_variant_names)]
pub enum ContentConditionType {
    PromptContains,
    PromptNotContains,
    PromptRegex,
}

pub struct ABTestConfig {
    pub variants: Vec<ABTestVariant>,
}

pub struct ABTestVariant {
    pub target: Target,
    pub weight: f64,
}

impl Strategy {
    pub fn from_mode(mode: StrategyMode) -> Self {
        match mode {
            StrategyMode::Single => Strategy::Single,
            StrategyMode::Fallback => Strategy::Fallback,
            StrategyMode::LoadBalance => Strategy::LoadBalance,
            StrategyMode::LeastLatency => Strategy::LeastLatency(LeastLatencyState {
                store: Arc::new(InMemoryLatencyStore::new()),
            }),
            StrategyMode::CostOptimized => Strategy::CostOptimized,
            // The advanced strategies carry per-rule configuration; without it
            // they degrade to selecting the first target. Use
            // `from_strategy_config` to supply rules/variants.
            StrategyMode::Conditional => Strategy::Conditional(ConditionalConfig {
                rules: Vec::new(),
                fallback: None,
            }),
            StrategyMode::ContentBased => Strategy::ContentBased(ContentBasedConfig {
                rules: Vec::new(),
                fallback: None,
            }),
            StrategyMode::ABTest => Strategy::ABTest(ABTestConfig {
                variants: Vec::new(),
            }),
        }
    }

    /// Build a strategy from the full `StrategyConfig`, wiring rules/variants
    /// for the advanced strategies. Simple strategies ignore the extra fields.
    pub fn from_strategy_config(config: &himadri_core::config::StrategyConfig) -> Self {
        use himadri_core::config::{
            ConditionKeyConfig, ContentConditionTypeConfig, StrategyMode as Mode,
        };

        match config.mode {
            Mode::Conditional => {
                let rules = config
                    .conditional_rules
                    .iter()
                    .map(|r| ConditionRule {
                        key: match r.key {
                            ConditionKeyConfig::Model => ConditionKey::Model,
                            ConditionKeyConfig::ModelPrefix => ConditionKey::ModelPrefix,
                        },
                        value: r.value.clone(),
                        target: r.target.clone(),
                    })
                    .collect();
                Strategy::Conditional(ConditionalConfig {
                    rules,
                    fallback: config.strategy_fallback.clone(),
                })
            }
            Mode::ContentBased => {
                let rules = config
                    .content_rules
                    .iter()
                    .map(|r| {
                        let condition_type = match r.condition_type {
                            ContentConditionTypeConfig::PromptContains => {
                                ContentConditionType::PromptContains
                            }
                            ContentConditionTypeConfig::PromptNotContains => {
                                ContentConditionType::PromptNotContains
                            }
                            ContentConditionTypeConfig::PromptRegex => {
                                ContentConditionType::PromptRegex
                            }
                        };
                        let compiled_regex = match r.condition_type {
                            ContentConditionTypeConfig::PromptRegex => Regex::new(&r.value).ok(),
                            _ => None,
                        };
                        ContentRule {
                            condition_type,
                            value: r.value.clone(),
                            target: r.target.clone(),
                            compiled_regex,
                        }
                    })
                    .collect();
                Strategy::ContentBased(ContentBasedConfig {
                    rules,
                    fallback: config.strategy_fallback.clone(),
                })
            }
            Mode::ABTest => {
                let variants = config
                    .ab_variants
                    .iter()
                    .map(|v| ABTestVariant {
                        target: v.target.clone(),
                        weight: v.weight,
                    })
                    .collect();
                Strategy::ABTest(ABTestConfig { variants })
            }
            other => Self::from_mode(other),
        }
    }

    /// Test-only convenience: the routing path uses `from_strategy_config`.
    #[allow(dead_code)]
    pub fn with_latency_store(mode: StrategyMode, store: Arc<dyn LatencyStore>) -> Self {
        match mode {
            StrategyMode::LeastLatency => Strategy::LeastLatency(LeastLatencyState { store }),
            _ => Self::from_mode(mode),
        }
    }

    /// Select a single primary target. Retained for tests and callers that do
    /// not need failover; the routing path uses [`Strategy::select_ordered`].
    #[allow(dead_code)]
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
            Strategy::Fallback => Ok(targets[0].clone()),
            Strategy::LoadBalance => {
                let total_weight: f64 = targets.iter().map(|t| t.weight.max(0.0)).sum();
                // All-zero weights would make gen_range panic on an empty
                // range; treat them as uniform instead.
                if total_weight <= 0.0 {
                    let idx = rand::thread_rng().gen_range(0..targets.len());
                    return Ok(targets[idx].clone());
                }
                let mut rng = rand::thread_rng();
                let mut random = rng.gen_range(0.0..total_weight);

                for target in targets {
                    random -= target.weight.max(0.0);
                    if random <= 0.0 {
                        return Ok(target.clone());
                    }
                }

                Ok(targets
                    .last()
                    .cloned()
                    .expect("targets checked non-empty above"))
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
            Strategy::CostOptimized => {
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

    /// Select targets in priority order for failover.
    ///
    /// The first element is the primary choice (identical to `select`); the
    /// remaining elements are the fallback order the caller should try if the
    /// primary fails with a retryable error. Targets are de-duplicated by
    /// endpoint so the same provider/key/base_url is never tried twice.
    pub async fn select_ordered(
        &self,
        request: &ChatCompletionRequest,
        targets: &[Target],
    ) -> Result<Vec<Target>, GatewayError> {
        if targets.is_empty() {
            return Err(GatewayError::Internal("No targets configured".to_string()));
        }

        let ordered = match self {
            Strategy::Single => vec![targets[0].clone()],
            Strategy::Fallback => targets.to_vec(),
            Strategy::LoadBalance => {
                // Weighted-random primary, remaining targets as fallbacks in a
                // further weighted-random order.
                let mut remaining: Vec<Target> = targets.to_vec();
                let mut ordered = Vec::with_capacity(remaining.len());
                let mut rng = rand::thread_rng();
                while !remaining.is_empty() {
                    let total: f64 = remaining.iter().map(|t| t.weight.max(0.0)).sum();
                    let idx = if total <= 0.0 {
                        0
                    } else {
                        let mut r = rng.gen_range(0.0..total);
                        let mut chosen = remaining.len() - 1;
                        for (i, t) in remaining.iter().enumerate() {
                            r -= t.weight.max(0.0);
                            if r <= 0.0 {
                                chosen = i;
                                break;
                            }
                        }
                        chosen
                    };
                    ordered.push(remaining.remove(idx));
                }
                ordered
            }
            Strategy::LeastLatency(state) => {
                let mut with_latency = Vec::with_capacity(targets.len());
                for t in targets {
                    let avg = state.store.get_avg_latency(&t.provider).await;
                    with_latency.push((avg, t.clone()));
                }
                with_latency.sort_by_key(|a| a.0);
                with_latency.into_iter().map(|(_, t)| t).collect()
            }
            Strategy::CostOptimized => {
                let mut sorted = targets.to_vec();
                sorted.sort_by(|a, b| {
                    a.weight
                        .partial_cmp(&b.weight)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                sorted
            }
            Strategy::Conditional(config) => {
                let primary = config
                    .rules
                    .iter()
                    .find(|rule| Self::matches_condition(rule, request))
                    .map(|rule| rule.target.clone())
                    .or_else(|| config.fallback.clone());
                Self::ordered_with_primary(primary, targets)
            }
            Strategy::ContentBased(config) => {
                let primary = config
                    .rules
                    .iter()
                    .find(|rule| Self::matches_content_rule(rule, request))
                    .map(|rule| rule.target.clone())
                    .or_else(|| config.fallback.clone());
                Self::ordered_with_primary(primary, targets)
            }
            Strategy::ABTest(config) => {
                let primary = Self::select_ab_test_variant(config, targets).ok();
                Self::ordered_with_primary(primary, targets)
            }
        };

        Ok(Self::dedup_targets(ordered))
    }

    /// Build an ordered list starting with `primary` (if any), followed by the
    /// remaining configured targets as fallbacks.
    fn ordered_with_primary(primary: Option<Target>, targets: &[Target]) -> Vec<Target> {
        let mut ordered = Vec::with_capacity(targets.len() + 1);
        if let Some(p) = primary {
            ordered.push(p);
        }
        ordered.extend(targets.iter().cloned());
        ordered
    }

    /// De-duplicate targets by endpoint identity (provider + key env + base
    /// url) preserving order, so failover never retries the same endpoint.
    fn dedup_targets(targets: Vec<Target>) -> Vec<Target> {
        let mut seen = std::collections::HashSet::new();
        targets
            .into_iter()
            .filter(|t| {
                seen.insert((
                    t.provider.clone(),
                    t.api_key_env.clone(),
                    t.base_url.clone(),
                ))
            })
            .collect()
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
            .map(|c| c.flat_text())
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn select_ab_test_variant(
        config: &ABTestConfig,
        targets: &[Target],
    ) -> Result<Target, GatewayError> {
        // No variants configured (e.g. mode switched to ab_test without
        // rules): fall back to the first target instead of letting
        // `gen_range(0.0..0.0)` panic the request.
        if config.variants.is_empty() {
            return targets
                .first()
                .cloned()
                .ok_or_else(|| GatewayError::Internal("No targets".to_string()));
        }
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
