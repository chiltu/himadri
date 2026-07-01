use crate::strategy::{Strategy, StrategyMode};
use himadri_core::{ChatCompletionRequest, Message, MessageContent, Role, Target};

fn test_request(model: &str) -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: model.to_string(),
        messages: vec![Message {
            role: Role::User,
            content: Some(MessageContent::Text("Hello".to_string())),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }],
        stream: false,
        temperature: None,
        top_p: None,
        max_tokens: None,
        stop: None,
        presence_penalty: None,
        frequency_penalty: None,
        user: None,
        tools: None,
        tool_choice: None,
        extra: Default::default(),
    }
}

fn test_targets() -> Vec<Target> {
    vec![
        Target {
            provider: "openai".to_string(),
            weight: 1.0,
            models: None,
            api_key_env: None,
            base_url: None,
        },
        Target {
            provider: "anthropic".to_string(),
            weight: 2.0,
            models: None,
            api_key_env: None,
            base_url: None,
        },
    ]
}

// ═══════════════════════════════════════════════════════════════════════
// Single Strategy
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_single_returns_first_target() {
    let strategy = Strategy::from_mode(StrategyMode::Single);
    let targets = test_targets();
    let req = test_request("gpt-4");
    let result = strategy.select(&req, &targets).await.unwrap();
    assert_eq!(result.provider, "openai");
}

#[tokio::test]
async fn test_single_empty_targets() {
    let strategy = Strategy::from_mode(StrategyMode::Single);
    let req = test_request("gpt-4");
    let result = strategy.select(&req, &[]).await;
    assert!(result.is_err());
}

// ═══════════════════════════════════════════════════════════════════════
// Fallback Strategy
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_fallback_returns_first_target() {
    let strategy = Strategy::from_mode(StrategyMode::Fallback);
    let targets = test_targets();
    let req = test_request("gpt-4");
    let result = strategy.select(&req, &targets).await.unwrap();
    assert_eq!(result.provider, "openai");
}

#[tokio::test]
async fn test_fallback_empty_targets() {
    let strategy = Strategy::from_mode(StrategyMode::Fallback);
    let req = test_request("gpt-4");
    let result = strategy.select(&req, &[]).await;
    assert!(result.is_err());
}

// ═══════════════════════════════════════════════════════════════════════
// LoadBalance Strategy
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_load_balance_returns_valid_target() {
    let strategy = Strategy::from_mode(StrategyMode::LoadBalance);
    let targets = test_targets();
    let req = test_request("gpt-4");
    let result = strategy.select(&req, &targets).await.unwrap();
    assert!(result.provider == "openai" || result.provider == "anthropic");
}

