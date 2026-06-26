mod gateway;
mod latency_store;
mod strategy;

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
use tower_http::cors::{AllowHeaders, AllowMethods, Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing::info;

use himadri_admin::{AdminHandlers, AuthMiddleware, StoreBackend};
use himadri_core::{
    AuthContext, ChatCompletionRequest, Config, GatewayError, ModelListResponse, ModelObject,
};
use himadri_observability::Metrics;
use himadri_plugins::{
    BudgetConfig, BudgetPlugin, MaxTokenPlugin, RateLimitConfig, RateLimitPlugin,
    RequestLoggerPlugin, WordFilterPlugin,
};
use himadri_provider::{
    AnthropicProvider, BedrockProvider, GeminiProvider, OpenAiCompatibleConfig,
    OpenAiCompatibleProvider,
};

use gateway::Gateway;

#[derive(Clone)]
struct AppState {
    gateway: Arc<Gateway>,
    #[allow(dead_code)]
    admin: Arc<AdminHandlers>,
    store: StoreBackend,
    metrics: Arc<Metrics>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = Config::load_from_env().unwrap_or_else(|e| {
        eprintln!("Failed to load config: {}, using defaults", e);
        Config::default()
    });

    // MASTER_KEY env var overrides config
    if let Ok(key) = std::env::var("MASTER_KEY") {
        config.admin.master_key = Some(key);
    }

    himadri_observability::init_tracing(
        &config.observability.tracing.service_name,
        config.observability.tracing.endpoint.as_deref(),
        config.observability.tracing.sample_ratio,
    );

    info!("Starting himadri v{}", env!("CARGO_PKG_VERSION"));

    let mut gateway = Gateway::new(config.clone(), Arc::new(Metrics::new()));

    // Register OpenAI (default, supports custom base URL via OPENAI_BASE_URL)
    let mut openai_config = OpenAiCompatibleConfig::openai();
    if let Ok(base_url) = std::env::var("OPENAI_BASE_URL") {
        openai_config.base_url = base_url;
        info!("OpenAI base URL overridden: {}", openai_config.base_url);
    }
    gateway.register_provider(Arc::new(OpenAiCompatibleProvider::new(openai_config)));

    // Register Anthropic (different API format)
    gateway.register_provider(Arc::new(AnthropicProvider::new(None)));

    // Register Gemini (different API format)
    gateway.register_provider(Arc::new(GeminiProvider::new(None)));

    // Register Azure OpenAI if configured
    if let (Some(api_key), Some(base_url), Some(deployment)) = (
        std::env::var("AZURE_OPENAI_API_KEY").ok(),
        std::env::var("AZURE_OPENAI_ENDPOINT").ok(),
        std::env::var("AZURE_OPENAI_DEPLOYMENT").ok(),
    ) {
        let api_version = std::env::var("AZURE_OPENAI_API_VERSION").ok();
        gateway.register_provider(Arc::new(OpenAiCompatibleProvider::azure(
            &api_key,
            &base_url,
            &deployment,
            api_version.as_deref().unwrap_or("2024-10-21"),
        )));
        info!("Registered Azure OpenAI provider");
    }

    // Register Bedrock if configured
    if let (Some(access_key), Some(secret_key)) = (
        std::env::var("AWS_ACCESS_KEY_ID").ok(),
        std::env::var("AWS_SECRET_ACCESS_KEY").ok(),
    ) {
        let region = std::env::var("AWS_REGION").ok();
        let session_token = std::env::var("AWS_SESSION_TOKEN").ok();
        gateway.register_provider(Arc::new(BedrockProvider::new(
            region.as_deref(),
            &access_key,
            &secret_key,
            session_token.as_deref(),
        )));
        info!("Registered AWS Bedrock provider");
    }

    // Register OpenRouter if configured
    if std::env::var("OPENROUTER_API_KEY").is_ok() {
        gateway.register_provider(Arc::new(OpenAiCompatibleProvider::new(
            OpenAiCompatibleConfig::openrouter(),
        )));
        info!("Registered OpenRouter provider");
    }

    // Register Together AI if configured
    if std::env::var("TOGETHER_API_KEY").is_ok() {
        gateway.register_provider(Arc::new(OpenAiCompatibleProvider::new(
            OpenAiCompatibleConfig::together_ai(),
        )));
        info!("Registered Together AI provider");
    }

    // Register Groq if configured
    if std::env::var("GROQ_API_KEY").is_ok() {
        gateway.register_provider(Arc::new(OpenAiCompatibleProvider::new(
            OpenAiCompatibleConfig::groq(),
        )));
        info!("Registered Groq provider");
    }

    // Register Fireworks if configured
    if std::env::var("FIREWORKS_API_KEY").is_ok() {
        gateway.register_provider(Arc::new(OpenAiCompatibleProvider::new(
            OpenAiCompatibleConfig::fireworks(),
        )));
        info!("Registered Fireworks AI provider");
    }

    // Register DeepInfra if configured
    if std::env::var("DEEPINFRA_API_KEY").is_ok() {
        gateway.register_provider(Arc::new(OpenAiCompatibleProvider::new(
            OpenAiCompatibleConfig::deepinfra(),
        )));
        info!("Registered DeepInfra provider");
    }

    // Register Cerebras if configured
    if std::env::var("CEREBRAS_API_KEY").is_ok() {
        gateway.register_provider(Arc::new(OpenAiCompatibleProvider::new(
            OpenAiCompatibleConfig::cerebras(),
        )));
        info!("Registered Cerebras provider");
    }

    // Register Novita if configured
    if std::env::var("NOVITA_API_KEY").is_ok() {
        gateway.register_provider(Arc::new(OpenAiCompatibleProvider::new(
            OpenAiCompatibleConfig::novita(),
        )));
        info!("Registered Novita AI provider");
    }

    let mut plugin_manager = himadri_plugin::PluginManager::new();
    plugin_manager.register(WordFilterPlugin::new(vec![
        "password".to_string(),
        "secret".to_string(),
    ]));
    plugin_manager.register(MaxTokenPlugin::new(4096));
    plugin_manager.register(RequestLoggerPlugin::new());

    // Register budget plugin if configured
    if let Ok(spend_limit) = std::env::var("BUDGET_SPEND_LIMIT_USD") {
        if let Ok(limit) = spend_limit.parse::<f64>() {
            if let Ok(budget_plugin) = BudgetPlugin::new(BudgetConfig {
                spend_limit_usd: Some(limit),
                input_per_m_tokens: std::env::var("BUDGET_INPUT_PER_M_TOKENS")
                    .ok()
                    .and_then(|v| v.parse().ok()),
                output_per_m_tokens: std::env::var("BUDGET_OUTPUT_PER_M_TOKENS")
                    .ok()
                    .and_then(|v| v.parse().ok()),
                ..Default::default()
            }) {
                plugin_manager.register(budget_plugin);
                info!("Registered budget plugin with ${:.2} limit", limit);
            }
        }
    }

    // Register rate limit plugin if configured
    if let Ok(rpm) = std::env::var("RATE_LIMIT_KEY_RPM") {
        if let Ok(rpm_val) = rpm.parse::<u64>() {
            if let Ok(rl_plugin) = RateLimitPlugin::new(RateLimitConfig {
                key_rpm: Some(rpm_val),
                ..Default::default()
            }) {
                plugin_manager.register(rl_plugin);
                info!("Registered rate limit plugin with {} RPM per key", rpm_val);
            }
        }
    }

    // Register per-IP rate limit plugin if configured
    if let Ok(rpm) = std::env::var("RATE_LIMIT_IP_RPM") {
        if let Ok(rpm_val) = rpm.parse::<u64>() {
            if let Ok(rl_plugin) = RateLimitPlugin::new(RateLimitConfig {
                ip_rpm: Some(rpm_val),
                requests_per_second: Some(1_000_000), // high global limit so only IP check matters
                ..Default::default()
            }) {
                plugin_manager.register(rl_plugin);
                info!("Registered rate limit plugin with {} RPM per IP", rpm_val);
            }
        }
    }

    gateway.set_plugin_manager(plugin_manager);

    let gateway = Arc::new(gateway);
    let store = StoreBackend::new().await;
    let master_key = config.admin.master_key.clone();

    if master_key.is_none() {
        tracing::warn!(
            "SECURITY: MASTER_KEY not set — all authentication is bypassed. \
             This is intended for development only. Set MASTER_KEY in production."
        );
    }

    // Initialize provider and model stores if SQLite is configured
    let mut admin = AdminHandlers::new(store.clone(), master_key.clone());
    #[cfg(feature = "sqlite")]
    if let Ok(database_url) = std::env::var("DATABASE_URL") {
        if database_url.starts_with("sqlite") {
            if let Ok(pool) = sqlx::SqlitePool::connect(&format!("{}?mode=rwc", database_url)).await {
                let provider_store = himadri_admin::ProviderStore::new(pool.clone());
                let model_store = himadri_admin::ModelStore::new(pool.clone());
                admin = admin.with_provider_model_stores(provider_store, model_store);
                info!("Initialized provider and model stores");
            }
        }
    }
    let admin = Arc::new(admin);
    let auth = Arc::new(AuthMiddleware::new(store.clone(), master_key.clone()));

    let state = AppState {
        gateway: gateway.clone(),
        admin,
        store,
        metrics: gateway.metrics(),
    };

    let public_routes = Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics_handler))
        .route("/v1/models", get(list_models));

    let api_routes = Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/completions", post(completions))
        .fallback(passthrough)
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
        .route("/admin/keys/{id}/rotate", post(rotate_key))
        .route("/admin/providers", get(list_providers))
        .route("/admin/providers", post(create_provider))
        .route("/admin/providers/{id}", get(get_provider))
        .route("/admin/providers/{id}", put(update_provider))
        .route("/admin/providers/{id}", delete(delete_provider))
        .route("/admin/providers/{id}/toggle", post(toggle_provider))
        .route("/admin/models", get(list_models_api))
        .route("/admin/models", post(create_model))
        .route("/admin/models/{id}", get(get_model))
        .route("/admin/models/{id}", put(update_model))
        .route("/admin/models/{id}", delete(delete_model))
        .route("/admin/models/{id}/toggle", post(toggle_model))
        .route("/admin/dashboard", get(dashboard))
        .route("/admin/usage", get(usage_stats))
        .route("/admin/usage/{key_id}", get(key_usage_stats))
        .route("/admin/config", get(get_config))
        .route("/admin/config", put(update_config))
        .route("/admin/config/history", get(config_history))
        .route("/admin/config/rollback/{version}", post(config_rollback))
        .route("/admin/logs", get(list_logs))
        .route("/admin/logs", delete(delete_logs))
        .route("/admin/reload", post(reload_config))
        .layer(middleware::from_fn_with_state(
            auth.clone(),
            AuthMiddleware::middleware,
        ));

    // Build CORS layer from config
    let cors_layer = if config.cors.enabled {
        let mut cors = CorsLayer::new();
        if config.cors.allowed_origins.is_empty() {
            cors = cors.allow_origin(Any);
        } else {
            for origin in &config.cors.allowed_origins {
                if let Ok(url) = origin.parse::<axum::http::HeaderValue>() {
                    cors = cors.allow_origin(url);
                }
            }
        }
        let methods: Vec<axum::http::Method> = config
            .cors
            .allowed_methods
            .iter()
            .filter_map(|m| m.parse().ok())
            .collect();
        cors = cors.allow_methods(AllowMethods::list(methods));
        let headers: Vec<axum::http::header::HeaderName> = config
            .cors
            .allowed_headers
            .iter()
            .filter_map(|h| h.parse().ok())
            .collect();
        cors = cors.allow_headers(AllowHeaders::list(headers));
        cors
    } else {
        CorsLayer::new()
    };

    let app = Router::new()
        .merge(public_routes)
        .merge(api_routes)
        .merge(admin_routes)
        .layer(TraceLayer::new_for_http())
        .layer(cors_layer)
        .with_state(state);

    let addr = std::env::var("PORT").unwrap_or_else(|_| "8080".to_string());
    let listener = TcpListener::bind(format!("0.0.0.0:{}", addr)).await?;
    info!("Server listening on {}", addr);

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;

    himadri_observability::shutdown_tracing();
    Ok(())
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

