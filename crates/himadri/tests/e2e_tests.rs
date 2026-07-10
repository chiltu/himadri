mod mock_provider;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    middleware,
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
    Json, Router,
};
use std::sync::Arc;
use tokio::net::TcpListener;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

use himadri_admin::{AuthMiddleware, StoreBackend};
use himadri_core::{
    ChatCompletionRequest, Config, GatewayError, Message, MessageContent, ModelListResponse, Role,
};
use himadri_plugin::Plugin;
use himadri_provider::traits::Provider;

use mock_provider::MockProvider;

#[derive(Clone)]
struct TestState {
    gateway: Arc<himadri::Gateway>,
    store: StoreBackend,
}

/// Setup test app with optional auth bypass
/// When `auth_enabled` is false, all requests pass through without auth
async fn setup_test_app(
    providers: Vec<Arc<dyn Provider>>,
    auth_enabled: bool,
) -> (String, tokio::task::JoinHandle<()>) {
    let config = Config {
        targets: providers
            .iter()
            .map(|p| himadri_core::Target {
                provider: p.name().to_string(),
                weight: 1.0,
                models: None,
                id: None,
                api_key_env: None,
                base_url: None,
            })
            .collect(),
        ..Default::default()
    };

    let gateway = himadri::Gateway::new(config, Arc::new(himadri_observability::Metrics::new()));

    for provider in providers {
        gateway.register_provider(provider);
    }

    let gateway = Arc::new(gateway);
    let store = StoreBackend::new().await;

    // Auth middleware: when auth_enabled=false, use no master key = bypass
    let auth = Arc::new(AuthMiddleware::new(
        store.clone(),
        if auth_enabled {
            Some("test-master-key".to_string())
        } else {
            None
        },
    ));

    let state = Arc::new(TestState { gateway, store });

    let public_routes = Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(list_models));

    let api_routes = Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/completions", post(completions))
        .layer(middleware::from_fn_with_state(
            auth.clone(),
            AuthMiddleware::middleware,
        ));

    let admin_routes = Router::new()
        .route("/admin/keys", get(list_keys))
        .route("/admin/keys", post(create_key))
        .route("/admin/keys/{id}", get(get_key))
        .route("/admin/keys/{id}", put(update_key))
        .route("/admin/keys/{id}", delete(delete_key))
        .route("/admin/keys/{id}/revoke", post(revoke_key))
        .layer(middleware::from_fn_with_state(
            auth.clone(),
            AuthMiddleware::middleware,
        ));

    let app = Router::new()
        .merge(public_routes)
        .merge(api_routes)
        .merge(admin_routes)
        .layer(TraceLayer::new_for_http())
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .with_state(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{}", addr);
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    (url, handle)
}

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
        temperature: Some(0.7),
        top_p: None,
        max_tokens: Some(100),
        stop: None,
        presence_penalty: None,
        frequency_penalty: None,
        user: None,
        tools: None,
        tool_choice: None,
        extra: Default::default(),
    }
}

fn test_stream_request(model: &str) -> ChatCompletionRequest {
    let mut req = test_request(model);
    req.stream = true;
    req
}

// ─── Handlers ───────────────────────────────────────────────────────

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok" }))
}

async fn list_models(State(state): State<Arc<TestState>>) -> Json<ModelListResponse> {
    let providers = state.gateway.list_providers();
    let mut models = Vec::new();
    for provider in &providers {
        models.push(himadri_core::ModelObject {
            id: format!("mock-{}", provider),
            object: "model".to_string(),
            created: 0,
            owned_by: provider.clone(),
        });
    }
    Json(ModelListResponse {
        object: "list".to_string(),
        data: models,
    })
}

async fn chat_completions(
    State(state): State<Arc<TestState>>,
    Json(request): Json<ChatCompletionRequest>,
) -> Response {
    match state.gateway.route(request, None, None).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => error_to_response(e),
    }
}

async fn completions(
    State(state): State<Arc<TestState>>,
    Json(request): Json<ChatCompletionRequest>,
) -> Response {
    match state.gateway.route(request, None, None).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => error_to_response(e),
    }
}

async fn list_keys(State(state): State<Arc<TestState>>) -> Json<Vec<himadri_admin::ApiKey>> {
    Json(state.store.list().await.unwrap_or_default())
}

async fn create_key(
    State(state): State<Arc<TestState>>,
    Json(request): Json<himadri_admin::CreateApiKeyRequest>,
) -> (StatusCode, Json<himadri_admin::ApiKey>) {
    let key = state.store.create(request).await.unwrap();
    (StatusCode::CREATED, Json(key))
}