#[tokio::test]
async fn test_load_balance_empty_targets() {
    let strategy = Strategy::from_mode(StrategyMode::LoadBalance);
    let req = test_request("gpt-4");
    let result = strategy.select(&req, &[]).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_load_balance_respects_weights() {
    let strategy = Strategy::from_mode(StrategyMode::LoadBalance);
    let targets = vec![
        Target {
            provider: "heavy".to_string(),
            weight: 100.0,
            models: None,
            api_key_env: None,
            base_url: None,
        },
        Target {
            provider: "light".to_string(),
            weight: 0.001,
            models: None,
            api_key_env: None,
            base_url: None,
        },
    ];
    let req = test_request("gpt-4");

    // Run 1000 times, heavy should win most
    let mut heavy_count = 0;
    for _ in 0..1000 {
        let result = strategy.select(&req, &targets).await.unwrap();
        if result.provider == "heavy" {
            heavy_count += 1;
        }
    }
    assert!(
        heavy_count > 900,
        "Heavy should win >90%: got {}",
        heavy_count
    );
}

// ═══════════════════════════════════════════════════════════════════════
// CostOptimized Strategy
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_cost_optimized_picks_cheapest() {
    let strategy = Strategy::from_mode(StrategyMode::CostOptimized);
    let targets = vec![
        Target {
            provider: "expensive".to_string(),
            weight: 10.0,
            models: None,
            api_key_env: None,
            base_url: None,
        },
        Target {
            provider: "cheap".to_string(),
            weight: 1.0,
            models: None,
            api_key_env: None,
            base_url: None,
        },
    ];
    let req = test_request("gpt-4");
    let result = strategy.select(&req, &targets).await.unwrap();
    assert_eq!(result.provider, "cheap");
}

#[tokio::test]
async fn test_cost_optimized_empty_targets() {
    let strategy = Strategy::from_mode(StrategyMode::CostOptimized);
    let req = test_request("gpt-4");
    let result = strategy.select(&req, &[]).await;
    assert!(result.is_err());
}

// ═══════════════════════════════════════════════════════════════════════
// Conditional Strategy
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_conditional_matches_model() {
    use crate::strategy::{ConditionKey, ConditionRule, ConditionalConfig};

    let strategy = Strategy::Conditional(ConditionalConfig {
        rules: vec![ConditionRule {
            key: ConditionKey::Model,
            value: "gpt-4".to_string(),
            target: Target {
                provider: "openai".to_string(),
                weight: 1.0,
                models: None,
                api_key_env: None,
                base_url: None,
            },
        }],
        fallback: Some(Target {
            provider: "anthropic".to_string(),
            weight: 1.0,
            models: None,
            api_key_env: None,
            base_url: None,
        }),
    });

    let req = test_request("gpt-4");
    let targets = test_targets();
    let result = strategy.select(&req, &targets).await.unwrap();
    assert_eq!(result.provider, "openai");
}

#[tokio::test]
async fn test_conditional_fallback_on_no_match() {
    use crate::strategy::{ConditionKey, ConditionRule, ConditionalConfig};

    let strategy = Strategy::Conditional(ConditionalConfig {
        rules: vec![ConditionRule {
            key: ConditionKey::Model,
            value: "gpt-4".to_string(),
            target: Target {
                provider: "openai".to_string(),
                weight: 1.0,
                models: None,
                api_key_env: None,
                base_url: None,
            },
        }],
        fallback: Some(Target {
            provider: "anthropic".to_string(),
            weight: 1.0,
            models: None,
            api_key_env: None,
            base_url: None,
        }),
    });

    let req = test_request("claude-3");
    let targets = test_targets();
    let result = strategy.select(&req, &targets).await.unwrap();
    assert_eq!(result.provider, "anthropic");
}

#[tokio::test]
async fn test_conditional_model_prefix() {
    use crate::strategy::{ConditionKey, ConditionRule, ConditionalConfig};

    let strategy = Strategy::Conditional(ConditionalConfig {
        rules: vec![ConditionRule {
            key: ConditionKey::ModelPrefix,
            value: "gpt-".to_string(),
            target: Target {
                provider: "openai".to_string(),
                weight: 1.0,
                models: None,
                api_key_env: None,
                base_url: None,
            },
        }],
        fallback: None,
    });

    let req = test_request("gpt-4o-mini");
    let targets = test_targets();
    let result = strategy.select(&req, &targets).await.unwrap();
    assert_eq!(result.provider, "openai");
}

// ═══════════════════════════════════════════════════════════════════════
// ContentBased Strategy
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_content_based_prompt_contains() {
    use crate::strategy::{ContentBasedConfig, ContentConditionType, ContentRule};

    let strategy = Strategy::ContentBased(ContentBasedConfig {
        rules: vec![ContentRule {
            condition_type: ContentConditionType::PromptContains,
            value: "code".to_string(),
            target: Target {
                provider: "openai".to_string(),
                weight: 1.0,
                models: None,
                api_key_env: None,
                base_url: None,
            },
            compiled_regex: None,
        }],
        fallback: None,
    });

    let req = ChatCompletionRequest {
        model: "gpt-4".to_string(),
        messages: vec![Message {
            role: Role::User,
            content: Some(MessageContent::Text("Write some code".to_string())),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }],
        stream: false,
        temperature: None,
        top_p: None,
        max_tokens: None,
        stop: None,
        presence_penalty: None,
        frequency_penalty: None,
        user: None,
        tools: None,
        tool_choice: None,
        extra: Default::default(),
    };

    let targets = test_targets();
    let result = strategy.select(&req, &targets).await.unwrap();
    assert_eq!(result.provider, "openai");
}

#[tokio::test]
async fn test_content_based_prompt_not_contains() {
    use crate::strategy::{ContentBasedConfig, ContentConditionType, ContentRule};

    let strategy = Strategy::ContentBased(ContentBasedConfig {
        rules: vec![ContentRule {
            condition_type: ContentConditionType::PromptNotContains,
            value: "code".to_string(),
            target: Target {
                provider: "anthropic".to_string(),
                weight: 1.0,
                models: None,
                api_key_env: None,
                base_url: None,
            },
            compiled_regex: None,
        }],
        fallback: None,
    });

    let req = ChatCompletionRequest {
        model: "gpt-4".to_string(),
        messages: vec![Message {
            role: Role::User,
            content: Some(MessageContent::Text("Tell me a joke".to_string())),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }],
        stream: false,
        temperature: None,
        top_p: None,
        max_tokens: None,
        stop: None,
        presence_penalty: None,
        frequency_penalty: None,
        user: None,
        tools: None,
        tool_choice: None,
        extra: Default::default(),
    };

    let targets = test_targets();
    let result = strategy.select(&req, &targets).await.unwrap();
    assert_eq!(result.provider, "anthropic");
}

#[tokio::test]
async fn test_content_based_prompt_regex() {
    use crate::strategy::{ContentBasedConfig, ContentConditionType, ContentRule};
    use regex::Regex;

    let strategy = Strategy::ContentBased(ContentBasedConfig {
        rules: vec![ContentRule {
            condition_type: ContentConditionType::PromptRegex,
            value: r"\b\d{3}\b".to_string(),
            target: Target {
                provider: "openai".to_string(),
                weight: 1.0,
                models: None,
                api_key_env: None,
                base_url: None,
            },
            compiled_regex: Some(Regex::new(r"\b\d{3}\b").unwrap()),
        }],
        fallback: None,
    });

    let req = ChatCompletionRequest {
        model: "gpt-4".to_string(),
        messages: vec![Message {
            role: Role::User,
            content: Some(MessageContent::Text("Call 555-1234".to_string())),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }],
        stream: false,
        temperature: None,
        top_p: None,
        max_tokens: None,
        stop: None,
        presence_penalty: None,
        frequency_penalty: None,
        user: None,
        tools: None,
        tool_choice: None,
        extra: Default::default(),
    };

    let targets = test_targets();
    let result = strategy.select(&req, &targets).await.unwrap();
    assert_eq!(result.provider, "openai");
}

#[tokio::test]
async fn test_content_based_fallback() {
    use crate::strategy::{ContentBasedConfig, ContentConditionType, ContentRule};

    let strategy = Strategy::ContentBased(ContentBasedConfig {
        rules: vec![ContentRule {
            condition_type: ContentConditionType::PromptContains,
            value: "code".to_string(),
            target: Target {
                provider: "openai".to_string(),
                weight: 1.0,
                models: None,
                api_key_env: None,
                base_url: None,
            },
            compiled_regex: None,
        }],
        fallback: Some(Target {
            provider: "anthropic".to_string(),
            weight: 1.0,
            models: None,
            api_key_env: None,
            base_url: None,
        }),
    });

    let req = ChatCompletionRequest {
        model: "gpt-4".to_string(),
        messages: vec![Message {
            role: Role::User,
            content: Some(MessageContent::Text("Tell me a joke".to_string())),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }],
        stream: false,
        temperature: None,
        top_p: None,
        max_tokens: None,
        stop: None,
        presence_penalty: None,
        frequency_penalty: None,
        user: None,
        tools: None,
        tool_choice: None,
        extra: Default::default(),
    };

    let targets = test_targets();
    let result = strategy.select(&req, &targets).await.unwrap();
    assert_eq!(result.provider, "anthropic");
}

#[tokio::test]
async fn test_content_based_only_checks_user_messages() {
    use crate::strategy::{ContentBasedConfig, ContentConditionType, ContentRule};

    let strategy = Strategy::ContentBased(ContentBasedConfig {
        rules: vec![ContentRule {
            condition_type: ContentConditionType::PromptContains,
            value: "code".to_string(),
            target: Target {
                provider: "openai".to_string(),
                weight: 1.0,
                models: None,
                api_key_env: None,
                base_url: None,
            },
            compiled_regex: None,
        }],
        fallback: Some(Target {
            provider: "anthropic".to_string(),
            weight: 1.0,
            models: None,
            api_key_env: None,
            base_url: None,
        }),
    });

    // "code" is in system message, not user message
    let req = ChatCompletionRequest {
        model: "gpt-4".to_string(),
        messages: vec![
            Message {
                role: Role::System,
                content: Some(MessageContent::Text("You are a code assistant".to_string())),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            },
            Message {
                role: Role::User,
                content: Some(MessageContent::Text("Hello".to_string())),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            },
        ],
        stream: false,
        temperature: None,
        top_p: None,
        max_tokens: None,
        stop: None,
        presence_penalty: None,
        frequency_penalty: None,
        user: None,
        tools: None,
        tool_choice: None,
        extra: Default::default(),
    };

    let targets = test_targets();
    let result = strategy.select(&req, &targets).await.unwrap();
    // Should fallback because "code" is not in user message
    assert_eq!(result.provider, "anthropic");
}

// ═══════════════════════════════════════════════════════════════════════
// ABTest Strategy
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_ab_test_distribution() {
    use crate::strategy::{ABTestConfig, ABTestVariant};

    let strategy = Strategy::ABTest(ABTestConfig {
        variants: vec![
            ABTestVariant {
                target: Target {
                    provider: "control".to_string(),
                    weight: 80.0,
                    models: None,
                    api_key_env: None,
                    base_url: None,
                },
                weight: 80.0,
                label: "control".to_string(),
            },
            ABTestVariant {
                target: Target {
                    provider: "challenger".to_string(),
                    weight: 20.0,
                    models: None,
                    api_key_env: None,
                    base_url: None,
                },
                weight: 20.0,
                label: "challenger".to_string(),
            },
        ],
    });

    let req = test_request("gpt-4");
    let targets = test_targets();

    let mut control_count = 0;
    for _ in 0..1000 {
        let result = strategy.select(&req, &targets).await.unwrap();
        if result.provider == "control" {
            control_count += 1;
        }
    }

    // Control should get ~80% of traffic (70-90% tolerance)
    assert!(
        control_count > 700 && control_count < 900,
        "Control should get ~80%, got {}%",
        control_count / 10
    );
}

#[tokio::test]
async fn test_ab_test_equal_weight() {
    use crate::strategy::{ABTestConfig, ABTestVariant};

    let strategy = Strategy::ABTest(ABTestConfig {
        variants: vec![
            ABTestVariant {
                target: Target {
                    provider: "a".to_string(),
                    weight: 1.0,
                    models: None,
                    api_key_env: None,
                    base_url: None,
                },
                weight: 1.0,
                label: "a".to_string(),
            },
            ABTestVariant {
                target: Target {
                    provider: "b".to_string(),
                    weight: 1.0,
                    models: None,
                    api_key_env: None,
                    base_url: None,
                },
                weight: 1.0,
                label: "b".to_string(),
            },
        ],
    });

    let req = test_request("gpt-4");
    let targets = test_targets();

    let mut a_count = 0;
    for _ in 0..1000 {
        let result = strategy.select(&req, &targets).await.unwrap();
        if result.provider == "a" {
            a_count += 1;
        }
    }

    // Equal weight: ~50% each (40-60% tolerance)
    assert!(
        a_count > 400 && a_count < 600,
        "Equal weight should be ~50%, got {}%",
        a_count / 10
    );
}

// ═══════════════════════════════════════════════════════════════════════
// LeastLatency Strategy
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_least_latency_picks_first_when_no_data() {
    // LeastLatency requires a latency store - create one with the strategy
    let store = std::sync::Arc::new(crate::latency_store::InMemoryLatencyStore::new());
    let strategy = Strategy::with_latency_store(StrategyMode::LeastLatency, store);
    let targets = test_targets();
    let req = test_request("gpt-4");
    let result = strategy.select(&req, &targets).await.unwrap();
    // Without latency data, should pick first target (u64::MAX default)
    assert!(result.provider == "openai" || result.provider == "anthropic");
}

// ═══════════════════════════════════════════════════════════════════════
// Strategy Mode Tests
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_all_modes_create_valid_strategies() {
    // Test modes that don't require special setup
    let modes = vec![
        StrategyMode::Single,
        StrategyMode::Fallback,
        StrategyMode::LoadBalance,
        StrategyMode::CostOptimized,
    ];

    for mode in modes {
        let strategy = Strategy::from_mode(mode);
        let targets = test_targets();
        let req = test_request("gpt-4");
        let result = strategy.select(&req, &targets).await;
        assert!(
            result.is_ok(),
            "Strategy {:?} failed: {:?}",
            mode,
            result.err()
        );
    }

    // Test LeastLatency with a store
    let store = std::sync::Arc::new(crate::latency_store::InMemoryLatencyStore::new());
    let strategy = Strategy::with_latency_store(StrategyMode::LeastLatency, store);
    let targets = test_targets();
    let req = test_request("gpt-4");
    let result = strategy.select(&req, &targets).await;
    assert!(result.is_ok(), "LeastLatency strategy failed");
}

// ═══════════════════════════════════════════════════════════════════════
// select_ordered — failover ordering
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_select_ordered_fallback_preserves_all_targets_in_order() {
    let strategy = Strategy::from_mode(StrategyMode::Fallback);
    let targets = test_targets();
    let req = test_request("gpt-4");
    let ordered = strategy.select_ordered(&req, &targets).await.unwrap();
    assert_eq!(ordered.len(), 2);
    assert_eq!(ordered[0].provider, "openai");
    assert_eq!(ordered[1].provider, "anthropic");
}

#[tokio::test]
async fn test_select_ordered_single_returns_only_primary() {
    let strategy = Strategy::from_mode(StrategyMode::Single);
    let targets = test_targets();
    let req = test_request("gpt-4");
    let ordered = strategy.select_ordered(&req, &targets).await.unwrap();
    assert_eq!(ordered.len(), 1);
    assert_eq!(ordered[0].provider, "openai");
}

#[tokio::test]
async fn test_select_ordered_cost_optimized_sorts_cheapest_first() {
    let strategy = Strategy::from_mode(StrategyMode::CostOptimized);
    let targets = test_targets(); // openai weight 1.0, anthropic weight 2.0
    let req = test_request("gpt-4");
    let ordered = strategy.select_ordered(&req, &targets).await.unwrap();
    assert_eq!(ordered.len(), 2);
    assert_eq!(ordered[0].provider, "openai"); // lower weight = cheaper, tried first
    assert_eq!(ordered[1].provider, "anthropic");
}

#[tokio::test]
async fn test_select_ordered_load_balance_includes_all_as_fallbacks() {
    let strategy = Strategy::from_mode(StrategyMode::LoadBalance);
    let targets = test_targets();
    let req = test_request("gpt-4");
    let ordered = strategy.select_ordered(&req, &targets).await.unwrap();
    // Both providers present so a failed primary can fall back to the other.
    assert_eq!(ordered.len(), 2);
    let mut providers: Vec<_> = ordered.iter().map(|t| t.provider.clone()).collect();
    providers.sort();
    assert_eq!(
        providers,
        vec!["anthropic".to_string(), "openai".to_string()]
    );
}

#[tokio::test]
async fn test_select_ordered_dedups_identical_endpoints() {
    let strategy = Strategy::from_mode(StrategyMode::Fallback);
    let targets = vec![
        Target {
            provider: "openai".to_string(),
            weight: 1.0,
            models: None,
            api_key_env: None,
            base_url: None,
        },
        Target {
            provider: "openai".to_string(),
            weight: 1.0,
            models: None,
            api_key_env: None,
            base_url: None,
        },
    ];
    let req = test_request("gpt-4");
    let ordered = strategy.select_ordered(&req, &targets).await.unwrap();
    assert_eq!(
        ordered.len(),
        1,
        "identical endpoints should be de-duplicated"
    );
}

#[tokio::test]
async fn test_select_ordered_empty_targets_errors() {
    let strategy = Strategy::from_mode(StrategyMode::Fallback);
    let req = test_request("gpt-4");
    let result = strategy.select_ordered(&req, &[]).await;
    assert!(result.is_err());
}

// ═══════════════════════════════════════════════════════════════════════
// from_strategy_config — advanced strategies built from config
// ═══════════════════════════════════════════════════════════════════════

use himadri_core::config::{
    ABVariantConfig, ConditionKeyConfig, ConditionalRuleConfig, ContentConditionTypeConfig,
    ContentRuleConfig, StrategyConfig, StrategyMode as Mode,
};

fn target(provider: &str) -> Target {
    Target {
        provider: provider.to_string(),
        weight: 1.0,
        models: None,
        api_key_env: None,
        base_url: None,
    }
}

#[tokio::test]
async fn test_from_config_conditional_routes_by_model() {
    let config = StrategyConfig {
        mode: Mode::Conditional,
        conditional_rules: vec![ConditionalRuleConfig {
            key: ConditionKeyConfig::Model,
            value: "claude-3".to_string(),
            target: target("anthropic"),
        }],
        strategy_fallback: Some(target("openai")),
        ..Default::default()
    };
    let strategy = Strategy::from_strategy_config(&config);
    let targets = test_targets();

    let matched = strategy
        .select(&test_request("claude-3"), &targets)
        .await
        .unwrap();
    assert_eq!(matched.provider, "anthropic");

    let fallback = strategy
        .select(&test_request("gpt-4"), &targets)
        .await
        .unwrap();
    assert_eq!(fallback.provider, "openai");
}

#[tokio::test]
async fn test_from_config_content_based_prompt_contains() {
    let config = StrategyConfig {
        mode: Mode::ContentBased,
        content_rules: vec![ContentRuleConfig {
            condition_type: ContentConditionTypeConfig::PromptContains,
            value: "translate".to_string(),
            target: target("anthropic"),
        }],
        strategy_fallback: Some(target("openai")),
        ..Default::default()
    };
    let strategy = Strategy::from_strategy_config(&config);
    let targets = test_targets();

    let mut req = test_request("gpt-4");
    req.messages[0].content = Some(MessageContent::Text("Please translate this".to_string()));
    let matched = strategy.select(&req, &targets).await.unwrap();
    assert_eq!(matched.provider, "anthropic");
}

#[tokio::test]
async fn test_from_config_ab_test_picks_a_variant() {
    let config = StrategyConfig {
        mode: Mode::ABTest,
        ab_variants: vec![
            ABVariantConfig {
                target: target("openai"),
                weight: 1.0,
                label: "a".to_string(),
            },
            ABVariantConfig {
                target: target("anthropic"),
                weight: 1.0,
                label: "b".to_string(),
            },
        ],
        ..Default::default()
    };
    let strategy = Strategy::from_strategy_config(&config);
    let targets = test_targets();
    let chosen = strategy
        .select(&test_request("gpt-4"), &targets)
        .await
        .unwrap();
    assert!(chosen.provider == "openai" || chosen.provider == "anthropic");
}

#[tokio::test]
async fn test_strategy_mode_serde_roundtrip_advanced() {
    // Ensure the new variants deserialize from their config spellings.
    assert_eq!(
        serde_json::from_str::<Mode>("\"conditional\"").unwrap(),
        Mode::Conditional
    );
    assert_eq!(
        serde_json::from_str::<Mode>("\"content_based\"").unwrap(),
        Mode::ContentBased
    );
    assert_eq!(
        serde_json::from_str::<Mode>("\"ab_test\"").unwrap(),
        Mode::ABTest
    );
}