async fn metrics_handler(State(state): State<AppState>) -> String {
    state.metrics.encode_metrics()
}

async fn list_models(State(state): State<AppState>) -> Json<ModelListResponse> {
    let providers = state.gateway.list_providers();
    let mut models = Vec::new();

    for provider in &providers {
        match provider.as_str() {
            "openai" => {
                models.push(ModelObject {
                    id: "gpt-4".to_string(),
                    object: "model".to_string(),
                    created: 1686935002,
                    owned_by: "openai".to_string(),
                });
                models.push(ModelObject {
                    id: "gpt-4-turbo".to_string(),
                    object: "model".to_string(),
                    created: 1686935002,
                    owned_by: "openai".to_string(),
                });
                models.push(ModelObject {
                    id: "gpt-4o".to_string(),
                    object: "model".to_string(),
                    created: 1686935002,
                    owned_by: "openai".to_string(),
                });
                models.push(ModelObject {
                    id: "gpt-4o-mini".to_string(),
                    object: "model".to_string(),
                    created: 1686935002,
                    owned_by: "openai".to_string(),
                });
                models.push(ModelObject {
                    id: "gpt-3.5-turbo".to_string(),
                    object: "model".to_string(),
                    created: 1686935002,
                    owned_by: "openai".to_string(),
                });
                models.push(ModelObject {
                    id: "o1".to_string(),
                    object: "model".to_string(),
                    created: 1686935002,
                    owned_by: "openai".to_string(),
                });
                models.push(ModelObject {
                    id: "o1-mini".to_string(),
                    object: "model".to_string(),
                    created: 1686935002,
                    owned_by: "openai".to_string(),
                });
            }
            "anthropic" => {
                models.push(ModelObject {
                    id: "claude-3-5-sonnet-20241022".to_string(),
                    object: "model".to_string(),
                    created: 1686935002,
                    owned_by: "anthropic".to_string(),
                });
                models.push(ModelObject {
                    id: "claude-3-5-haiku-20241022".to_string(),
                    object: "model".to_string(),
                    created: 1686935002,
                    owned_by: "anthropic".to_string(),
                });
                models.push(ModelObject {
                    id: "claude-3-opus-20240229".to_string(),
                    object: "model".to_string(),
                    created: 1686935002,
                    owned_by: "anthropic".to_string(),
                });
                models.push(ModelObject {
                    id: "claude-3-haiku-20240307".to_string(),
                    object: "model".to_string(),
                    created: 1686935002,
                    owned_by: "anthropic".to_string(),
                });
            }
            "gemini" => {
                models.push(ModelObject {
                    id: "gemini-2.0-flash".to_string(),
                    object: "model".to_string(),
                    created: 1686935002,
                    owned_by: "google".to_string(),
                });
                models.push(ModelObject {
                    id: "gemini-1.5-pro".to_string(),
                    object: "model".to_string(),
                    created: 1686935002,
                    owned_by: "google".to_string(),
                });
                models.push(ModelObject {
                    id: "gemini-1.5-flash".to_string(),
                    object: "model".to_string(),
                    created: 1686935002,
                    owned_by: "google".to_string(),
                });
            }
            "azure-openai" => {
                models.push(ModelObject {
                    id: "gpt-4".to_string(),
                    object: "model".to_string(),
                    created: 1686935002,
                    owned_by: "azure".to_string(),
                });
                models.push(ModelObject {
                    id: "gpt-4o".to_string(),
                    object: "model".to_string(),
                    created: 1686935002,
                    owned_by: "azure".to_string(),
                });
            }
            "bedrock" => {
                models.push(ModelObject {
                    id: "anthropic.claude-3-5-sonnet-20241022-v2:0".to_string(),
                    object: "model".to_string(),
                    created: 1686935002,
                    owned_by: "aws".to_string(),
                });
                models.push(ModelObject {
                    id: "anthropic.claude-3-opus-20240229-v1:0".to_string(),
                    object: "model".to_string(),
                    created: 1686935002,
                    owned_by: "aws".to_string(),
                });
                models.push(ModelObject {
                    id: "meta.llama3-70b-instruct-v1:0".to_string(),
                    object: "model".to_string(),
                    created: 1686935002,
                    owned_by: "aws".to_string(),
                });
            }
            "openrouter" => {
                models.push(ModelObject {
                    id: "openrouter/auto".to_string(),
                    object: "model".to_string(),
                    created: 1686935002,
                    owned_by: "openrouter".to_string(),
                });
                models.push(ModelObject {
                    id: "openai/gpt-4o".to_string(),
                    object: "model".to_string(),
                    created: 1686935002,
                    owned_by: "openrouter".to_string(),
                });
                models.push(ModelObject {
                    id: "anthropic/claude-3.5-sonnet".to_string(),
                    object: "model".to_string(),
                    created: 1686935002,
                    owned_by: "openrouter".to_string(),
                });
            }
            "together" => {
                models.push(ModelObject {
                    id: "meta-llama/Llama-3-70b-chat-hf".to_string(),
                    object: "model".to_string(),
                    created: 1686935002,
                    owned_by: "together".to_string(),
                });
                models.push(ModelObject {
                    id: "mistralai/Mixtral-8x7B-Instruct-v0.1".to_string(),
                    object: "model".to_string(),
                    created: 1686935002,
                    owned_by: "together".to_string(),
                });
            }
            "groq" => {
                models.push(ModelObject {
                    id: "llama3-70b-8192".to_string(),
                    object: "model".to_string(),
                    created: 1686935002,
                    owned_by: "groq".to_string(),
                });
                models.push(ModelObject {
                    id: "mixtral-8x7b-32768".to_string(),
                    object: "model".to_string(),
                    created: 1686935002,
                    owned_by: "groq".to_string(),
                });
            }
            _ => {}
        }
    }

    Json(ModelListResponse {
        object: "list".to_string(),
        data: models,
    })
}