async fn get_key(
    State(state): State<Arc<TestState>>,
    Path(id): Path<String>,
) -> Result<Json<himadri_admin::ApiKey>, StatusCode> {
    state
        .store
        .get(&id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

async fn update_key(
    State(state): State<Arc<TestState>>,
    Path(id): Path<String>,
    Json(request): Json<himadri_admin::UpdateApiKeyRequest>,
) -> Result<Json<himadri_admin::ApiKey>, StatusCode> {
    state
        .store
        .update(&id, request)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

async fn delete_key(
    State(state): State<Arc<TestState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    if state.store.delete(&id).await.unwrap_or(false) {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

async fn revoke_key(
    State(state): State<Arc<TestState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    if state.store.revoke(&id).await.unwrap_or(false) {
        Ok(StatusCode::OK)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

// Mirrors the production `handlers::error_to_response`: 5xx detail is
// sanitized at the edge so upstream bodies never reach clients.
fn error_to_response(e: GatewayError) -> Response {
    let status = StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let message = if status.is_server_error() {
        match &e {
            GatewayError::CircuitOpen(_) | GatewayError::ServiceUnavailable(_) => {
                "upstream provider unavailable".to_string()
            }
            _ => "internal server error".to_string(),
        }
    } else {
        e.to_string()
    };
    let body =
        Json(serde_json::json!({ "error": { "message": message, "type": "gateway_error" } }));
    (status, body).into_response()
}

// ═══════════════════════════════════════════════════════════════════════
// E2E TESTS — Auth Disabled (Dummy Keys)
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_health() {
    let (url, h) = setup_test_app(vec![], false).await;
    let resp = reqwest::get(format!("{}/health", url)).await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    h.abort();
}

#[tokio::test]
async fn test_list_models() {
    let mock = Arc::new(MockProvider::new("mock", "Hello"));
    let (url, h) = setup_test_app(vec![mock], false).await;
    let resp = reqwest::get(format!("{}/v1/models", url)).await.unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["data"].as_array().unwrap().len(), 1);
    h.abort();
}

#[tokio::test]
async fn test_chat_completions_no_auth() {
    let mock = Arc::new(MockProvider::new("openai", "Hello from mock"));
    let (url, h) = setup_test_app(vec![mock.clone()], false).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", url))
        .json(&test_request("mock-openai"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["choices"][0]["message"]["content"]
        .as_str()
        .unwrap()
        .contains("Hello from mock"));
    assert_eq!(body["usage"]["total_tokens"], 30);
    assert_eq!(mock.call_count(), 1);
    h.abort();
}

#[tokio::test]
async fn test_streaming_no_auth() {
    let mock = Arc::new(MockProvider::new("openai", "Hello world from mock"));
    let (url, h) = setup_test_app(vec![mock.clone()], false).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", url))
        .json(&test_stream_request("mock-openai"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let body = resp.text().await.unwrap();
    assert!(ct.contains("text/event-stream") || body.contains("data:") || body.contains("Hello"));
    assert_eq!(mock.call_count(), 1);
    h.abort();
}

#[tokio::test]
async fn test_provider_error_no_auth() {
    let mock = Arc::new(MockProvider::new("openai", "nope"));
    let (url, h) = setup_test_app(vec![mock], false).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", url))
        .json(&test_request("error-model"))
        .send()
        .await
        .unwrap();
    // Upstream 5xx surfaces as 503 (upstream unavailable), not a generic
    // 500 — and the upstream error body must NOT leak through the edge.
    assert_eq!(resp.status(), 503);
    let body: serde_json::Value = resp.json().await.unwrap();
    let message = body["error"]["message"].as_str().unwrap();
    assert!(
        !message.contains("Mock provider error"),
        "upstream detail leaked: {message}"
    );
    assert!(message.contains("unavailable"));
    h.abort();
}

#[tokio::test]
async fn test_concurrent_requests_no_auth() {
    let mock = Arc::new(MockProvider::new("openai", "Concurrent response"));
    let (url, h) = setup_test_app(vec![mock.clone()], false).await;
    let client = reqwest::Client::new();
    let mut handles = Vec::new();
    for _ in 0..10 {
        let url = url.clone();
        let client = client.clone();
        handles.push(tokio::spawn(async move {
            let resp = client
                .post(format!("{}/v1/chat/completions", url))
                .json(&test_request("mock-openai"))
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), 200);
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    assert_eq!(mock.call_count(), 10);
    h.abort();
}

// ═══════════════════════════════════════════════════════════════════════
// E2E TESTS — Admin API (Auth Disabled)
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_admin_create_key() {
    let (url, h) = setup_test_app(vec![], false).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/admin/keys", url))
        .json(&serde_json::json!({"name": "test-key", "scopes": ["admin"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["key"].as_str().unwrap().starts_with("sk-"));
    assert_eq!(body["name"], "test-key");
    h.abort();
}

#[tokio::test]
async fn test_admin_list_keys() {
    let (url, h) = setup_test_app(vec![], false).await;
    let client = reqwest::Client::new();

    // Create 3 keys
    for i in 0..3 {
        client
            .post(format!("{}/admin/keys", url))
            .json(&serde_json::json!({"name": format!("key-{}", i), "scopes": ["admin"]}))
            .send()
            .await
            .unwrap();
    }

    let resp = client
        .get(format!("{}/admin/keys", url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body.as_array().unwrap().len(), 3);
    h.abort();
}

#[tokio::test]
async fn test_admin_get_key() {
    let (url, h) = setup_test_app(vec![], false).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{}/admin/keys", url))
        .json(&serde_json::json!({"name": "test", "scopes": ["admin"]}))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let key_id = body["id"].as_str().unwrap();

    let resp = client
        .get(format!("{}/admin/keys/{}", url, key_id))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let fetched: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(fetched["id"], key_id);
    h.abort();
}

#[tokio::test]
async fn test_admin_delete_key() {
    let (url, h) = setup_test_app(vec![], false).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{}/admin/keys", url))
        .json(&serde_json::json!({"name": "to-delete", "scopes": ["admin"]}))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let key_id = body["id"].as_str().unwrap();

    let resp = client
        .delete(format!("{}/admin/keys/{}", url, key_id))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    let resp = client
        .get(format!("{}/admin/keys/{}", url, key_id))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    h.abort();
}

#[tokio::test]
async fn test_admin_revoke_key() {
    let (url, h) = setup_test_app(vec![], false).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{}/admin/keys", url))
        .json(&serde_json::json!({"name": "to-revoke", "scopes": ["admin"]}))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let key_id = body["id"].as_str().unwrap();

    let resp = client
        .post(format!("{}/admin/keys/{}/revoke", url, key_id))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let resp = client
        .get(format!("{}/admin/keys/{}", url, key_id))
        .send()
        .await
        .unwrap();
    let fetched: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(fetched["enabled"], false);
    h.abort();
}

// ═══════════════════════════════════════════════════════════════════════
// E2E TESTS — Multi-Provider Routing
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_multi_provider_routing() {
    let mock1 = Arc::new(MockProvider::new("openai", "OpenAI response"));
    let mock2 = Arc::new(MockProvider::new("anthropic", "Anthropic response"));
    let (url, h) = setup_test_app(vec![mock1.clone(), mock2.clone()], false).await;

    let client = reqwest::Client::new();

    // Both requests should route to first provider (Single strategy)
    let resp = client
        .post(format!("{}/v1/chat/completions", url))
        .json(&test_request("mock-openai"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["choices"][0]["message"]["content"]
        .as_str()
        .unwrap()
        .contains("OpenAI"));

    let resp = client
        .post(format!("{}/v1/chat/completions", url))
        .json(&test_request("mock-anthropic"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["choices"][0]["message"]["content"]
        .as_str()
        .unwrap()
        .contains("OpenAI"));

    // First provider handles all requests (Single strategy)
    assert_eq!(mock1.call_count(), 2);
    assert_eq!(mock2.call_count(), 0);
    h.abort();
}

// ═══════════════════════════════════════════════════════════════════════
// E2E TESTS — Error Handling
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_invalid_json_request() {
    let (url, h) = setup_test_app(vec![], false).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", url))
        .header("content-type", "application/json")
        .body("not json")
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_client_error());
    h.abort();
}

#[tokio::test]
async fn test_missing_model_field() {
    let (url, h) = setup_test_app(vec![], false).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", url))
        .json(&serde_json::json!({"messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_client_error());
    h.abort();
}

#[tokio::test]
async fn test_empty_messages() {
    let mock = Arc::new(MockProvider::new("openai", "ok"));
    let (url, h) = setup_test_app(vec![mock], false).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", url))
        .json(&serde_json::json!({"model": "mock-openai", "messages": []}))
        .send()
        .await
        .unwrap();
    // Should either succeed or return a validation error
    assert!(resp.status().is_success() || resp.status().is_client_error());
    h.abort();
}

#[tokio::test]
async fn test_nonexistent_endpoint() {
    let (url, h) = setup_test_app(vec![], false).await;
    let resp = reqwest::get(format!("{}/nonexistent", url)).await.unwrap();
    assert_eq!(resp.status(), 404);
    h.abort();
}

// ═══════════════════════════════════════════════════════════════════════
// E2E TESTS — Streaming Edge Cases
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_streaming_error_mid_stream() {
    let mock = Arc::new(MockProvider::new("openai", "Partial response"));
    let (url, h) = setup_test_app(vec![mock], false).await;
    let client = reqwest::Client::new();

    // Request with error model should fail before streaming starts.
    // Upstream 5xx maps to 503 (upstream unavailable), not 500.
    let resp = client
        .post(format!("{}/v1/chat/completions", url))
        .json(&test_stream_request("error-model"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);
    h.abort();
}

#[tokio::test]
async fn test_streaming_multiple_words() {
    let mock = Arc::new(MockProvider::new("openai", "one two three four five"));
    let (url, h) = setup_test_app(vec![mock.clone()], false).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/chat/completions", url))
        .json(&test_stream_request("mock-openai"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    // Should contain all words
    assert!(body.contains("one"));
    assert!(body.contains("two"));
    assert!(body.contains("three"));
    assert!(body.contains("four"));
    assert!(body.contains("five"));
    assert_eq!(mock.call_count(), 1);
    h.abort();
}

// ═══════════════════════════════════════════════════════════════════════
// E2E TESTS — Plugin Integration
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_word_filter_rejects_blocked_content() {
    let plugin = himadri_plugins::WordFilterPlugin::new(vec!["blocked".to_string()]);
    let mut ctx = himadri_plugin::PluginContext::from_request(
        &ChatCompletionRequest {
            model: "test".to_string(),
            messages: vec![Message {
                role: Role::User,
                content: Some(MessageContent::Text("This has a blocked word".to_string())),
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
        },
        None,
    );
    assert!(plugin.execute(&mut ctx).await.is_err());
}

#[tokio::test]
async fn test_max_token_rejects_over_limit() {
    let plugin = himadri_plugins::MaxTokenPlugin::new(100);
    let mut ctx = himadri_plugin::PluginContext::from_request(
        &ChatCompletionRequest {
            model: "test".to_string(),
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
            max_tokens: Some(200),
            stop: None,
            presence_penalty: None,
            frequency_penalty: None,
            user: None,
            tools: None,
            tool_choice: None,
            extra: Default::default(),
        },
        None,
    );
    assert!(plugin.execute(&mut ctx).await.is_err());
}

// ═══════════════════════════════════════════════════════════════════════
// E2E TESTS — Infrastructure
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_config_validation() {
    let mut config = himadri_core::Config::default();
    config.targets.clear();
    assert!(config.validate().is_err());
}

#[tokio::test]
async fn test_rate_limiter() {
    use himadri_ratelimit::TokenBucket;
    let bucket = TokenBucket::new(10, 10);
    for _ in 0..10 {
        assert!(bucket.allow());
    }
    assert!(!bucket.allow());
}

#[tokio::test]
async fn test_circuit_breaker() {
    use himadri_circuitbreaker::{CircuitBreaker, CircuitBreakerConfig};
    use std::time::Duration;
    let cb = CircuitBreaker::new(CircuitBreakerConfig {
        failure_threshold: 3,
        recovery_timeout: Duration::from_millis(10),
        half_open_max_calls: 2,
    });
    cb.record_failure();
    cb.record_failure();
    cb.record_failure();
    assert!(!cb.allow());
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(cb.allow());
    cb.record_success();
    assert!(cb.allow());
    cb.record_success();
    assert!(cb.allow());
    cb.record_success();
    assert!(cb.allow());
}

#[tokio::test]
async fn test_store_crud() {
    let store = StoreBackend::new().await;

    // Create
    let key = store
        .create(himadri_admin::CreateApiKeyRequest {
            name: "test".into(),
            scopes: vec!["admin".into()],
            expires_at: None,
            metadata: None,
            org_id: Some("org-1".into()),
            team_id: Some("team-1".into()),
            user_id: None,
            models: None,
            rate_limit_override: None,
            token_budget: None,
        })
        .await
        .unwrap();
    assert!(store.validate(&key.key).await.unwrap().is_some());
    assert_eq!(key.org_id, Some("org-1".into()));

    // Get
    let fetched = store.get(&key.id).await.unwrap().unwrap();
    assert_eq!(fetched.name, "test");

    // List
    let list = store.list().await.unwrap();
    assert_eq!(list.len(), 1);

    // Delete
    assert!(store.delete(&key.id).await.unwrap());
    assert!(store.validate(&key.key).await.unwrap().is_none());
}

#[tokio::test]
async fn test_auth_context_rate_limit() {
    use himadri_auth::JwtClaims;

    let claims = JwtClaims {
        sub: "user123".to_string(),
        iss: "https://example.com".to_string(),
        aud: "client123".to_string(),
        exp: 9999999999,
        iat: 0,
        nbf: None,
        jti: None,
        scope: Some("admin".to_string()),
        org_id: Some("org-1".to_string()),
        team_id: None,
        email: None,
        email_verified: None,
        roles: None,
        rate_limit_rpm: Some(600),
        budget_limit_usd: Some(50.0),
        custom: std::collections::HashMap::new(),
    };

    let auth_ctx = claims.into_auth_context();
    let rl = auth_ctx.rate_limit_override.unwrap();
    assert_eq!(rl.requests_per_second, Some(10)); // 600/60
    assert_eq!(rl.burst_size, Some(600));
}
