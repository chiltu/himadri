mod combined_auth;
mod gateway;
mod handlers;
mod latency_store;
mod strategy;

use axum::{
    middleware,
    routing::{delete, get, post, put},
    Router,
};
use std::sync::Arc;
use tokio::net::TcpListener;
use tower_http::cors::{AllowHeaders, AllowMethods, Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing::info;

use himadri_admin::{AdminHandlers, AuthMiddleware, StoreBackend};
use himadri_core::{AuthContext, AuthScope, Config};
use himadri_observability::Metrics;
use himadri_plugins::{
    BudgetConfig, BudgetPlugin, MaxTokenPlugin, RateLimitConfig, RateLimitPlugin,
    RequestLoggerPlugin, ResponseCachePlugin, WordFilterPlugin,
};
use himadri_provider::{
    AnthropicProvider, BedrockProvider, GeminiProvider, OpenAiCompatibleConfig,
    OpenAiCompatibleProvider,
};

use gateway::Gateway;
use handlers::{AppState, *};

/// Command-line options. Parsed by hand to avoid an argument-parser
/// dependency for two flags.
struct CliArgs {
    /// Run embedded database migrations to the latest version before startup.
    migrate: bool,
    /// Listen port; overrides the PORT env var.
    port: Option<String>,
}

fn parse_cli_args() -> CliArgs {
    let mut cli = CliArgs {
        migrate: false,
        port: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--migrate" => cli.migrate = true,
            "--port" => cli.port = args.next(),
            "--help" | "-h" => {
                println!(
                    "himadri {}\n\nUSAGE:\n    himadri [OPTIONS]\n\nOPTIONS:\n    \
                     --migrate        Migrate the database (DATABASE_URL) to the latest \
                     schema version before starting\n    \
                     --port <PORT>    Listen port (overrides the PORT env var; default 8080)\n    \
                     -h, --help       Print this help",
                    env!("CARGO_PKG_VERSION")
                );
                std::process::exit(0);
            }
            other => {
                eprintln!("Unknown argument: {other} (see --help)");
                std::process::exit(2);
            }
        }
    }
    cli
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = parse_cli_args();

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

    // Explicit pre-startup migration (--migrate): bring the database schema
    // to the latest version and fail hard on any error, instead of the
    // log-and-fall-back behavior of the store connect paths.
    if cli.migrate {
        match std::env::var("DATABASE_URL") {
            Ok(database_url) => {
                if let Err(e) = himadri_admin::migrate_to_latest(&database_url).await {
                    tracing::error!("--migrate failed: {e}");
                    std::process::exit(1);
                }
                info!("Database migrated to latest schema version");
            }
            Err(_) => {
                tracing::error!("--migrate requires DATABASE_URL to be set");
                std::process::exit(1);
            }
        }
    }

    let gateway = Arc::new(build_gateway(&config).await);
    let (admin, auth) = build_admin(&config).await;

    // Sync routing targets from DB-registered providers/models, so they are
    // active immediately after a restart (previously they stayed inactive
    // until the first provider/model mutation triggered a rebuild). A DB
    // with no providers leaves the env/file-configured targets untouched.
    let db_providers = admin.list_providers().await;
    if !db_providers.is_empty() {
        let db_models = admin.list_models().await;
        gateway
            .rebuild_targets_from_db(&db_providers, &db_models)
            .await;
        info!(
            "Loaded {} provider(s) and {} model(s) from database into routing targets",
            db_providers.len(),
            db_models.len()
        );
    }

    // JWT/OIDC (when JWT_ISSUER is set) runs alongside API-key auth.
    let jwt_discovery = init_jwt_discovery().await;
    let combined_auth = Arc::new(combined_auth::CombinedAuth::new(
        auth.clone(),
        jwt_discovery,
        Some(gateway.audit_log_arc()),
    ));

    let state = AppState {
        gateway: gateway.clone(),
        admin,
        metrics: gateway.metrics(),
    };
    let app = build_router(state, &config, auth, combined_auth);

    let addr = cli
        .port
        .or_else(|| std::env::var("PORT").ok())
        .unwrap_or_else(|| "8080".to_string());
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

/// Build the gateway core: register env-configured providers, wire the
/// plugin pipeline, and attach the request-log store and response cache.
async fn build_gateway(config: &Config) -> Gateway {
    let mut gateway = Gateway::new(config.clone(), Arc::new(Metrics::new()));
    register_providers_from_env(&gateway);
    gateway.set_plugin_manager(wire_plugins());

    // Persist request logs to Postgres when configured; otherwise they remain
    // in-memory and are lost on restart.
    #[cfg(feature = "postgres")]
    if let Ok(database_url) = std::env::var("DATABASE_URL") {
        if database_url.starts_with("postgres") {
            match himadri_admin::PostgresRequestLogStore::new(&database_url).await {
                Ok(store) => {
                    gateway.set_request_log_store(Arc::new(store));
                    info!("Request logs persisted to Postgres");
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to initialize Postgres request log store ({}); \
                         falling back to in-memory logs",
                        e
                    );
                }
            }
        }
    }

    // Enable response caching if configured (CACHE_TTL_SECS, optional CACHE_MAX_ENTRIES).
    if let Ok(ttl_secs) = std::env::var("CACHE_TTL_SECS") {
        if let Ok(ttl) = ttl_secs.parse::<u64>() {
            let max_entries = std::env::var("CACHE_MAX_ENTRIES")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(10_000);
            let cache = ResponseCachePlugin::new(max_entries, std::time::Duration::from_secs(ttl));
            gateway.set_response_cache(cache);
            info!(
                "Registered response cache ({}s TTL, {} max entries)",
                ttl, max_entries
            );
        }
    }

    gateway
}