async fn chat_completions(
    State(state): State<AppState>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    axum::extract::Extension(auth): axum::extract::Extension<Option<AuthContext>>,
    Json(request): Json<ChatCompletionRequest>,
) -> Response {
    let remote_ip = resolve_remote_ip(peer, &headers);
    if request.stream {
        match state
            .gateway
            .route_stream(request, auth.as_ref(), remote_ip)
            .await
        {
            Ok(stream) => {
                use axum::response::sse::{Event, Sse};
                use futures::StreamExt;
                use std::convert::Infallible;

                let event_stream = stream.map(|chunk| match chunk {
                    Ok(chunk) => {
                        let data = serde_json::to_string(&chunk).unwrap_or_default();
                        Ok::<_, Infallible>(Event::default().data(data))
                    }
                    Err(e) => {
                        let error_data = serde_json::json!({
                            "error": { "message": e.to_string(), "type": "gateway_error" }
                        });
                        Ok(Event::default().data(error_data.to_string()))
                    }
                });

                Sse::new(event_stream)
                    .keep_alive(
                        axum::response::sse::KeepAlive::new()
                            .interval(std::time::Duration::from_secs(15))
                            .text("ping"),
                    )
                    .into_response()
            }
            Err(e) => error_to_response(e),
        }
    } else {
        match state.gateway.route(request, auth.as_ref(), remote_ip).await {
            Ok(response) => Json(response).into_response(),
            Err(e) => error_to_response(e),
        }
    }
}

