//! Use-case-driven end-to-end tests covering the admin/gateway surface added
//! and fixed across recent sprints: RBAC, per-principal budgets, provider
//! failover, response caching, the model/endpoint admin CRUD API (SQLite and
//! Postgres), endpoint-API-key encryption at rest, and the SQLite timestamp
//! bug fix (see `sqlite_time`/`parse_sqlite_timestamp`).
//!
//! Group A drives `Gateway` directly (no HTTP), following the precedent in
//! `feature_tests.rs`. Group B spins up a real axum server backed by a
//! throwaway SQLite file so the admin HTTP API and its on-disk persistence
//! are both actually exercised. Group C does the same against a live
//! Postgres, skipped unless `TEST_POSTGRES_URL` is set.

mod mock_provider;

use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use tokio::net::TcpListener;

use himadri_admin::{
    AdminHandlers, ApiKey, CipherKey, CreateApiKeyRequest, CreateModelEndpointRequest,
    CreateModelRequest, Model, StoreBackend, UpdateApiKeyRequest, UpdateModelRequest,
};
use himadri_core::{
    AuthContext, AuthScope, ChatCompletionRequest, Config, GatewayError, Message, MessageContent,
    RbacConfig, Role, RolePolicy, StrategyConfig, StrategyMode, Target,
};
use himadri_plugin::PluginManager;
use himadri_plugins::{BudgetConfig, BudgetPlugin, ResponseCachePlugin};

use mock_provider::MockProvider;

fn metrics() -> Arc<himadri_observability::Metrics> {
    Arc::new(himadri_observability::Metrics::new())
}

fn target(provider: &str) -> Target {
    Target {
        provider: provider.to_string(),
        weight: 1.0,
        models: None,
        id: None,
        api_key_env: None,
        base_url: None,
    }
}