/// Register every provider configured through environment variables.
/// OpenAI, Anthropic and Gemini are always registered; the rest activate
/// when their API keys / endpoints are present.
fn register_providers_from_env(gateway: &Gateway) {
    // Register OpenAI (default, supports custom base URL via OPENAI_BASE_URL)
    let mut openai_config = OpenAiCompatibleConfig::openai();
    if let Ok(base_url) = std::env::var("OPENAI_BASE_URL") {
        openai_config.base_url = base_url;
        info!("OpenAI base URL overridden: {}", openai_config.base_url);
    }
    gateway.register_provider(Arc::new(OpenAiCompatibleProvider::new(openai_config)));

    // Register a secondary OpenAI-compatible upstream (for multi-endpoint /
    // failover setups). Provider name: "openai-secondary".
    if let Ok(base_url) = std::env::var("OPENAI_SECONDARY_BASE_URL") {
        let mut cfg = OpenAiCompatibleConfig::openai();
        cfg.name = "openai-secondary".to_string();
        cfg.display_name = "OpenAI Secondary".to_string();
        cfg.base_url = base_url;
        gateway.register_provider(Arc::new(OpenAiCompatibleProvider::new(cfg)));
        info!("Registered secondary OpenAI-compatible provider: openai-secondary");
    }

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
}

