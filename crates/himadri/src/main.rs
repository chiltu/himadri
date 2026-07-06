// `gateway`, `strategy` and `latency_store` live in the library crate
// (`himadri::*`); the binary consumes them rather than compiling its own copy.
// Only `handlers` and `combined_auth` are binary-local.
mod combined_auth;
mod handlers;

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
    AnthropicProvider, GeminiProvider, OpenAiCompatibleConfig, OpenAiCompatibleProvider,
};

use handlers::{AppState, *};
use himadri::gateway::Gateway;

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

    // Fail fast on a malformed config: silently booting with defaults would
    // run without the operator's auth/routing settings.
    let mut config = match Config::load_from_env() {
        Ok(config) => config,
        Err(e) => {
            eprintln!("Failed to load config: {e}");
            std::process::exit(1);
        }
    };

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

    // OpenAI-compatible vendors: registered when their API-key env var is set.
    // Adding a vendor is one table row (plus its `OpenAiCompatibleConfig`
    // preset). Row = (API-key env var, config factory, display name).
    type ProviderPreset = (&'static str, fn() -> OpenAiCompatibleConfig, &'static str);
    let openai_compatible: &[ProviderPreset] = &[
        (
            "OPENROUTER_API_KEY",
            OpenAiCompatibleConfig::openrouter,
            "OpenRouter",
        ),
        (
            "TOGETHER_API_KEY",
            OpenAiCompatibleConfig::together_ai,
            "Together AI",
        ),
        ("GROQ_API_KEY", OpenAiCompatibleConfig::groq, "Groq"),
        (
            "FIREWORKS_API_KEY",
            OpenAiCompatibleConfig::fireworks,
            "Fireworks AI",
        ),
        (
            "DEEPINFRA_API_KEY",
            OpenAiCompatibleConfig::deepinfra,
            "DeepInfra",
        ),
        (
            "CEREBRAS_API_KEY",
            OpenAiCompatibleConfig::cerebras,
            "Cerebras",
        ),
        (
            "NOVITA_API_KEY",
            OpenAiCompatibleConfig::novita,
            "Novita AI",
        ),
    ];
    for &(env_var, config_fn, display_name) in openai_compatible {
        if std::env::var(env_var).is_ok() {
            gateway.register_provider(Arc::new(OpenAiCompatibleProvider::new(config_fn())));
            info!("Registered {} provider", display_name);
        }
    }
}

/// Assemble the plugin pipeline (word filter, max-token, request logger,
/// and the env-configured budget and rate-limit plugins).
fn wire_plugins() -> himadri_plugin::PluginManager {
    let mut plugin_manager = himadri_plugin::PluginManager::new();

    // Word filter is opt-in: WORD_FILTER_BLOCKLIST is a comma-separated
    // list of blocked words. (A hardcoded default blocklist used to reject
    // any prompt containing e.g. "password" on every deployment.)
    if let Ok(blocklist) = std::env::var("WORD_FILTER_BLOCKLIST") {
        let words: Vec<String> = blocklist
            .split(',')
            .map(|w| w.trim().to_string())
            .filter(|w| !w.is_empty())
            .collect();
        if !words.is_empty() {
            info!(
                "Registered word filter with {} blocked word(s)",
                words.len()
            );
            plugin_manager.register(WordFilterPlugin::new(words));
        }
    }

    // Global max_tokens cap is opt-in via MAX_TOKENS_LIMIT (used to be a
    // hardcoded 4096 that rejected any larger request).
    if let Some(limit) = std::env::var("MAX_TOKENS_LIMIT")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
    {
        info!("Registered max-token cap of {}", limit);
        plugin_manager.register(MaxTokenPlugin::new(limit));
    }

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

    // Register the rate-limit plugin when a per-key and/or per-IP limit is
    // configured. Both scopes share one plugin, and the global limiter stays
    // unset so configuring a key/IP limit doesn't silently impose an
    // unrelated global request cap.
    let key_rpm = std::env::var("RATE_LIMIT_KEY_RPM")
        .ok()
        .and_then(|v| v.parse::<u64>().ok());
    let ip_rpm = std::env::var("RATE_LIMIT_IP_RPM")
        .ok()
        .and_then(|v| v.parse::<u64>().ok());
    if key_rpm.is_some() || ip_rpm.is_some() {
        if let Ok(rl_plugin) = RateLimitPlugin::new(RateLimitConfig {
            key_rpm,
            ip_rpm,
            ..Default::default()
        }) {
            plugin_manager.register(rl_plugin);
            if let Some(rpm) = key_rpm {
                info!("Registered rate limit: {} RPM per key", rpm);
            }
            if let Some(rpm) = ip_rpm {
                info!("Registered rate limit: {} RPM per IP", rpm);
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
    let mut admin = AdminHandlers::new(store.clone());
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
            if audience.is_empty() {
                // An empty audience makes `aud` validation reject every real
                // token — a silent auth outage. Refuse to start instead.
                tracing::error!(
                    "JWT_ISSUER is set but JWT_AUDIENCE is empty; set JWT_AUDIENCE \
                     to the expected `aud` claim (typically the OAuth client id)"
                );
                std::process::exit(1);
            }
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
) -> Result<axum::response::Response, axum::response::Response> {
    use axum::response::IntoResponse;
    match auth {
        Some(ctx) if ctx.scope == AuthScope::Admin => Ok(next.run(request).await),
        _ => Err((
            axum::http::StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({
                "error": { "message": "admin scope required", "type": "gateway_error" }
            })),
        )
            .into_response()),
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
        assert_eq!(
            status_for(Some(ctx_with(AuthScope::Admin))).await,
            StatusCode::OK
        );
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
