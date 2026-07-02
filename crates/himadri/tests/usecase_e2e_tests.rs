//! Use-case-driven end-to-end tests covering the admin/gateway surface added
//! and fixed across recent sprints: RBAC, per-principal budgets, provider
//! failover, response caching, the provider/model admin CRUD API (SQLite and
//! Postgres), provider-API-key encryption at rest, and the SQLite timestamp
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
    AdminHandlers, ApiKey, CipherKey, CreateApiKeyRequest, CreateModelRequest,
    CreateProviderRequest, Model, Provider, StoreBackend, UpdateApiKeyRequest, UpdateModelRequest,
    UpdateProviderRequest,
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
        key_id: None,
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
    let mut admin = AdminHandlers::new(store, None);

    let (provider_store, model_store) =
        himadri_admin::connect_provider_model_stores(&db_url, cipher)
            .await
            .expect("sqlite provider/model store should connect");
    admin = admin.with_provider_model_stores(provider_store, model_store);

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
        .route(
            "/admin/providers",
            get(list_providers_h).post(create_provider_h),
        )
        .route(
            "/admin/providers/{id}",
            get(get_provider_h)
                .put(update_provider_h)
                .delete(delete_provider_h),
        )
        .route("/admin/providers/{id}/toggle", post(toggle_provider_h))
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

async fn list_keys_h(State(s): State<AdminAppState>) -> Json<Vec<ApiKey>> {
    Json(s.admin.list_keys().await)
}
async fn create_key_h(
    State(s): State<AdminAppState>,
    Json(req): Json<CreateApiKeyRequest>,
) -> Result<(StatusCode, Json<ApiKey>), (StatusCode, String)> {
    s.admin
        .create_key(req)
        .await
        .map(|k| (StatusCode::CREATED, Json(k)))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}