/// Assemble the plugin pipeline (word filter, max-token, request logger,
/// and the env-configured budget and rate-limit plugins).
fn wire_plugins() -> himadri_plugin::PluginManager {
    let mut plugin_manager = himadri_plugin::PluginManager::new();
    plugin_manager.register(WordFilterPlugin::new(vec![
        "password".to_string(),
        "secret".to_string(),
    ]));
    plugin_manager.register(MaxTokenPlugin::new(4096));
    plugin_manager.register(RequestLoggerPlugin::new());

    // Register the budget plugin when a global spend limit and/or token pricing
    // is configured. Pricing alone is enough: per-principal caps (e.g. a JWT
    // `budget_limit_usd` claim) are enforced against accumulated cost, which
    // requires pricing but not a global limit.
    let global_spend_limit = std::env::var("BUDGET_SPEND_LIMIT_USD")
        .ok()
        .and_then(|v| v.parse::<f64>().ok());
    let input_per_m = std::env::var("BUDGET_INPUT_PER_M_TOKENS")
        .ok()
        .and_then(|v| v.parse::<f64>().ok());
    let output_per_m = std::env::var("BUDGET_OUTPUT_PER_M_TOKENS")
        .ok()
        .and_then(|v| v.parse::<f64>().ok());

    if global_spend_limit.is_some() || input_per_m.is_some() || output_per_m.is_some() {
        match BudgetPlugin::new(BudgetConfig {
            spend_limit_usd: Some(global_spend_limit.unwrap_or(0.0)),
            input_per_m_tokens: input_per_m,
            output_per_m_tokens: output_per_m,
            ..Default::default()
        }) {
            Ok(budget_plugin) => {
                plugin_manager.register(budget_plugin);
                match global_spend_limit {
                    Some(limit) => info!(
                        "Registered budget plugin (global ${:.2} limit; per-principal caps honored)",
                        limit
                    ),
                    None => info!(
                        "Registered budget plugin (no global limit; per-principal caps honored)"
                    ),
                }
            }
            Err(e) => tracing::error!("Budget plugin not registered: {}", e),
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

    plugin_manager
}

/// Connect the API-key store and admin handlers (with provider/model stores
/// when DATABASE_URL is set), plus the master-key auth middleware.
async fn build_admin(config: &Config) -> (Arc<AdminHandlers>, Arc<AuthMiddleware>) {
    let store = StoreBackend::new().await;
    let master_key = config.admin.master_key.clone();

    if master_key.is_none() {
        tracing::warn!(
            "SECURITY: MASTER_KEY not set — all authentication is bypassed. \
             This is intended for development only. Set MASTER_KEY in production."
        );
    }

    // Initialize provider and model stores (SQLite or Postgres, selected by
    // DATABASE_URL's scheme — see himadri_admin::provider_backend).
    let mut admin = AdminHandlers::new(store.clone(), master_key.clone());
    let cipher = himadri_admin::CipherKey::from_env();
    if cipher.is_none() {
        tracing::warn!(
            "SECURITY: PROVIDER_ENCRYPTION_KEY not set — provider API keys are stored in \
             plaintext in the database. Set PROVIDER_ENCRYPTION_KEY (32-byte base64, e.g. \
             `openssl rand -base64 32`) in production."
        );
    }
    if let Ok(database_url) = std::env::var("DATABASE_URL") {
        if let Some((provider_store, model_store)) =
            himadri_admin::connect_provider_model_stores(&database_url, cipher).await
        {
            admin = admin.with_provider_model_stores(provider_store, model_store);
            info!("Initialized provider and model stores");
        }
    }
    let admin = Arc::new(admin);
    let auth = Arc::new(AuthMiddleware::new(store.clone(), master_key.clone()));
    (admin, auth)
}

/// Discover the OIDC issuer and start a background JWKS refresh task when
/// JWT_ISSUER is configured. Tokens are validated against the provider's
/// JWKS; API keys continue to work alongside JWTs on the same /v1 endpoints.
async fn init_jwt_discovery() -> Option<Arc<himadri_auth::OidcDiscovery>> {
    match std::env::var("JWT_ISSUER") {
        Ok(issuer) if !issuer.is_empty() => {
            let audience = std::env::var("JWT_AUDIENCE").unwrap_or_default();
            let jwks_uri = std::env::var("JWT_JWKS_URI").ok();
            match himadri_auth::OidcDiscovery::new(&issuer, &audience, jwks_uri.as_deref()).await {
                Ok(discovery) => {
                    info!("JWT/OIDC authentication enabled (issuer: {})", issuer);
                    // Periodically refresh the JWKS so rotated signing keys are
                    // picked up without a restart.
                    let refresh_secs = std::env::var("JWT_JWKS_REFRESH_SECS")
                        .ok()
                        .and_then(|v| v.parse::<u64>().ok())
                        .unwrap_or(3600);
                    let refresher = discovery.clone();
                    tokio::spawn(async move {
                        let interval = std::time::Duration::from_secs(refresh_secs);
                        loop {
                            tokio::time::sleep(interval).await;
                            if let Err(e) = refresher.refresh_jwks().await {
                                tracing::warn!("JWKS refresh failed: {}", e);
                            }
                        }
                    });
                    Some(discovery)
                }
                Err(e) => {
                    tracing::error!(
                        "Failed to initialize JWT/OIDC discovery ({}); JWT auth disabled",
                        e
                    );
                    None
                }
            }
        }
        _ => None,
    }
}

/// Assemble the public, bearer-auth API, and master-key admin routes with
/// CORS and tracing layers applied.
fn build_router(
    state: AppState,
    config: &Config,
    auth: Arc<AuthMiddleware>,
    combined_auth: Arc<combined_auth::CombinedAuth>,
) -> Router {
    let public_routes = Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics_handler))
        .route("/v1/models", get(list_models));

    let api_routes = Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/completions", post(completions))
        .route("/v1/embeddings", post(embeddings))
        .fallback(passthrough)
        .layer(middleware::from_fn_with_state(
            combined_auth.clone(),
            combined_auth::CombinedAuth::middleware,
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
        // Inner gate: reject authenticated-but-non-admin principals. Added
        // before the auth layer so it runs *after* it (last `.layer()` is
        // outermost), seeing the AuthContext that layer inserts.
        .layer(middleware::from_fn(require_admin_scope))
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

    Router::new()
        .merge(public_routes)
        .merge(api_routes)
        .merge(admin_routes)
        .layer(TraceLayer::new_for_http())
        .layer(cors_layer)
        .with_state(state)
}

/// Require `AuthScope::Admin` for `/admin/*` routes.
///
/// Runs after `AuthMiddleware`, which authenticates the bearer token and
/// inserts the `AuthContext`. Authentication alone is not authorization: a
/// valid but non-admin API key (`ReadOnly` / `ApiKey` scope) must not reach
/// admin endpoints — otherwise any key could mint admin keys, read decrypted
/// provider secrets, or wipe logs. In dev-bypass mode (`MASTER_KEY` unset)
/// the injected anonymous context is `Admin`, so this stays a no-op there.
async fn require_admin_scope(
    axum::extract::Extension(auth): axum::extract::Extension<Option<AuthContext>>,
    request: axum::extract::Request,
    next: middleware::Next,
) -> Result<axum::response::Response, axum::http::StatusCode> {
    match auth {
        Some(ctx) if ctx.scope == AuthScope::Admin => Ok(next.run(request).await),
        _ => Err(axum::http::StatusCode::FORBIDDEN),
    }
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

#[cfg(test)]
mod admin_scope_tests {
    use super::*;
    use axum::{body::Body, http::Request, http::StatusCode, routing::get, Router};
    use tower::ServiceExt;

    /// Build a one-route admin app gated by `require_admin_scope`, with an
    /// inner layer that injects `ctx` the way `AuthMiddleware` would.
    fn app(ctx: Option<AuthContext>) -> Router {
        Router::new()
            .route("/admin/keys", get(|| async { "ok" }))
            .layer(middleware::from_fn(require_admin_scope))
            .layer(middleware::from_fn(
                move |mut req: axum::extract::Request, next: middleware::Next| {
                    let ctx = ctx.clone();
                    async move {
                        req.extensions_mut().insert(ctx);
                        next.run(req).await
                    }
                },
            ))
    }

    async fn status_for(ctx: Option<AuthContext>) -> StatusCode {
        app(ctx)
            .oneshot(
                Request::builder()
                    .uri("/admin/keys")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    fn ctx_with(scope: AuthScope) -> AuthContext {
        AuthContext {
            scope,
            ..AuthContext::anonymous()
        }
    }

    #[tokio::test]
    async fn admin_scope_is_allowed() {
        assert_eq!(status_for(Some(ctx_with(AuthScope::Admin))).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn non_admin_scopes_are_forbidden() {
        // A regular API key or read-only key must not reach admin routes.
        assert_eq!(
            status_for(Some(ctx_with(AuthScope::ApiKey))).await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            status_for(Some(ctx_with(AuthScope::ReadOnly))).await,
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn missing_context_is_forbidden() {
        assert_eq!(status_for(None).await, StatusCode::FORBIDDEN);
    }
}
