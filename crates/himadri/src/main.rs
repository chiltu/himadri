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
use himadri_provider::{
    AnthropicProvider, GeminiProvider, OpenAiCompatibleConfig, OpenAiCompatibleProvider,
};

use handlers::{AppState, *};
use himadri::gateway::{Gateway, OnEmpty};
use himadri::wire::providers::non_preset_env;

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

/// True when the process is expected to run with authentication enabled.
/// Set `REQUIRE_AUTH=1` explicitly, or deploy with `RUST_ENV`/`HIMADRI_ENV`
/// of `production` / `prod` / `staging`.
fn auth_is_required() -> bool {
    himadri_core::env::flag_is_truthy("REQUIRE_AUTH")
        || env_is_nondev_deployment(
            std::env::var("RUST_ENV")
                .or_else(|_| std::env::var("HIMADRI_ENV"))
                .ok()
                .as_deref(),
        )
}

fn env_is_nondev_deployment(value: Option<&str>) -> bool {
    matches!(
        value.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("production" | "prod" | "staging")
    )
}

fn master_key_configured(config: &Config) -> bool {
    config
        .admin
        .master_key
        .as_ref()
        .is_some_and(|k| !k.trim().is_empty())
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

    // Fail closed when operators have marked this as a non-dev deployment.
    // Without MASTER_KEY (and with no JWT_ISSUER / DEV_ADMIN_PASSWORD), auth
    // is fully bypassed — every request is anonymous Admin, including
    // /admin/*. Production keeps requiring MASTER_KEY outright as the
    // guaranteed root credential, independent of the other mechanisms.
    if auth_is_required() && !master_key_configured(&config) {
        eprintln!(
            "FATAL: MASTER_KEY is required when REQUIRE_AUTH=1 or \
             RUST_ENV/HIMADRI_ENV is production|prod|staging. \
             Refusing to start with authentication bypassed."
        );
        std::process::exit(1);
    }

    // Provider-routing source (HIMADRI_PROVIDER_SOURCE). Fail fast on a value
    // we don't recognize — a typo must not silently mean "auto" — and on
    // strict db-source without a database to source from.
    let provider_source = match himadri::wire::mode::ProviderSource::from_env() {
        Ok(source) => source,
        Err(e) => {
            eprintln!("FATAL: {e}");
            std::process::exit(1);
        }
    };
    if provider_source == himadri::wire::mode::ProviderSource::Db
        && std::env::var("DATABASE_URL").is_err()
    {
        eprintln!(
            "FATAL: {}=db requires DATABASE_URL — there is no database to route from.",
            himadri::wire::mode::PROVIDER_SOURCE_VAR
        );
        std::process::exit(1);
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

    let gateway = Arc::new(build_gateway(&config, provider_source).await);
    let (admin, auth) = build_admin(&config).await;

    // Strict db routing is only honored if the database can actually supply
    // it. `DATABASE_URL` being *set* is not enough: on a bad URL or an
    // unreachable server the model stores fail to connect and reads simply
    // return empty, which under strict mode — where no env providers are
    // registered — would boot a gateway that can never route and whose admin
    // writes vanish on restart.
    if provider_source == himadri::wire::mode::ProviderSource::Db && !admin.has_model_stores() {
        eprintln!(
            "FATAL: {}=db requires a working database, but the model/endpoint stores \
             could not be connected — check DATABASE_URL and that the server is reachable. \
             Refusing to start: strict mode registers no env providers, so nothing could route.",
            himadri::wire::mode::PROVIDER_SOURCE_VAR
        );
        std::process::exit(1);
    }

    // Sync routing targets from DB-registered models/endpoints, so they are
    // active immediately after a restart (previously they stayed inactive
    // until the first model/endpoint mutation triggered a rebuild). A DB
    // that produces no targets — no rows, only disabled ones, or only rows
    // the provider registry can't build — or one that can't be read right
    // now, leaves the env/file-configured targets untouched instead of
    // wiping them (`OnEmpty::KeepPrevious`).
    let db_routing_active = match (admin.list_models().await, admin.list_endpoints().await) {
        (Ok(db_models), Ok(db_endpoints)) => {
            let outcome = gateway
                .rebuild_targets_from_db(&db_models, &db_endpoints, OnEmpty::KeepPrevious)
                .await;
            if outcome.applied {
                info!(
                    "Loaded {} routing target(s) from database ({} endpoint(s) skipped)",
                    outcome.targets_built,
                    outcome.skipped.len()
                );
            } else if !outcome.skipped.is_empty() {
                // Enabled rows exist but none routes: keeping env/file targets
                // instead of wiping routing. Each skip was warned individually
                // by the rebuild; summarize the decision here.
                tracing::warn!(
                    "DB produced no routing targets ({} endpoint(s) unbuildable); keeping env/file-configured targets",
                    outcome.skipped.len()
                );
            }
            outcome.applied
        }
        (Err(e), _) | (_, Err(e)) => {
            tracing::warn!("skipping DB target sync at startup (stores unavailable): {e}");
            false
        }
    };
    log_provider_routing_mode(provider_source, db_routing_active);

    // JWT/OIDC (when JWT_ISSUER is set) runs alongside API-key auth.
    let jwt_discovery = init_jwt_discovery().await;

    // Dev/break-glass admin login (DEV_ADMIN_PASSWORD): username+password →
    // short-lived, locally signed admin JWT. For development without an OIDC
    // provider, and for regaining admin access when OIDC is down.
    let admin_login = himadri_auth::AdminLogin::from_env().map(Arc::new);
    if let Some(login) = &admin_login {
        tracing::warn!(
            "Dev admin login enabled for user '{}' (break-glass credential): \
             tokens are signed with a per-boot secret, so all sessions end on \
             restart; unset DEV_ADMIN_PASSWORD to disable",
            login.username()
        );
    }

    let combined_auth = Arc::new(combined_auth::CombinedAuth::new(
        auth.clone(),
        jwt_discovery,
        admin_login.clone(),
        Some(gateway.audit_log_arc()),
    ));

    let state = AppState {
        gateway: gateway.clone(),
        admin,
        metrics: gateway.metrics(),
        admin_login,
    };
    let app = build_router(state, &config, combined_auth);

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

/// Announce which source is providing routing targets this boot, and warn
/// about provider env config that isn't feeding routing. One line every boot,
/// so "which mode am I in" always has a visible answer instead of being
/// inferred from side effects.
fn log_provider_routing_mode(
    source: himadri::wire::mode::ProviderSource,
    db_routing_active: bool,
) {
    use himadri::wire::mode::{inert_provider_env_vars_from_env, ProviderSource};

    match source {
        ProviderSource::Db => {
            info!("Provider routing: database (strict)");
            if !db_routing_active {
                tracing::warn!(
                    "strict database routing has no targets yet; nothing will route \
                     until models/endpoints are configured via the admin API"
                );
            }
            let inert = inert_provider_env_vars_from_env();
            if !inert.is_empty() {
                tracing::warn!(
                    "provider env vars are ignored under {}=db: {}",
                    himadri::wire::mode::PROVIDER_SOURCE_VAR,
                    inert.join(", ")
                );
            }
        }
        ProviderSource::Auto if db_routing_active => {
            info!("Provider routing: database");
            let inert = inert_provider_env_vars_from_env();
            if !inert.is_empty() {
                tracing::warn!(
                    "provider env vars set but not used for routing while the database \
                     provides targets (they remain the routing fallback if it stops): {}",
                    inert.join(", ")
                );
            }
        }
        ProviderSource::Auto => {
            if std::env::var("DATABASE_URL").is_ok() {
                info!(
                    "Provider routing: environment/config (database routing configured \
                     but has no targets yet)"
                );
            } else {
                info!("Provider routing: environment/config");
            }
        }
    }
}

/// Build the gateway core: register env-configured providers, wire the
/// plugin pipeline, and attach the request-log store and response cache.
async fn build_gateway(
    config: &Config,
    provider_source: himadri::wire::mode::ProviderSource,
) -> Gateway {
    let metrics = Arc::new(Metrics::new());
    let mut gateway = Gateway::new(config.clone(), metrics.clone());
    // Strict db-source deployments must never route with env-configured
    // providers, so their clients are never registered at all.
    if provider_source == himadri::wire::mode::ProviderSource::Auto {
        register_providers_from_env(&gateway);
    }
    let wired = himadri::wire::plugins::build(
        himadri::wire::plugins::PluginSettings::from_env(),
        config,
        gateway.config_handle(),
        &metrics,
    );
    gateway.set_plugin_manager(wired.manager);
    if let Some(cache) = wired.response_cache {
        gateway.set_response_cache(cache);
    }

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


    gateway
}

/// Register every provider configured through environment variables.
/// OpenAI, Anthropic and Gemini are always registered; the rest activate
/// when their API keys / endpoints are present.
fn register_providers_from_env(gateway: &Gateway) {
    // Register OpenAI (default, supports custom base URL via OPENAI_BASE_URL)
    let mut openai_config = OpenAiCompatibleConfig::openai();
    if let Ok(base_url) = std::env::var(non_preset_env::OPENAI_BASE_URL) {
        openai_config.base_url = base_url;
        info!("OpenAI base URL overridden: {}", openai_config.base_url);
    }
    gateway.register_provider(Arc::new(OpenAiCompatibleProvider::new(openai_config)));

    // Register a secondary OpenAI-compatible upstream (for multi-endpoint /
    // failover setups). Provider name: "openai-secondary".
    if let Ok(base_url) = std::env::var(non_preset_env::OPENAI_SECONDARY_BASE_URL) {
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
        std::env::var(non_preset_env::AZURE_OPENAI_API_KEY).ok(),
        std::env::var(non_preset_env::AZURE_OPENAI_ENDPOINT).ok(),
        std::env::var(non_preset_env::AZURE_OPENAI_DEPLOYMENT).ok(),
    ) {
        let api_version = std::env::var(non_preset_env::AZURE_OPENAI_API_VERSION).ok();
        gateway.register_provider(Arc::new(OpenAiCompatibleProvider::azure(
            &api_key,
            &base_url,
            &deployment,
            api_version.as_deref().unwrap_or("2024-10-21"),
        )));
        info!("Registered Azure OpenAI provider");
    }

    // OpenAI-compatible vendors: registered when their API-key env var is set.
    //
    // The vendor list, its display names, and its type names all come from the
    // one preset table in `himadri_provider::compatible` — the same table DB
    // mode's registry builds from. Adding a row there enables the vendor in both
    // modes; there is nothing to edit here.
    //
    // `openai` is skipped: it is registered unconditionally above (with its
    // OPENAI_BASE_URL override) rather than gated on a key.
    for (provider_type, preset) in himadri_provider::compatible::presets() {
        if provider_type == "openai" {
            continue;
        }
        if std::env::var(himadri::wire::providers::api_key_env_var(provider_type)).is_err() {
            continue;
        }
        let config = preset();
        info!("Registered {} provider", config.display_name);
        gateway.register_provider(Arc::new(OpenAiCompatibleProvider::new(config)));
    }
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
        if let Some((model_store, endpoint_store)) =
            himadri_admin::connect_model_stores(&database_url, cipher).await
        {
            admin = admin.with_model_stores(model_store, endpoint_store);
            info!("Initialized model and endpoint stores");
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
                    let refresh_secs =
                        himadri_core::env::parse_var("JWT_JWKS_REFRESH_SECS").unwrap_or(3600u64);
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
    combined_auth: Arc<combined_auth::CombinedAuth>,
) -> Router {
    let public_routes = Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics_handler))
        .route("/v1/models", get(list_models))
        // Unauthenticated by design (it *is* the login); 404s unless
        // DEV_ADMIN_PASSWORD enables the dev/break-glass admin account.
        .route("/auth/admin/login", post(dev_admin_login));

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
        .route("/admin/models", get(list_models_api))
        .route("/admin/models", post(create_model))
        .route("/admin/models/{id}", get(get_model))
        .route("/admin/models/{id}", put(update_model))
        .route("/admin/models/{id}", delete(delete_model))
        .route("/admin/models/{id}/toggle", post(toggle_model))
        .route("/admin/endpoints", get(list_all_model_endpoints))
        .route(
            "/admin/models/{id}/endpoints",
            get(list_model_endpoints).post(create_model_endpoint),
        )
        .route(
            "/admin/endpoints/{id}",
            get(get_model_endpoint)
                .put(update_model_endpoint)
                .delete(delete_model_endpoint),
        )
        .route("/admin/endpoints/{id}/toggle", post(toggle_model_endpoint))
        .route("/admin/known-providers", get(known_providers))
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
        // Combined auth (not the plain API-key middleware): admin access is
        // any Admin-scope principal — master key, admin-scoped API key,
        // dev/break-glass admin JWT, or an OIDC token carrying the admin
        // role. Its bypass also stays off when DEV_ADMIN_PASSWORD or
        // JWT_ISSUER is configured, so those setups require a real login
        // even without MASTER_KEY.
        .layer(middleware::from_fn_with_state(
            combined_auth.clone(),
            combined_auth::CombinedAuth::middleware,
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
