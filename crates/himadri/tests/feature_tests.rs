//! Integration tests for the gateway-level features added across the sprints:
//! provider failover, response caching, and embeddings routing. These drive the
//! `Gateway` directly (no HTTP) so the routing logic is exercised precisely.

mod mock_provider;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use himadri_core::config::{StrategyConfig, StrategyMode};
use himadri_core::{
    ChatCompletionRequest, ChatCompletionResponse, Config, EmbeddingData, EmbeddingInput,
    EmbeddingRequest, EmbeddingResponse, EmbeddingUsage, Message, MessageContent, Role, Target,
};
use himadri_provider::error::ProviderError;
use himadri_provider::traits::{BoxStream, Provider};

use mock_provider::MockProvider;

fn metrics() -> Arc<himadri_observability::Metrics> {
    Arc::new(himadri_observability::Metrics::new())
}

fn target(provider: &str) -> Target {
    Target {
        provider: provider.to_string(),
        weight: 1.0,
        models: None,
        api_key_env: None,
        base_url: None,
    }
}

fn fallback_config(providers: &[&str]) -> Config {
    Config {
        targets: providers.iter().map(|p| target(p)).collect(),
        strategy: StrategyConfig {
            mode: StrategyMode::Fallback,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn request(model: &str, prompt: &str) -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: model.to_string(),
        messages: vec![Message {
            role: Role::User,
            content: Some(MessageContent::Text(prompt.to_string())),
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

/// A provider whose `complete` always fails. `retryable` controls whether the
/// failure is one the gateway should fall back from.
struct FailingProvider {
    name: String,
    retryable: bool,
    call_count: Arc<AtomicUsize>,
}

impl FailingProvider {
    fn new(name: &str, retryable: bool) -> (Arc<Self>, Arc<AtomicUsize>) {
        let count = Arc::new(AtomicUsize::new(0));
        (
            Arc::new(Self {
                name: name.to_string(),
                retryable,
                call_count: count.clone(),
            }),
            count,
        )
    }
}

#[async_trait]
impl Provider for FailingProvider {
    fn name(&self) -> &str {
        &self.name
    }

    async fn complete(
        &self,
        _request: &ChatCompletionRequest,
        _api_key: &str,
    ) -> Result<ChatCompletionResponse, ProviderError> {
        self.call_count.fetch_add(1, Ordering::Relaxed);
        if self.retryable {
            Err(ProviderError::Api {
                status: 503,
                message: "unavailable".to_string(),
            })
        } else {
            Err(ProviderError::Auth("bad key".to_string()))
        }
    }

    async fn complete_stream(
        &self,
        _request: &ChatCompletionRequest,
        _api_key: &str,
    ) -> Result<BoxStream<'static, Result<himadri_core::StreamChunk, ProviderError>>, ProviderError>
    {
        self.call_count.fetch_add(1, Ordering::Relaxed);
        if self.retryable {
            Err(ProviderError::Api {
                status: 503,
                message: "unavailable".to_string(),
            })
        } else {
            Err(ProviderError::Auth("bad key".to_string()))
        }
    }
}

/// A provider that supports embeddings, returning a fixed vector per input.
struct EmbeddingProvider {
    name: String,
    call_count: Arc<AtomicUsize>,
}

impl EmbeddingProvider {
    fn new(name: &str) -> (Arc<Self>, Arc<AtomicUsize>) {
        let count = Arc::new(AtomicUsize::new(0));
        (
            Arc::new(Self {
                name: name.to_string(),
                call_count: count.clone(),
            }),
            count,
        )
    }
}

#[async_trait]
impl Provider for EmbeddingProvider {
    fn name(&self) -> &str {
        &self.name
    }

    async fn complete(
        &self,
        _request: &ChatCompletionRequest,
        _api_key: &str,
    ) -> Result<ChatCompletionResponse, ProviderError> {
        Err(ProviderError::Unsupported("chat not supported".to_string()))
    }

    async fn complete_stream(
        &self,
        _request: &ChatCompletionRequest,
        _api_key: &str,
    ) -> Result<BoxStream<'static, Result<himadri_core::StreamChunk, ProviderError>>, ProviderError>
    {
        Err(ProviderError::Unsupported("chat not supported".to_string()))
    }

    async fn embed(
        &self,
        request: &EmbeddingRequest,
        _api_key: &str,
    ) -> Result<EmbeddingResponse, ProviderError> {
        self.call_count.fetch_add(1, Ordering::Relaxed);
        let n = match &request.input {
            EmbeddingInput::Single(_) => 1,
            EmbeddingInput::Multiple(v) => v.len(),
        };
        Ok(EmbeddingResponse {
            object: "list".to_string(),
            data: (0..n)
                .map(|i| EmbeddingData {
                    object: "embedding".to_string(),
                    index: i as u32,
                    embedding: vec![0.1, 0.2, 0.3],
                })
                .collect(),
            model: request.model.clone(),
            usage: EmbeddingUsage {
                prompt_tokens: 5,
                total_tokens: 5,
            },
        })
    }
}

fn embed_request(model: &str, input: EmbeddingInput) -> EmbeddingRequest {
    EmbeddingRequest {
        model: model.to_string(),
        input,
        encoding_format: None,
        dimensions: None,
        user: None,
        extra: Default::default(),
    }
}

// ─── Failover ───────────────────────────────────────────────────────────

#[tokio::test]
async fn fallback_succeeds_when_first_target_fails_retryably() {
    let (failing, fail_count) = FailingProvider::new("failing", true);
    let healthy = Arc::new(MockProvider::new("healthy", "hi"));

    let gw = himadri::Gateway::new(fallback_config(&["failing", "healthy"]), metrics());
    gw.register_provider(failing);
    gw.register_provider(healthy);

    let resp = gw
        .route(request("gpt-4", "hello"), None, None)
        .await
        .expect("should fall back to the healthy provider");

    assert!(resp.choices[0]
        .message
        .content
        .as_deref()
        .unwrap()
        .contains("hi"));
    assert_eq!(fail_count.load(Ordering::Relaxed), 1, "failing tried once");
}

#[tokio::test]
async fn non_retryable_error_does_not_fall_back() {
    let (failing, fail_count) = FailingProvider::new("failing", false);
    let healthy = Arc::new(MockProvider::new("healthy", "hi"));
    let healthy_handle = healthy.clone();

    let gw = himadri::Gateway::new(fallback_config(&["failing", "healthy"]), metrics());
    gw.register_provider(failing);
    gw.register_provider(healthy);

    let result = gw.route(request("gpt-4", "hello"), None, None).await;

    assert!(result.is_err(), "non-retryable error should propagate");
    assert_eq!(fail_count.load(Ordering::Relaxed), 1);
    assert_eq!(
        healthy_handle.call_count(),
        0,
        "healthy provider must not be tried after a non-retryable failure"
    );
}

#[tokio::test]
async fn fallback_exhausts_all_targets_then_errors() {
    let (f1, c1) = FailingProvider::new("f1", true);
    let (f2, c2) = FailingProvider::new("f2", true);

    let gw = himadri::Gateway::new(fallback_config(&["f1", "f2"]), metrics());
    gw.register_provider(f1);
    gw.register_provider(f2);

    let result = gw.route(request("gpt-4", "hello"), None, None).await;

    assert!(result.is_err());
    assert_eq!(c1.load(Ordering::Relaxed), 1);
    assert_eq!(c2.load(Ordering::Relaxed), 1, "all targets attempted");
}

#[tokio::test]
async fn streaming_falls_back_when_first_target_fails_to_open() {
    use futures::StreamExt;

    let (failing, fail_count) = FailingProvider::new("failing", true);
    let healthy = Arc::new(MockProvider::new("healthy", "streamed words"));

    let gw = himadri::Gateway::new(fallback_config(&["failing", "healthy"]), metrics());
    gw.register_provider(failing);
    gw.register_provider(healthy);

    let mut req = request("gpt-4", "hello");
    req.stream = true;

    let stream = gw
        .route_stream(req, None, None)
        .await
        .expect("stream should open via the healthy provider after fallback");

    let chunks: Vec<_> = stream.collect().await;
    let text: String = chunks
        .into_iter()
        .filter_map(|c| c.ok())
        .filter_map(|c| c.choices.into_iter().next())
        .filter_map(|c| c.delta.content)
        .collect();

    assert!(text.contains("streamed"), "got streamed content: {text:?}");
    assert_eq!(fail_count.load(Ordering::Relaxed), 1);
}

// ─── Response cache ──────────────────────────────────────────────────────

#[tokio::test]
async fn cache_serves_identical_request_without_hitting_provider() {
    let healthy = Arc::new(MockProvider::new("healthy", "hi"));
    let handle = healthy.clone();

    let mut gw = himadri::Gateway::new(fallback_config(&["healthy"]), metrics());
    gw.set_response_cache(himadri_plugins::ResponseCachePlugin::new(
        100,
        std::time::Duration::from_secs(60),
    ));
    gw.register_provider(healthy);

    gw.route(request("gpt-4", "same"), None, None)
        .await
        .unwrap();
    gw.route(request("gpt-4", "same"), None, None)
        .await
        .unwrap();

    assert_eq!(
        handle.call_count(),
        1,
        "second identical request should be served from cache"
    );
}

#[tokio::test]
async fn cache_misses_for_different_prompt() {
    let healthy = Arc::new(MockProvider::new("healthy", "hi"));
    let handle = healthy.clone();

    let mut gw = himadri::Gateway::new(fallback_config(&["healthy"]), metrics());
    gw.set_response_cache(himadri_plugins::ResponseCachePlugin::new(
        100,
        std::time::Duration::from_secs(60),
    ));
    gw.register_provider(healthy);

    gw.route(request("gpt-4", "first"), None, None)
        .await
        .unwrap();
    gw.route(request("gpt-4", "second"), None, None)
        .await
        .unwrap();

    assert_eq!(
        handle.call_count(),
        2,
        "different prompts both hit provider"
    );
}

// ─── Embeddings ──────────────────────────────────────────────────────────

#[tokio::test]
async fn embed_skips_unsupported_provider_and_uses_supporting_one() {
    // MockProvider does not implement embed (default: Unsupported); the
    // EmbeddingProvider does and is listed second.
    let unsupported = Arc::new(MockProvider::new("chat-only", "hi"));
    let (embedder, embed_count) = EmbeddingProvider::new("embedder");

    let gw = himadri::Gateway::new(fallback_config(&["chat-only", "embedder"]), metrics());
    gw.register_provider(unsupported);
    gw.register_provider(embedder);

    let resp = gw
        .embed(
            embed_request(
                "text-embedding-3-small",
                EmbeddingInput::Single("hi".to_string()),
            ),
            None,
        )
        .await
        .expect("should route to the embedding-capable provider");

    assert_eq!(resp.data.len(), 1);
    assert_eq!(resp.data[0].embedding.len(), 3);
    assert_eq!(embed_count.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn embed_returns_count_matching_multiple_inputs() {
    let (embedder, _) = EmbeddingProvider::new("embedder");
    let gw = himadri::Gateway::new(fallback_config(&["embedder"]), metrics());
    gw.register_provider(embedder);

    let resp = gw
        .embed(
            embed_request(
                "text-embedding-3-small",
                EmbeddingInput::Multiple(vec!["a".to_string(), "b".to_string()]),
            ),
            None,
        )
        .await
        .unwrap();

    assert_eq!(resp.data.len(), 2);
}

#[tokio::test]
async fn embed_errors_when_no_provider_supports_embeddings() {
    let unsupported = Arc::new(MockProvider::new("chat-only", "hi"));
    let gw = himadri::Gateway::new(fallback_config(&["chat-only"]), metrics());
    gw.register_provider(unsupported);

    let result = gw
        .embed(
            embed_request(
                "text-embedding-3-small",
                EmbeddingInput::Single("hi".to_string()),
            ),
            None,
        )
        .await;

    assert!(
        result.is_err(),
        "no embedding-capable provider should error"
    );
}

// ─── RBAC (tiered access) ────────────────────────────────────────────

use himadri_core::config::{RbacConfig, RolePolicy};
use himadri_core::{AuthContext, AuthScope};
use std::collections::HashMap;

fn auth(roles: &[&str], admin: bool) -> AuthContext {
    AuthContext {
        api_key: "test".to_string(),
        key_id: None,
        scope: if admin {
            AuthScope::Admin
        } else {
            AuthScope::ApiKey
        },
        org_id: None,
        team_id: None,
        user_id: Some("u1".to_string()),
        rate_limit_override: None,
        roles: roles.iter().map(|s| s.to_string()).collect(),
        budget_limit_usd: None,
    }
}

fn rbac_config(providers: &[&str], roles: Vec<(&str, RolePolicy)>) -> Config {
    let mut role_map = HashMap::new();
    for (name, policy) in roles {
        role_map.insert(name.to_string(), policy);
    }
    Config {
        targets: providers.iter().map(|p| target(p)).collect(),
        rbac: RbacConfig {
            enabled: true,
            roles: role_map,
            default_role: None,
        },
        ..Default::default()
    }
}

fn role(models: Option<&[&str]>, providers: Option<&[&str]>) -> RolePolicy {
    RolePolicy {
        models: models.map(|m| m.iter().map(|s| s.to_string()).collect()),
        providers: providers.map(|p| p.iter().map(|s| s.to_string()).collect()),
    }
}

#[tokio::test]
async fn rbac_denies_model_outside_role() {
    let gw = himadri::Gateway::new(
        rbac_config(
            &["healthy"],
            vec![("analyst", role(Some(&["gpt-4o-mini"]), None))],
        ),
        metrics(),
    );
    gw.register_provider(Arc::new(MockProvider::new("healthy", "hi")));

    // analyst may not use gpt-4o
    let denied = gw
        .route(
            request("gpt-4o", "hello"),
            Some(&auth(&["analyst"], false)),
            None,
        )
        .await;
    assert!(matches!(
        denied,
        Err(himadri_core::GatewayError::Forbidden(_))
    ));

    // analyst may use gpt-4o-mini
    let allowed = gw
        .route(
            request("gpt-4o-mini", "hello"),
            Some(&auth(&["analyst"], false)),
            None,
        )
        .await;
    assert!(allowed.is_ok());
}

#[tokio::test]
async fn rbac_admin_scope_bypasses() {
    let gw = himadri::Gateway::new(
        rbac_config(
            &["healthy"],
            vec![("analyst", role(Some(&["gpt-4o-mini"]), None))],
        ),
        metrics(),
    );
    gw.register_provider(Arc::new(MockProvider::new("healthy", "hi")));

    // Admin scope ignores the model allow-list.
    let allowed = gw
        .route(request("gpt-4o", "hello"), Some(&auth(&[], true)), None)
        .await;
    assert!(allowed.is_ok());
}

#[tokio::test]
async fn rbac_unknown_role_denied() {
    let gw = himadri::Gateway::new(
        rbac_config(
            &["healthy"],
            vec![("analyst", role(Some(&["gpt-4o-mini"]), None))],
        ),
        metrics(),
    );
    gw.register_provider(Arc::new(MockProvider::new("healthy", "hi")));

    let denied = gw
        .route(
            request("gpt-4o-mini", "hello"),
            Some(&auth(&["stranger"], false)),
            None,
        )
        .await;
    assert!(matches!(
        denied,
        Err(himadri_core::GatewayError::Forbidden(_))
    ));
}

#[tokio::test]
async fn rbac_denies_provider_outside_role() {
    // Role allows any model but only the "openai" provider; the only target is
    // "healthy", so provider filtering leaves nothing → Forbidden.
    let gw = himadri::Gateway::new(
        rbac_config(
            &["healthy"],
            vec![("engineer", role(None, Some(&["openai"])))],
        ),
        metrics(),
    );
    gw.register_provider(Arc::new(MockProvider::new("healthy", "hi")));

    let denied = gw
        .route(
            request("gpt-4o", "hello"),
            Some(&auth(&["engineer"], false)),
            None,
        )
        .await;
    assert!(matches!(
        denied,
        Err(himadri_core::GatewayError::Forbidden(_))
    ));
}