async fn get_key_h(
    State(s): State<AdminAppState>,
    Path(id): Path<String>,
) -> Result<Json<ApiKey>, StatusCode> {
    s.admin
        .get_key(&id)
        .await
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
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}
async fn delete_key_h(State(s): State<AdminAppState>, Path(id): Path<String>) -> StatusCode {
    if s.admin.delete_key(&id).await {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
}
async fn rotate_key_h(
    State(s): State<AdminAppState>,
    Path(id): Path<String>,
) -> Result<Json<ApiKey>, StatusCode> {
    s.admin
        .rotate_key(&id)
        .await
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}
async fn revoke_key_h(State(s): State<AdminAppState>, Path(id): Path<String>) -> StatusCode {
    if s.admin.revoke_key(&id).await {
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    }
}

async fn list_providers_h(State(s): State<AdminAppState>) -> Json<Vec<Provider>> {
    Json(s.admin.list_providers().await)
}
async fn create_provider_h(
    State(s): State<AdminAppState>,
    Json(req): Json<CreateProviderRequest>,
) -> Result<(StatusCode, Json<Provider>), (StatusCode, String)> {
    s.admin
        .create_provider(req)
        .await
        .map(|p| (StatusCode::CREATED, Json(p)))
        .ok_or((
            StatusCode::INTERNAL_SERVER_ERROR,
            "create failed".to_string(),
        ))
}
async fn get_provider_h(
    State(s): State<AdminAppState>,
    Path(id): Path<String>,
) -> Result<Json<Provider>, StatusCode> {
    s.admin
        .get_provider(&id)
        .await
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}
async fn update_provider_h(
    State(s): State<AdminAppState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateProviderRequest>,
) -> Result<Json<Provider>, StatusCode> {
    s.admin
        .update_provider(&id, req)
        .await
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}
async fn delete_provider_h(
    State(s): State<AdminAppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    if s.admin.delete_provider(&id).await {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((StatusCode::CONFLICT, "delete failed".to_string()))
    }
}
async fn toggle_provider_h(
    State(s): State<AdminAppState>,
    Path(id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<Provider>, (StatusCode, String)> {
    let enabled = body["enabled"].as_bool().unwrap_or(true);
    s.admin
        .toggle_provider(&id, enabled)
        .await
        .map(Json)
        .ok_or((StatusCode::CONFLICT, "toggle failed".to_string()))
}

async fn list_models_h(State(s): State<AdminAppState>) -> Json<Vec<Model>> {
    Json(s.admin.list_models().await)
}
async fn create_model_h(
    State(s): State<AdminAppState>,
    Json(req): Json<CreateModelRequest>,
) -> Result<(StatusCode, Json<Model>), (StatusCode, String)> {
    s.admin
        .create_model(req)
        .await
        .map(|m| (StatusCode::CREATED, Json(m)))
        .ok_or((StatusCode::BAD_REQUEST, "create failed".to_string()))
}
async fn get_model_h(
    State(s): State<AdminAppState>,
    Path(id): Path<String>,
) -> Result<Json<Model>, StatusCode> {
    s.admin
        .get_model(&id)
        .await
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
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}
async fn delete_model_h(
    State(s): State<AdminAppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    if s.admin.delete_model(&id).await {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((StatusCode::CONFLICT, "delete failed".to_string()))
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
        .map(Json)
        .ok_or((StatusCode::CONFLICT, "toggle failed".to_string()))
}

async fn dashboard_h(State(s): State<AdminAppState>) -> Json<serde_json::Value> {
    let key_count = s.admin.list_keys().await.len();
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

fn create_provider_req(name: &str, api_key: Option<&str>) -> CreateProviderRequest {
    CreateProviderRequest {
        name: name.to_string(),
        enabled: true,
        api_key: api_key.map(|s| s.to_string()),
        base_url: None,
        weight: 1.0,
    }
}

/// Use case: an operator registers a new upstream provider, reads it back,
/// edits its weight, and removes it once it's no longer needed.
#[tokio::test]
async fn provider_full_crud_lifecycle() {
    let app = setup_admin_app(None).await;
    let client = reqwest::Client::new();

    let created: Provider = client
        .post(format!("{}/admin/providers", app.url))
        .json(&create_provider_req("acme", Some("sk-acme")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(created.name, "acme");
    assert_eq!(created.weight, 1.0);

    let fetched: Provider = client
        .get(format!("{}/admin/providers/{}", app.url, created.id))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(fetched.id, created.id);

    let updated: Provider = client
        .put(format!("{}/admin/providers/{}", app.url, created.id))
        .json(&serde_json::json!({"weight": 5.0}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(updated.weight, 5.0);
    assert_eq!(updated.name, "acme", "unspecified fields must be preserved");

    let del_status = client
        .delete(format!("{}/admin/providers/{}", app.url, created.id))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(del_status, 204);

    let after_delete = client
        .get(format!("{}/admin/providers/{}", app.url, created.id))
        .send()
        .await
        .unwrap();
    assert_eq!(after_delete.status(), 404);
}

/// Use case: an operator tries to delete a provider that still has models
/// attached — the gateway must refuse rather than orphan the models.
#[tokio::test]
async fn provider_delete_blocked_when_models_exist() {
    let app = setup_admin_app(None).await;
    let client = reqwest::Client::new();

    let provider: Provider = client
        .post(format!("{}/admin/providers", app.url))
        .json(&create_provider_req("with-model", None))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    client
        .post(format!("{}/admin/models", app.url))
        .json(&CreateModelRequest {
            name: "gpt-x".to_string(),
            provider_id: provider.id.clone(),
            display_name: None,
            enabled: true,
        })
        .send()
        .await
        .unwrap();

    let del_status = client
        .delete(format!("{}/admin/providers/{}", app.url, provider.id))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(
        del_status, 409,
        "deleting a provider with models must be rejected"
    );
}

/// Use case: an operator tries to disable a provider that still has an
/// *enabled* model on it — disabling must be refused so no in-flight
/// routing target silently goes stale.
#[tokio::test]
async fn provider_disable_blocked_when_enabled_models_exist() {
    let app = setup_admin_app(None).await;
    let client = reqwest::Client::new();

    let provider: Provider = client
        .post(format!("{}/admin/providers", app.url))
        .json(&create_provider_req("has-enabled-model", None))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    client
        .post(format!("{}/admin/models", app.url))
        .json(&CreateModelRequest {
            name: "gpt-y".to_string(),
            provider_id: provider.id.clone(),
            display_name: None,
            enabled: true,
        })
        .send()
        .await
        .unwrap();

    let toggle_status = client
        .post(format!(
            "{}/admin/providers/{}/toggle",
            app.url, provider.id
        ))
        .json(&serde_json::json!({"enabled": false}))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(toggle_status, 409);
}

/// Use case: an operator tries to add a model under a provider they've
/// disabled — creation must fail up front rather than producing an
/// unusable, unreachable model.
#[tokio::test]
async fn model_create_fails_for_disabled_provider() {
    let app = setup_admin_app(None).await;
    let client = reqwest::Client::new();

    let provider: Provider = client
        .post(format!("{}/admin/providers", app.url))
        .json(&create_provider_req("disabled-provider", None))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    client
        .post(format!(
            "{}/admin/providers/{}/toggle",
            app.url, provider.id
        ))
        .json(&serde_json::json!({"enabled": false}))
        .send()
        .await
        .unwrap();

    let resp = client
        .post(format!("{}/admin/models", app.url))
        .json(&CreateModelRequest {
            name: "should-fail".to_string(),
            provider_id: provider.id,
            display_name: None,
            enabled: true,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

/// Use case: an operator adds a model, enables/disables it, and confirms
/// deletion is blocked while it's live (mirroring the provider guard above)
/// but succeeds once disabled.
#[tokio::test]
async fn model_full_crud_lifecycle() {
    let app = setup_admin_app(None).await;
    let client = reqwest::Client::new();

    let provider: Provider = client
        .post(format!("{}/admin/providers", app.url))
        .json(&create_provider_req("model-lifecycle-provider", None))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let model: Model = client
        .post(format!("{}/admin/models", app.url))
        .json(&CreateModelRequest {
            name: "gpt-z".to_string(),
            provider_id: provider.id.clone(),
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

/// Use case: a secret provider API key is stored encrypted at rest — the
/// admin API must still return it in plaintext to authenticated callers,
/// while the raw database row is unreadable ciphertext.
#[tokio::test]
async fn provider_encryption_at_rest_transparent() {
    let key_bytes: [u8; 32] = rand_bytes_32();
    let cipher = CipherKey::from_base64(&base64_encode(&key_bytes)).expect("valid 32-byte key");
    let app = setup_admin_app(Some(cipher)).await;
    let client = reqwest::Client::new();

    let created: Provider = client
        .post(format!("{}/admin/providers", app.url))
        .json(&create_provider_req(
            "secret-provider",
            Some("sk-top-secret-123"),
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        created.api_key.as_deref(),
        Some("sk-top-secret-123"),
        "API response must return the decrypted plaintext"
    );

    let db_url = format!("sqlite://{}", app.db_path.display());
    let pool = sqlx::sqlite::SqlitePool::connect(&db_url).await.unwrap();
    let (raw_api_key,): (String,) = sqlx::query_as("SELECT api_key FROM providers WHERE id = ?")
        .bind(&created.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(
        raw_api_key.starts_with("enc:v1:"),
        "raw DB row must be ciphertext, got: {raw_api_key}"
    );
    assert_ne!(raw_api_key, "sk-top-secret-123");
}

/// Regression test: SQLite-backed providers must record a real creation
/// timestamp, not the Unix epoch (the `datetime('now')`-vs-RFC3339 bug fixed
/// this session).
#[tokio::test]
async fn provider_created_at_is_real_timestamp_not_epoch() {
    let app = setup_admin_app(None).await;
    let client = reqwest::Client::new();
    let before = chrono::Utc::now();

    let created: Provider = client
        .post(format!("{}/admin/providers", app.url))
        .json(&create_provider_req("ts-provider", None))
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

    // Updating must also refresh updated_at to a real timestamp, not epoch.
    let updated: Provider = client
        .put(format!("{}/admin/providers/{}", app.url, created.id))
        .json(&serde_json::json!({"weight": 2.0}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        updated.updated_at > before - chrono::Duration::seconds(5),
        "updated_at should be ~now, got {}",
        updated.updated_at
    );
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

/// Use case: the same provider CRUD an operator relies on for SQLite
/// deployments must also work when the gateway is deployed against
/// Postgres — the gap this session's Postgres provider-store fix closed.
#[tokio::test]
async fn postgres_provider_crud_parity() {
    let Some(url) = postgres_test_url() else {
        eprintln!("skipping postgres_provider_crud_parity: TEST_POSTGRES_URL not set");
        return;
    };
    let (provider_store, model_store) = himadri_admin::connect_provider_model_stores(&url, None)
        .await
        .expect("postgres provider/model store should connect");
    let admin = AdminHandlers::new(StoreBackend::new().await, None)
        .with_provider_model_stores(provider_store, model_store);

    let created = admin
        .create_provider(create_provider_req(
            "pg-parity-provider",
            Some("sk-pg-secret"),
        ))
        .await
        .expect("create should succeed against postgres");
    assert_eq!(created.api_key.as_deref(), Some("sk-pg-secret"));

    let listed = admin.list_providers().await;
    assert!(listed.iter().any(|p| p.id == created.id));

    let updated = admin
        .update_provider(
            &created.id,
            UpdateProviderRequest {
                weight: Some(9.0),
                ..Default::default()
            },
        )
        .await
        .expect("update should succeed");
    assert_eq!(updated.weight, 9.0);

    assert!(admin.delete_provider(&created.id).await);
}

/// Use case: encryption at rest for provider secrets must behave identically
/// on Postgres as on SQLite.
#[tokio::test]
async fn postgres_encryption_at_rest_transparent() {
    let Some(url) = postgres_test_url() else {
        eprintln!("skipping postgres_encryption_at_rest_transparent: TEST_POSTGRES_URL not set");
        return;
    };
    let key_bytes: [u8; 32] = rand_bytes_32();
    let cipher = CipherKey::from_base64(&base64_encode(&key_bytes)).unwrap();

    let (provider_store, model_store) =
        himadri_admin::connect_provider_model_stores(&url, Some(cipher))
            .await
            .expect("postgres provider/model store should connect");
    let admin = AdminHandlers::new(StoreBackend::new().await, None)
        .with_provider_model_stores(provider_store, model_store);

    let created = admin
        .create_provider(create_provider_req(
            "pg-secret-provider",
            Some("sk-pg-top-secret"),
        ))
        .await
        .expect("create should succeed");
    assert_eq!(created.api_key.as_deref(), Some("sk-pg-top-secret"));

    let pool = sqlx::postgres::PgPool::connect(&url).await.unwrap();
    let row: (String,) = sqlx::query_as("SELECT api_key FROM providers WHERE id = $1::uuid")
        .bind(&created.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(
        row.0.starts_with("enc:v1:"),
        "raw postgres row must be ciphertext, got: {}",
        row.0
    );

    admin.delete_provider(&created.id).await;
}

fn rand_bytes_32() -> [u8; 32] {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    STANDARD.encode(bytes)
}