async fn completions(
    State(state): State<AppState>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    axum::extract::Extension(auth): axum::extract::Extension<Option<AuthContext>>,
    Json(request): Json<ChatCompletionRequest>,
) -> Response {
    let remote_ip = resolve_remote_ip(peer, &headers);
    match state.gateway.route(request, auth.as_ref(), remote_ip).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => error_to_response(e),
    }
}

async fn list_keys(
    State(state): State<AppState>,
) -> Result<Json<Vec<himadri_admin::ApiKey>>, (StatusCode, String)> {
    state
        .store
        .list()
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

async fn create_key(
    State(state): State<AppState>,
    Json(request): Json<himadri_admin::CreateApiKeyRequest>,
) -> Result<(StatusCode, Json<himadri_admin::ApiKey>), (StatusCode, String)> {
    state
        .store
        .create(request)
        .await
        .map(|key| (StatusCode::CREATED, Json(key)))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

async fn get_key(
    State(state): State<AppState>,
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
    State(state): State<AppState>,
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
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    state
        .store
        .delete(&id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
        .map(|deleted| {
            if deleted {
                StatusCode::NO_CONTENT
            } else {
                StatusCode::NOT_FOUND
            }
        })
}

async fn revoke_key(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    state
        .store
        .revoke(&id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
        .map(|revoked| {
            if revoked {
                StatusCode::OK
            } else {
                StatusCode::NOT_FOUND
            }
        })
}

async fn rotate_key(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<himadri_admin::ApiKey>, StatusCode> {
    state
        .store
        .rotate(&id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

async fn passthrough(State(_state): State<AppState>, _request: axum::extract::Request) -> Response {
    error_to_response(GatewayError::NotFound(
        "Endpoint not implemented".to_string(),
    ))
}

async fn reload_config(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let new_config = Config::load_from_env().map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("Failed to load config: {}", e),
        )
    })?;
    state
        .gateway
        .reload_config(new_config)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(Json(serde_json::json!({ "status": "reloaded" })))
}

// ─── New Admin Endpoints ─────────────────────────────────────────────

async fn dashboard(State(state): State<AppState>) -> Json<himadri_admin::DashboardSummary> {
    let key_count = state.store.list().await.map(|k| k.len()).unwrap_or(0);
    let dashboard = state.gateway.usage_store().get_dashboard(key_count);
    Json(dashboard)
}

async fn usage_stats(State(state): State<AppState>) -> Json<serde_json::Value> {
    let store = state.gateway.usage_store();
    let dashboard = store.get_dashboard(0);
    Json(serde_json::json!({
        "total_requests": dashboard.total_requests,
        "total_tokens": dashboard.total_tokens,
        "total_cost_usd": dashboard.total_cost_usd,
        "avg_latency_ms": dashboard.avg_latency_ms,
        "error_rate": dashboard.error_rate,
        "top_models": dashboard.top_models,
        "top_providers": dashboard.top_providers,
    }))
}

async fn key_usage_stats(
    State(state): State<AppState>,
    Path(key_id): Path<String>,
) -> Result<Json<himadri_admin::UsageStats>, StatusCode> {
    let store = state.gateway.usage_store();
    let stats = store.get_key_stats(&key_id);
    Ok(Json(stats))
}

async fn get_config(State(state): State<AppState>) -> Json<himadri_core::Config> {
    let config = state.gateway.get_config().await;
    Json(config)
}

async fn update_config(
    State(state): State<AppState>,
    Json(new_config): Json<himadri_core::Config>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .gateway
        .reload_config(new_config)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(Json(serde_json::json!({ "status": "updated" })))
}

async fn config_history(State(state): State<AppState>) -> Json<serde_json::Value> {
    let config = state.gateway.get_config().await;
    Json(serde_json::json!({
        "data": [],
        "summary": { "total_versions": 1 },
        "current_config": config,
    }))
}

async fn config_rollback(
    State(_state): State<AppState>,
    Path(_version): Path<u32>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    Err((
        StatusCode::NOT_IMPLEMENTED,
        "Rollback not yet implemented".to_string(),
    ))
}

async fn list_logs(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<himadri_admin::RequestLogQuery>,
) -> Result<Json<himadri_admin::RequestLogListResult>, (StatusCode, String)> {
    let result = state
        .gateway
        .request_log()
        .list(query)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(result))
}

async fn delete_logs(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<himadri_admin::MaintenanceQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let deleted = state
        .gateway
        .request_log()
        .delete(query)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(serde_json::json!({ "deleted": deleted })))
}

// ─── Provider Handlers ───────────────────────────────────────────────

async fn list_providers(
    State(state): State<AppState>,
) -> Result<Json<Vec<himadri_admin::Provider>>, (StatusCode, String)> {
    let admin = state.admin.as_ref();
    Ok(Json(admin.list_providers().await))
}

async fn create_provider(
    State(state): State<AppState>,
    Json(request): Json<himadri_admin::CreateProviderRequest>,
) -> Result<(StatusCode, Json<himadri_admin::Provider>), (StatusCode, String)> {
    let admin = state.admin.as_ref();
    match admin.create_provider(request).await {
        Some(p) => {
            // Rebuild routing targets
            let providers = admin.list_providers().await;
            let models = admin.list_models().await;
            state.gateway.rebuild_targets_from_db(&providers, &models).await;
            Ok((StatusCode::CREATED, Json(p)))
        }
        None => Err((StatusCode::INTERNAL_SERVER_ERROR, "Failed to create provider".to_string())),
    }
}

async fn get_provider(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<himadri_admin::Provider>, StatusCode> {
    let admin = state.admin.as_ref();
    admin.get_provider(&id).await.map(Json).ok_or(StatusCode::NOT_FOUND)
}

async fn update_provider(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<himadri_admin::UpdateProviderRequest>,
) -> Result<Json<himadri_admin::Provider>, StatusCode> {
    let admin = state.admin.as_ref();
    match admin.update_provider(&id, request).await {
        Some(p) => {
            // Rebuild routing targets
            let providers = admin.list_providers().await;
            let models = admin.list_models().await;
            state.gateway.rebuild_targets_from_db(&providers, &models).await;
            Ok(Json(p))
        }
        None => Err(StatusCode::NOT_FOUND),
    }
}

async fn delete_provider(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    let admin = state.admin.as_ref();
    if admin.delete_provider(&id).await {
        // Rebuild routing targets
        let providers = admin.list_providers().await;
        let models = admin.list_models().await;
        state.gateway.rebuild_targets_from_db(&providers, &models).await;
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

async fn toggle_provider(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<himadri_admin::Provider>, StatusCode> {
    let admin = state.admin.as_ref();
    let enabled = body.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
    match admin.toggle_provider(&id, enabled).await {
        Some(p) => {
            // Rebuild routing targets
            let providers = admin.list_providers().await;
            let models = admin.list_models().await;
            state.gateway.rebuild_targets_from_db(&providers, &models).await;
            Ok(Json(p))
        }
        None => Err(StatusCode::NOT_FOUND),
    }
}

// ─── Model Handlers ──────────────────────────────────────────────────

async fn list_models_api(
    State(state): State<AppState>,
) -> Result<Json<Vec<himadri_admin::Model>>, (StatusCode, String)> {
    let admin = state.admin.as_ref();
    Ok(Json(admin.list_models().await))
}

async fn create_model(
    State(state): State<AppState>,
    Json(request): Json<himadri_admin::CreateModelRequest>,
) -> Result<(StatusCode, Json<himadri_admin::Model>), (StatusCode, String)> {
    let admin = state.admin.as_ref();
    match admin.create_model(request).await {
        Some(m) => {
            // Rebuild routing targets
            let providers = admin.list_providers().await;
            let models = admin.list_models().await;
            state.gateway.rebuild_targets_from_db(&providers, &models).await;
            Ok((StatusCode::CREATED, Json(m)))
        }
        None => Err((StatusCode::INTERNAL_SERVER_ERROR, "Failed to create model".to_string())),
    }
}

async fn get_model(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<himadri_admin::Model>, StatusCode> {
    let admin = state.admin.as_ref();
    admin.get_model(&id).await.map(Json).ok_or(StatusCode::NOT_FOUND)
}

async fn update_model(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<himadri_admin::UpdateModelRequest>,
) -> Result<Json<himadri_admin::Model>, StatusCode> {
    let admin = state.admin.as_ref();
    match admin.update_model(&id, request).await {
        Some(m) => {
            // Rebuild routing targets
            let providers = admin.list_providers().await;
            let models = admin.list_models().await;
            state.gateway.rebuild_targets_from_db(&providers, &models).await;
            Ok(Json(m))
        }
        None => Err(StatusCode::NOT_FOUND),
    }
}

async fn delete_model(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    let admin = state.admin.as_ref();
    if admin.delete_model(&id).await {
        // Rebuild routing targets
        let providers = admin.list_providers().await;
        let models = admin.list_models().await;
        state.gateway.rebuild_targets_from_db(&providers, &models).await;
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

async fn toggle_model(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<himadri_admin::Model>, StatusCode> {
    let admin = state.admin.as_ref();
    let enabled = body.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
    match admin.toggle_model(&id, enabled).await {
        Some(m) => {
            // Rebuild routing targets
            let providers = admin.list_providers().await;
            let models = admin.list_models().await;
            state.gateway.rebuild_targets_from_db(&providers, &models).await;
            Ok(Json(m))
        }
        None => Err(StatusCode::NOT_FOUND),
    }
}

fn error_to_response(e: GatewayError) -> Response {
    let status = StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let body = Json(serde_json::json!({
        "error": { "message": e.to_string(), "type": "gateway_error" }
    }));
    (status, body).into_response()
}

/// Check if an IP address is loopback or private (RFC 1918 / link-local / loopback).
fn is_private_or_loopback(ip: std::net::IpAddr) -> bool {
    ip.is_loopback()
        || match ip {
            std::net::IpAddr::V4(v4) => {
                v4.is_private()
                    || v4.is_link_local()
                    || v4.is_unspecified()
                    || v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64 // 100.64.0.0/10 (CGNAT)
                    || v4.octets()[0] == 169 && v4.octets()[1] == 254 // 169.254.0.0/16 (link-local)
            }
            std::net::IpAddr::V6(v6) => {
                v6.is_loopback()
                    || v6.is_unspecified()
                    || v6.is_unique_local() // fc00::/7
                    || v6.is_unicast_link_local() // fe80::/10
            }
        }
}

/// Resolve the client's IP address.
///
/// Uses TCP peer address as the source of truth. Only falls back to proxy
/// headers when the peer is a known trusted proxy (loopback/private range).
/// This prevents IP spoofing via X-Forwarded-For / X-Real-IP headers.
fn resolve_remote_ip(
    peer: std::net::SocketAddr,
    headers: &axum::http::HeaderMap,
) -> Option<String> {
    let peer_ip = peer.ip();

    // If the peer is a loopback or private address, it's likely a reverse proxy.
    // In that case, we can cautiously trust proxy headers — but only the
    // rightmost IP that isn't another private/loopback address (i.e., the
    // outermost client added by the last non-proxy hop).
    if is_private_or_loopback(peer_ip) {
        if let Some(ip) = trusted_proxy_ip(headers) {
            return Some(ip);
        }
    }

    // Direct connection or untrusted proxy — use TCP peer address.
    Some(peer_ip.to_string())
}

/// Extract the most trustworthy client IP from proxy headers.
///
/// Parses X-Forwarded-For and returns the rightmost non-private IP
/// (the outermost client). Falls back to X-Real-IP if present.
/// Returns None if no usable IP is found.
fn trusted_proxy_ip(headers: &axum::http::HeaderMap) -> Option<String> {
    // X-Forwarded-For: client, proxy1, proxy2
    // The rightmost non-private/non-loopback IP is the outermost client.
    if let Some(val) = headers.get("x-forwarded-for") {
        if let Ok(s) = val.to_str() {
            // Walk right-to-left, pick the first non-private/non-loopback IP.
            for ip_str in s.split(',').rev() {
                let ip_str = ip_str.trim();
                if let Ok(addr) = ip_str.parse::<std::net::IpAddr>() {
                    if !is_private_or_loopback(addr) {
                        return Some(addr.to_string());
                    }
                }
            }
        }
    }
    // X-Real-IP (single value, client-controlled — only use if the value
    // itself looks like a public IP, which is weaker but better than nothing).
    if let Some(val) = headers.get("x-real-ip") {
        if let Ok(s) = val.to_str() {
            let ip_str = s.trim();
            if let Ok(addr) = ip_str.parse::<std::net::IpAddr>() {
                if !is_private_or_loopback(addr) {
                    return Some(addr.to_string());
                }
            }
        }
    }
    None
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    info!("Shutdown signal received, starting graceful shutdown");
}