fn request(model: &str) -> ChatCompletionRequest {
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

fn auth_ctx(api_key: &str, roles: Vec<String>, scope: AuthScope) -> AuthContext {
    AuthContext {
        api_key: api_key.to_string(),
        // Real auth flows (API-key store, JWT) always set a stable key_id;
        // budget/rate-limit tracking is keyed by it, never by the raw secret.
        key_id: Some(api_key.to_string()),
        scope,
        org_id: None,
        team_id: None,
        user_id: None,
        rate_limit_override: None,
        roles,
        budget_limit_usd: None,
    }
}

// ═══════════════════════════════════════════════════════════════════════
// GROUP A — Gateway-driven use cases (RBAC, budgets, failover, cache)
// ═══════════════════════════════════════════════════════════════════════

/// Use case: a "free" tier principal may only use the model their role
/// allows-listed; requesting any other model is denied.
#[tokio::test]
async fn rbac_denies_model_not_in_role_policy() {
    let mock = Arc::new(MockProvider::new("openai", "hi"));
    let mut rbac = RbacConfig {
        enabled: true,
        ..Default::default()
    };
    rbac.roles.insert(
        "free".to_string(),
        RolePolicy {
            models: Some(vec!["mock-openai".to_string()]),
            providers: None,
        },
    );
    let config = Config {
        targets: vec![target("openai")],
        rbac,
        ..Default::default()
    };
    let gateway = himadri::Gateway::new(config, metrics());
    gateway.register_provider(mock.clone());

    let auth = auth_ctx("k1", vec!["free".to_string()], AuthScope::ApiKey);
    let result = gateway
        .route(request("mock-openai-large"), Some(&auth), None)
        .await;
    assert!(matches!(result, Err(GatewayError::Forbidden(_))));
    assert_eq!(
        mock.call_count(),
        0,
        "provider must not be called when RBAC denies the model"
    );
}

/// Use case: the same "free" tier principal succeeds for the model their
/// role does allow.
#[tokio::test]
async fn rbac_allows_model_in_role_policy() {
    let mock = Arc::new(MockProvider::new("openai", "hi"));
    let mut rbac = RbacConfig {
        enabled: true,
        ..Default::default()
    };
    rbac.roles.insert(
        "free".to_string(),
        RolePolicy {
            models: Some(vec!["mock-openai".to_string()]),
            providers: None,
        },
    );
    let config = Config {
        targets: vec![target("openai")],
        rbac,
        ..Default::default()
    };
    let gateway = himadri::Gateway::new(config, metrics());
    gateway.register_provider(mock.clone());

    let auth = auth_ctx("k1", vec!["free".to_string()], AuthScope::ApiKey);
    let result = gateway
        .route(request("mock-openai"), Some(&auth), None)
        .await;
    assert!(
        result.is_ok(),
        "expected allowed model to succeed: {:?}",
        result.err()
    );
    assert_eq!(mock.call_count(), 1);
}

/// Use case: an admin-scoped principal bypasses RBAC entirely, even for a
/// model no role explicitly allow-lists.
#[tokio::test]
async fn rbac_admin_scope_bypasses_restrictions() {
    let mock = Arc::new(MockProvider::new("openai", "hi"));
    let mut rbac = RbacConfig {
        enabled: true,
        ..Default::default()
    };
    rbac.roles.insert(
        "free".to_string(),
        RolePolicy {
            models: Some(vec!["mock-openai".to_string()]),
            providers: None,
        },
    );
    let config = Config {
        targets: vec![target("openai")],
        rbac,
        ..Default::default()
    };
    let gateway = himadri::Gateway::new(config, metrics());
    gateway.register_provider(mock.clone());

    let admin = auth_ctx("admin-key", vec![], AuthScope::Admin);
    let result = gateway
        .route(request("mock-openai-large"), Some(&admin), None)
        .await;
    assert!(
        result.is_ok(),
        "admin scope should bypass RBAC: {:?}",
        result.err()
    );
}

/// Use case: a principal whose roles don't match any policy falls back to
/// `default_role`'s policy rather than being denied outright.
#[tokio::test]
async fn rbac_default_role_applies_when_no_role_matches() {
    let mock = Arc::new(MockProvider::new("openai", "hi"));
    let mut rbac = RbacConfig {
        enabled: true,
        default_role: Some("free".to_string()),
        ..Default::default()
    };
    rbac.roles.insert(
        "free".to_string(),
        RolePolicy {
            models: Some(vec!["mock-openai".to_string()]),
            providers: None,
        },
    );
    let config = Config {
        targets: vec![target("openai")],
        rbac,
        ..Default::default()
    };
    let gateway = himadri::Gateway::new(config, metrics());
    gateway.register_provider(mock.clone());

    // No roles at all — an API-key principal with no tier — should still
    // resolve via default_role rather than being denied for "no matching role".
    let auth = auth_ctx("k1", vec![], AuthScope::ApiKey);
    let ok = gateway
        .route(request("mock-openai"), Some(&auth), None)
        .await;
    assert!(
        ok.is_ok(),
        "default_role should grant its policy: {:?}",
        ok.err()
    );

    let denied = gateway
        .route(request("mock-openai-large"), Some(&auth), None)
        .await;
    assert!(matches!(denied, Err(GatewayError::Forbidden(_))));
}

fn budget_plugin_manager(store_id: &str, spend_limit_usd: f64) -> PluginManager {
    let plugin = BudgetPlugin::new(BudgetConfig {
        store_id: Some(store_id.to_string()),
        spend_limit_usd: Some(spend_limit_usd),
        input_per_m_tokens: Some(100.0),
        output_per_m_tokens: Some(100.0),
        max_keys: None,
    })
    .unwrap();
    let mut manager = PluginManager::new();
    manager.register(plugin);
    manager
}

/// Use case: once a key's accumulated spend reaches its budget cap, further
/// requests from that key are rejected — mirroring a customer who's used up
/// their monthly allotment. Each mock completion costs a fixed
/// (10 prompt + 20 completion tokens) * $100/M = $0.003, so a $0.005 cap
/// allows two calls and rejects the third.
#[tokio::test]
async fn budget_blocks_after_limit_exceeded() {
    let mock = Arc::new(MockProvider::new("openai", "hi"));
    let config = Config {
        targets: vec![target("openai")],
        ..Default::default()
    };
    let mut gateway = himadri::Gateway::new(config, metrics());
    gateway.register_provider(mock.clone());
    gateway.set_plugin_manager(budget_plugin_manager("budget-test-block", 0.005));

    let auth = auth_ctx("budget-key-1", vec![], AuthScope::ApiKey);
    for i in 0..2 {
        let r = gateway
            .route(request("mock-openai"), Some(&auth), None)
            .await;
        assert!(r.is_ok(), "call {i} should be within budget: {:?}", r.err());
    }
    let third = gateway
        .route(request("mock-openai"), Some(&auth), None)
        .await;
    assert!(third.is_err(), "third call should exceed the $0.005 budget");
    assert_eq!(
        mock.call_count(),
        2,
        "the rejected call must not reach the provider"
    );
}

/// Use case: two different API keys sharing a gateway have independent
/// budgets — one customer maxing out their spend must not affect another's.
#[tokio::test]
async fn budget_tracks_keys_independently() {
    let mock = Arc::new(MockProvider::new("openai", "hi"));
    let config = Config {
        targets: vec![target("openai")],
        ..Default::default()
    };
    let mut gateway = himadri::Gateway::new(config, metrics());
    gateway.register_provider(mock.clone());
    gateway.set_plugin_manager(budget_plugin_manager("budget-test-isolated", 0.005));

    let key_a = auth_ctx("budget-key-a", vec![], AuthScope::ApiKey);
    let key_b = auth_ctx("budget-key-b", vec![], AuthScope::ApiKey);

    // Exhaust key A's budget.
    for _ in 0..2 {
        gateway
            .route(request("mock-openai"), Some(&key_a), None)
            .await
            .unwrap();
    }
    assert!(gateway
        .route(request("mock-openai"), Some(&key_a), None)
        .await
        .is_err());

    // Key B should be unaffected.
    let b_result = gateway
        .route(request("mock-openai"), Some(&key_b), None)
        .await;
    assert!(
        b_result.is_ok(),
        "key B's own budget should be untouched: {:?}",
        b_result.err()
    );
}

/// Use case: the primary provider is down; the gateway fails over to the
/// secondary rather than surfacing an error to the caller.
#[tokio::test]
async fn fallback_strategy_retries_next_provider_on_failure() {
    let primary = Arc::new(MockProvider::new("openai", "primary"));
    let secondary = Arc::new(MockProvider::new("anthropic", "secondary"));
    let config = Config {
        targets: vec![target("openai"), target("anthropic")],
        strategy: StrategyConfig {
            mode: StrategyMode::Fallback,
            ..Default::default()
        },
        ..Default::default()
    };
    let gateway = himadri::Gateway::new(config, metrics());
    gateway.register_provider(primary.clone());
    gateway.register_provider(secondary.clone());

    // Only specific statuses are retryable (502/503/529, plus RateLimited —
    // see `ProviderError::retryable`); a generic 500 intentionally is not.
    // "rate-limit" makes MockProvider return a retryable `RateLimited` error
    // regardless of which provider handles it, so both targets get tried.
    let result = gateway.route(request("mock-rate-limit"), None, None).await;
    assert!(
        result.is_err(),
        "both providers reject, so the overall request still fails"
    );
    assert_eq!(primary.call_count(), 1);
    assert_eq!(
        secondary.call_count(),
        1,
        "fallback must retry the next target on a retryable failure"
    );
}

/// Use case: identical requests within the cache TTL are served from cache,
/// saving a redundant (billable) call to the upstream provider.
#[tokio::test]
async fn response_cache_avoids_duplicate_provider_call() {
    let mock = Arc::new(MockProvider::new("openai", "cached response"));
    let config = Config {
        targets: vec![target("openai")],
        ..Default::default()
    };
    let mut gateway = himadri::Gateway::new(config, metrics());
    gateway.register_provider(mock.clone());
    gateway.set_response_cache(ResponseCachePlugin::new(100, Duration::from_secs(60)));

    let first = gateway.route(request("mock-openai"), None, None).await;
    assert!(first.is_ok());
    let second = gateway.route(request("mock-openai"), None, None).await;
    assert!(second.is_ok());

    assert_eq!(
        mock.call_count(),
        1,
        "second identical request should be served from cache"
    );
    assert_eq!(
        first.unwrap().choices[0].message.content,
        second.unwrap().choices[0].message.content
    );
}

/// Use case: a provider with no native embeddings support (the mock, like
/// several real providers per GAP_ANALYSIS.md) surfaces a clear error rather
/// than silently returning nonsense.
#[tokio::test]
async fn embeddings_unsupported_provider_returns_error() {
    let mock = Arc::new(MockProvider::new("openai", "n/a"));
    let config = Config {
        targets: vec![target("openai")],
        ..Default::default()
    };
    let gateway = himadri::Gateway::new(config, metrics());
    gateway.register_provider(mock);

    let req = himadri_core::EmbeddingRequest {
        model: "mock-openai".to_string(),
        input: himadri_core::EmbeddingInput::Single("hello world".to_string()),
        encoding_format: None,
        dimensions: None,
        user: None,
        extra: Default::default(),
    };
    let result = gateway.embed(req, None).await;
    assert!(
        result.is_err(),
        "embeddings on an unsupported provider must error, not silently succeed"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// GROUP B — Admin HTTP API use cases (real SQLite file, real persistence)
// ═══════════════════════════════════════════════════════════════════════

struct TestAdminApp {
    url: String,
    handle: tokio::task::JoinHandle<()>,
    db_path: std::path::PathBuf,
}

impl Drop for TestAdminApp {
    fn drop(&mut self) {
        self.handle.abort();
        let _ = std::fs::remove_file(&self.db_path);
    }
}

#[derive(Clone)]
struct AdminAppState {
    admin: Arc<AdminHandlers>,
    gateway: Arc<himadri::Gateway>,
}

async fn setup_admin_app(cipher: Option<CipherKey>) -> TestAdminApp {
    let db_path =
        std::env::temp_dir().join(format!("himadri-usecase-test-{}.db", uuid::Uuid::new_v4()));
    let db_url = format!("sqlite://{}", db_path.display());

    let sqlite_store = himadri_admin::store::SqliteStore::new(&db_url)
        .await
        .unwrap();
    let store = StoreBackend::Sqlite(Arc::new(sqlite_store));
    let mut admin = AdminHandlers::new(store);

    let (model_store, endpoint_store) = himadri_admin::connect_model_stores(&db_url, cipher)
        .await
        .expect("sqlite model store should connect");
    admin = admin.with_model_stores(model_store, endpoint_store);

    let gateway = Arc::new(himadri::Gateway::new(Config::default(), metrics()));
    let state = AdminAppState {
        admin: Arc::new(admin),
        gateway,
    };

    let app = Router::new()
        .route("/admin/keys", get(list_keys_h).post(create_key_h))
        .route(
            "/admin/keys/{id}",
            get(get_key_h).put(update_key_h).delete(delete_key_h),
        )
        .route("/admin/keys/{id}/rotate", post(rotate_key_h))
        .route("/admin/keys/{id}/revoke", post(revoke_key_h))
        .route("/admin/models", get(list_models_h).post(create_model_h))
        .route(
            "/admin/models/{id}",
            get(get_model_h).put(update_model_h).delete(delete_model_h),
        )
        .route("/admin/models/{id}/toggle", post(toggle_model_h))
        .route("/admin/dashboard", get(dashboard_h))
        .route("/admin/config", get(get_config_h).put(update_config_h))
        .route("/admin/config/history", get(config_history_h))
        .route("/admin/config/rollback/{version}", post(config_rollback_h))
        .with_state(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{addr}");
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(30)).await;

    TestAdminApp {
        url,
        handle,
        db_path,
    }
}

async fn list_keys_h(State(s): State<AdminAppState>) -> Result<Json<Vec<ApiKey>>, StatusCode> {
    s.admin
        .list_keys()
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}
async fn create_key_h(
    State(s): State<AdminAppState>,
    Json(req): Json<CreateApiKeyRequest>,
) -> Result<(StatusCode, Json<ApiKey>), (StatusCode, String)> {
    s.admin
        .create_key(req)
        .await
        .map(|k| (StatusCode::CREATED, Json(k)))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}
async fn get_key_h(
    State(s): State<AdminAppState>,
    Path(id): Path<String>,
) -> Result<Json<ApiKey>, StatusCode> {
    s.admin
        .get_key(&id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}
async fn update_key_h(
    State(s): State<AdminAppState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateApiKeyRequest>,
) -> Result<Json<ApiKey>, StatusCode> {
    s.admin
        .update_key(&id, req)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}
async fn delete_key_h(State(s): State<AdminAppState>, Path(id): Path<String>) -> StatusCode {
    match s.admin.delete_key(&id).await {
        Ok(true) => StatusCode::NO_CONTENT,
        Ok(false) => StatusCode::NOT_FOUND,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
async fn rotate_key_h(
    State(s): State<AdminAppState>,
    Path(id): Path<String>,
) -> Result<Json<ApiKey>, StatusCode> {
    s.admin
        .rotate_key(&id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}
async fn revoke_key_h(State(s): State<AdminAppState>, Path(id): Path<String>) -> StatusCode {
    match s.admin.revoke_key(&id).await {
        Ok(true) => StatusCode::OK,
        Ok(false) => StatusCode::NOT_FOUND,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

async fn list_models_h(State(s): State<AdminAppState>) -> Result<Json<Vec<Model>>, StatusCode> {
    s.admin
        .list_models()
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}
async fn create_model_h(
    State(s): State<AdminAppState>,
    Json(req): Json<CreateModelRequest>,
) -> Result<(StatusCode, Json<Model>), (StatusCode, String)> {
    s.admin
        .create_model(req)
        .await
        .map(|m| (StatusCode::CREATED, Json(m)))
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))
}
async fn get_model_h(
    State(s): State<AdminAppState>,
    Path(id): Path<String>,
) -> Result<Json<Model>, StatusCode> {
    s.admin
        .get_model(&id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}
async fn update_model_h(
    State(s): State<AdminAppState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateModelRequest>,
) -> Result<Json<Model>, StatusCode> {
    s.admin
        .update_model(&id, req)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}
async fn delete_model_h(
    State(s): State<AdminAppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    match s.admin.delete_model(&id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err((StatusCode::NOT_FOUND, "not found".to_string())),
        // Mirror the real handlers' mapping: guard conflicts are 409, store
        // failures 500.
        Err(himadri_admin::AdminError::Conflict(m)) => Err((StatusCode::CONFLICT, m)),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}
async fn toggle_model_h(
    State(s): State<AdminAppState>,
    Path(id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<Model>, (StatusCode, String)> {
    let enabled = body["enabled"].as_bool().unwrap_or(true);
    s.admin
        .toggle_model(&id, enabled)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .ok_or((StatusCode::NOT_FOUND, "not found".to_string()))
}

async fn dashboard_h(State(s): State<AdminAppState>) -> Json<serde_json::Value> {
    let key_count = s.admin.list_keys().await.map_or(0, |k| k.len());
    let dashboard = s.gateway.usage_store().get_dashboard(key_count);
    Json(serde_json::json!({ "total_keys": key_count, "dashboard": dashboard }))
}

async fn get_config_h(State(s): State<AdminAppState>) -> Json<Config> {
    Json(s.gateway.get_config().await)
}
async fn update_config_h(
    State(s): State<AdminAppState>,
    Json(cfg): Json<Config>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    s.gateway
        .reload_config(cfg)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(Json(serde_json::json!({ "status": "updated" })))
}
async fn config_history_h(State(s): State<AdminAppState>) -> Json<serde_json::Value> {
    let history = s.gateway.config_history().await;
    Json(serde_json::json!({ "data": history, "total_versions": history.len() }))
}
async fn config_rollback_h(
    State(s): State<AdminAppState>,
    Path(version): Path<u32>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    s.gateway
        .rollback_config(version)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(Json(serde_json::json!({ "status": "rolled_back" })))
}

/// Use case: an operator onboards a model (first-party, no provider needed),
/// enables/disables it, and confirms deletion is blocked while it's live but
/// succeeds once disabled.
#[tokio::test]
async fn model_full_crud_lifecycle() {
    let app = setup_admin_app(None).await;
    let client = reqwest::Client::new();

    let model: Model = client
        .post(format!("{}/admin/models", app.url))
        .json(&CreateModelRequest {
            name: "gpt-z".to_string(),
            display_name: Some("GPT Z".to_string()),
            enabled: true,
        })
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(model.enabled);

    // Enabled — delete must be blocked.
    let blocked = client
        .delete(format!("{}/admin/models/{}", app.url, model.id))
        .send()
        .await
        .unwrap();
    assert_eq!(blocked.status(), 409);

    // Disable, then delete should succeed.
    let disabled: Model = client
        .post(format!("{}/admin/models/{}/toggle", app.url, model.id))
        .json(&serde_json::json!({"enabled": false}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!disabled.enabled);

    let deleted = client
        .delete(format!("{}/admin/models/{}", app.url, model.id))
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), 204);
}

/// Use case: a model is inactive until it has an enabled endpoint. Onboarding a
/// model exposes nothing on `/v1/models`; attaching a provider endpoint
/// activates it (with the endpoint's provider type as `owned_by` and its key
/// round-tripping decrypted); disabling the endpoint deactivates it again.
#[tokio::test]
async fn model_endpoint_lifecycle_controls_activation() {
    let db_path =
        std::env::temp_dir().join(format!("himadri-endpoint-test-{}.db", uuid::Uuid::new_v4()));
    let db_url = format!("sqlite://{}", db_path.display());
    // A 32-byte key (base64) so the endpoint api_key is encrypted at rest and
    // must round-trip decrypted through the store.
    let cipher = CipherKey::from_base64("MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=").unwrap();

    let (model_store, endpoint_store) = himadri_admin::connect_model_stores(&db_url, Some(cipher))
        .await
        .expect("sqlite stores should connect");
    let admin = AdminHandlers::new(StoreBackend::new().await)
        .with_model_stores(model_store, endpoint_store);

    // Onboard a model — no endpoints yet, so it's inactive (absent from the
    // OpenAI-facing model list).
    let model = admin
        .create_model(CreateModelRequest {
            name: "gpt-z".to_string(),
            display_name: Some("GPT Z".to_string()),
            enabled: true,
        })
        .await
        .expect("model created");
    assert!(admin
        .list_enabled_models_for_api()
        .await
        .unwrap()
        .is_empty());

    // Attach a provider endpoint — the model becomes active.
    let ep = admin
        .create_endpoint(
            &model.id,
            CreateModelEndpointRequest {
                provider_type: "openai".to_string(),
                base_url: Some("https://api.openai.com/v1".to_string()),
                api_key: Some("sk-secret".to_string()),
                weight: 1.0,
                enabled: true,
            },
        )
        .await
        .expect("endpoint created");

    let active = admin.list_enabled_models_for_api().await.unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id, "gpt-z");
    assert_eq!(active[0].owned_by, "openai");

    // The endpoint's key round-trips decrypted through the store.
    let fetched = admin
        .get_endpoint(&ep.id)
        .await
        .unwrap()
        .expect("endpoint exists");
    assert_eq!(fetched.api_key.as_deref(), Some("sk-secret"));

    // Disable the endpoint — the model is inactive again.
    admin.toggle_endpoint(&ep.id, false).await.expect("toggled");
    assert!(admin
        .list_enabled_models_for_api()
        .await
        .unwrap()
        .is_empty());

    // Regression: an enabled endpoint that routing would skip (unknown
    // provider type, no base_url) must not activate the model either —
    // otherwise `/v1/models` advertises a model whose completions 404.
    admin
        .create_endpoint(
            &model.id,
            CreateModelEndpointRequest {
                provider_type: "mystery-vendor".to_string(),
                base_url: None,
                api_key: Some("sk-other".to_string()),
                weight: 1.0,
                enabled: true,
            },
        )
        .await
        .expect("endpoint created");
    assert!(admin
        .list_enabled_models_for_api()
        .await
        .unwrap()
        .is_empty());

    let _ = std::fs::remove_file(&db_path);
}

/// Regression test: SQLite-backed API keys must record a real creation
/// timestamp, exercised through the same `AdminHandlers` path `main.rs` now
/// uses uniformly for keys (previously it bypassed `AdminHandlers` and hit
/// `state.store` directly).
#[tokio::test]
async fn api_key_created_at_is_real_timestamp_not_epoch() {
    let app = setup_admin_app(None).await;
    let client = reqwest::Client::new();
    let before = chrono::Utc::now();

    let created: ApiKey = client
        .post(format!("{}/admin/keys", app.url))
        .json(&serde_json::json!({"name": "ts-key", "scopes": ["api"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(
        created.created_at > before - chrono::Duration::seconds(5),
        "created_at should be ~now, got {}",
        created.created_at
    );
}

/// Use case: the full API-key lifecycle an admin panel drives — create,
/// fetch, edit scopes, rotate the secret, revoke, then delete — all through
/// the single `AdminHandlers`-backed code path.
#[tokio::test]
async fn api_key_full_lifecycle_via_admin_handlers() {
    let app = setup_admin_app(None).await;
    let client = reqwest::Client::new();

    let created: ApiKey = client
        .post(format!("{}/admin/keys", app.url))
        .json(&serde_json::json!({"name": "lifecycle-key", "scopes": ["api"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let original_secret = created.key.clone();

    let fetched: ApiKey = client
        .get(format!("{}/admin/keys/{}", app.url, created.id))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(fetched.id, created.id);

    let updated: ApiKey = client
        .put(format!("{}/admin/keys/{}", app.url, created.id))
        .json(&serde_json::json!({"scopes": ["admin"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(updated.scopes, vec!["admin".to_string()]);

    let rotated: ApiKey = client
        .post(format!("{}/admin/keys/{}/rotate", app.url, created.id))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_ne!(
        rotated.key, original_secret,
        "rotate must issue a new secret"
    );

    let revoke_status = client
        .post(format!("{}/admin/keys/{}/revoke", app.url, created.id))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(revoke_status, 200);
    let revoked: ApiKey = client
        .get(format!("{}/admin/keys/{}", app.url, created.id))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!revoked.enabled);

    let delete_status = client
        .delete(format!("{}/admin/keys/{}", app.url, created.id))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(delete_status, 204);
}

/// Regression test: the dashboard's key count must reflect keys created
/// through the admin API (previously `state.store` and `state.admin` were
/// two different, potentially-diverging code paths for keys).
#[tokio::test]
async fn dashboard_key_count_reflects_created_keys() {
    let app = setup_admin_app(None).await;
    let client = reqwest::Client::new();

    for i in 0..3 {
        client
            .post(format!("{}/admin/keys", app.url))
            .json(&serde_json::json!({"name": format!("dash-key-{i}"), "scopes": ["api"]}))
            .send()
            .await
            .unwrap();
    }

    let dashboard: serde_json::Value = client
        .get(format!("{}/admin/dashboard", app.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(dashboard["total_keys"], 3);
}

/// Use case: an operator edits the live rate-limit config and immediately
/// sees the change reflected back.
#[tokio::test]
async fn config_get_update_roundtrip() {
    let app = setup_admin_app(None).await;
    let client = reqwest::Client::new();

    let mut config: Config = client
        .get(format!("{}/admin/config", app.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!config.rate_limit.enabled);

    config.rate_limit.enabled = true;
    config.rate_limit.requests_per_second = 42;
    let update_status = client
        .put(format!("{}/admin/config", app.url))
        .json(&config)
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(update_status, 200);

    let reloaded: Config = client
        .get(format!("{}/admin/config", app.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(reloaded.rate_limit.enabled);
    assert_eq!(reloaded.rate_limit.requests_per_second, 42);
}

/// Use case: after two config edits, the operator lists history and rolls
/// back to the version before their mistake.
#[tokio::test]
async fn config_history_and_rollback() {
    let app = setup_admin_app(None).await;
    let client = reqwest::Client::new();

    let mut v1: Config = client
        .get(format!("{}/admin/config", app.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    v1.rate_limit.requests_per_second = 111;
    client
        .put(format!("{}/admin/config", app.url))
        .json(&v1)
        .send()
        .await
        .unwrap();

    let mut v2 = v1.clone();
    v2.rate_limit.requests_per_second = 222;
    client
        .put(format!("{}/admin/config", app.url))
        .json(&v2)
        .send()
        .await
        .unwrap();

    let history: serde_json::Value = client
        .get(format!("{}/admin/config/history", app.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let versions = history["data"].as_array().unwrap();
    // Gateway::new() seeds an initial "version 1" from the default config, so
    // by now there are 3 entries: the default, the 111 edit, and the 222 edit.
    assert!(
        versions.len() >= 3,
        "expected at least 3 history entries (default + 2 edits), got {}",
        versions.len()
    );

    let target_version = versions
        .iter()
        .find(|v| v["config"]["rate_limit"]["requests_per_second"] == 111)
        .expect("history should contain the version that set requests_per_second to 111")["version"]
        .as_u64()
        .unwrap() as u32;
    let rollback_status = client
        .post(format!(
            "{}/admin/config/rollback/{}",
            app.url, target_version
        ))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(rollback_status, 200);

    let after_rollback: Config = client
        .get(format!("{}/admin/config", app.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(after_rollback.rate_limit.requests_per_second, 111);
}

// ═══════════════════════════════════════════════════════════════════════
// GROUP C — Postgres parity (skipped unless TEST_POSTGRES_URL is set)
// ═══════════════════════════════════════════════════════════════════════

fn postgres_test_url() -> Option<String> {
    std::env::var("TEST_POSTGRES_URL").ok()
}

/// Use case: API-key update semantics must match SQLite — partial updates
/// keep unspecified fields, `Some(None)` clears nullable fields, and
/// budget/model restrictions round-trip.
#[tokio::test]
async fn postgres_api_key_update_parity() {
    let Some(url) = postgres_test_url() else {
        eprintln!("skipping postgres_api_key_update_parity: TEST_POSTGRES_URL not set");
        return;
    };
    let store = himadri_admin::PostgresStore::new(&url)
        .await
        .expect("postgres api-key store should connect");

    let created = store
        .create(himadri_admin::CreateApiKeyRequest {
            name: "pg-parity-key".to_string(),
            scopes: vec!["chat".to_string()],
            expires_at: None,
            metadata: None,
            org_id: Some("org-1".to_string()),
            team_id: None,
            user_id: None,
            models: None,
            rate_limit_override: None,
            token_budget: None,
        })
        .await
        .expect("create should succeed");

    // Partial update: set models + budget, leave the rest untouched.
    let updated = store
        .update(
            &created.id,
            himadri_admin::UpdateApiKeyRequest {
                models: Some(Some(vec!["gpt-4o".to_string()])),
                token_budget: Some(Some(himadri_admin::TokenBudget {
                    max_tokens_per_request: Some(1000),
                    max_tokens_per_day: None,
                    max_tokens_per_month: None,
                    cost_limit_per_day: None,
                    cost_limit_per_month: None,
                })),
                ..Default::default()
            },
        )
        .await
        .expect("update should succeed")
        .expect("key should exist");
    assert_eq!(updated.name, "pg-parity-key");
    assert_eq!(updated.org_id.as_deref(), Some("org-1"));
    assert_eq!(updated.models, Some(vec!["gpt-4o".to_string()]));
    assert_eq!(
        updated
            .token_budget
            .as_ref()
            .and_then(|b| b.max_tokens_per_request),
        Some(1000)
    );

    // `Some(None)` clears a nullable field; others remain.
    let cleared = store
        .update(
            &created.id,
            himadri_admin::UpdateApiKeyRequest {
                org_id: Some(None),
                ..Default::default()
            },
        )
        .await
        .expect("update should succeed")
        .expect("key should exist");
    assert_eq!(cleared.org_id, None);
    assert_eq!(cleared.models, Some(vec!["gpt-4o".to_string()]));

    // Missing id → Ok(None), matching the previous dynamic-update behavior.
    let missing = store
        .update(
            &uuid::Uuid::new_v4().to_string(),
            himadri_admin::UpdateApiKeyRequest::default(),
        )
        .await
        .expect("update of missing id should not error");
    assert!(missing.is_none());

    assert!(store.delete(&created.id).await.expect("delete"));
}

/// Use case: filtered request-log queries must actually apply their filters on
/// Postgres. The `list`/`delete` builders previously emitted `$N` placeholders
/// but never bound the values, so any filtered query errored at runtime.
///
/// Uses a multi-thread runtime because `RequestLogStore` methods are sync and
/// `block_in_place` internally — as they do under the real server runtime.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn postgres_request_log_filters_are_bound() {
    use himadri_admin::{
        MaintenanceQuery, PostgresRequestLogStore, RequestLogEntry, RequestLogQuery,
        RequestLogStore,
    };

    let Some(url) = postgres_test_url() else {
        eprintln!("skipping postgres_request_log_filters_are_bound: TEST_POSTGRES_URL not set");
        return;
    };
    let store = PostgresRequestLogStore::new(&url)
        .await
        .expect("postgres request-log store should connect");

    let tag = uuid::Uuid::new_v4().to_string();
    let entry = |provider: &str| RequestLogEntry {
        trace_id: uuid::Uuid::new_v4().to_string(),
        stage: "completed".to_string(),
        model: tag.clone(), // unique per test run, so filters isolate our rows
        provider: provider.to_string(),
        prompt_tokens: 1,
        completion_tokens: 1,
        total_tokens: 2,
        error_message: None,
        created_at: chrono::Utc::now(),
    };
    store.write(entry("openai")).unwrap();
    store.write(entry("openai")).unwrap();
    store.write(entry("anthropic")).unwrap();

    // `write` enqueues to a background flusher; poll until all three rows
    // are visible before asserting on filters, or the test races its own
    // writes.
    let all = RequestLogQuery {
        model: Some(tag.clone()),
        ..Default::default()
    };
    for _ in 0..100 {
        if store.list(all.clone()).map(|r| r.total).unwrap_or(0) >= 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        store.list(all).expect("list should succeed").total,
        3,
        "all writes must flush"
    );

    // Filtered list: model + provider must both apply (previously errored).
    let listed = store
        .list(RequestLogQuery {
            model: Some(tag.clone()),
            provider: Some("openai".to_string()),
            ..Default::default()
        })
        .expect("filtered list should succeed");
    assert_eq!(listed.total, 2);
    assert!(listed.data.iter().all(|e| e.provider == "openai"));

    // Filtered delete removes only the matching subset.
    let deleted = store
        .delete(MaintenanceQuery {
            model: Some(tag.clone()),
            provider: Some("openai".to_string()),
            ..Default::default()
        })
        .expect("filtered delete should succeed");
    assert_eq!(deleted, 2);

    // The anthropic row for this tag survives.
    let remaining = store
        .list(RequestLogQuery {
            model: Some(tag.clone()),
            ..Default::default()
        })
        .expect("list should succeed");
    assert_eq!(remaining.total, 1);
    assert_eq!(remaining.data[0].provider, "anthropic");

    // Clean up.
    store
        .delete(MaintenanceQuery {
            model: Some(tag),
            ..Default::default()
        })
        .unwrap();
}
