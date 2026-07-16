//! HTTP handlers for the public, bearer-auth API, and admin routes, plus the
//! shared request helpers (client-IP resolution, error mapping, and the
//! mutate-then-rebuild-targets pattern for provider/model CRUD). Route
//! registration lives in `main.rs`.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use std::sync::Arc;

use himadri_admin::AdminHandlers;
use himadri_core::{
    AuthContext, ChatCompletionRequest, Config, GatewayError, ModelListResponse, ModelObject,
};
use himadri_observability::Metrics;

use himadri::gateway::{Gateway, OnEmpty};

/// Maximum buffered body size for the `/v1/*` passthrough proxy (10 MiB).
/// Large enough for typical multimodal/base64 payloads, bounded to prevent
/// memory-exhaustion DoS.
const MAX_PROXY_BODY_BYTES: usize = 10 * 1024 * 1024;

/// Uniform JSON error for non-`GatewayError` handler failures, so admin
/// endpoints return the same `{"error": {...}}` envelope as the /v1 API
/// instead of plain-text bodies or empty responses.
#[derive(Debug)]
pub(crate) struct ApiError(pub StatusCode, pub String);

impl ApiError {
    pub(crate) fn not_found() -> Self {
        ApiError(StatusCode::NOT_FOUND, "not found".to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Json(serde_json::json!({
            "error": { "message": self.1, "type": "gateway_error" }
        }));
        (self.0, body).into_response()
    }
}

impl From<(StatusCode, String)> for ApiError {
    fn from((status, message): (StatusCode, String)) -> Self {
        ApiError(status, message)
    }
}

/// The single place admin-store errors become HTTP statuses. `Validation`
/// and `Conflict` messages are written for the client and returned verbatim;
/// `Store` detail (SQL, connection errors) is already logged by the admin
/// facade and must not be echoed to clients.
impl From<himadri_admin::AdminError> for ApiError {
    fn from(e: himadri_admin::AdminError) -> Self {
        use himadri_admin::AdminError;
        match e {
            AdminError::NotFound => ApiError::not_found(),
            AdminError::Validation(m) => ApiError(StatusCode::BAD_REQUEST, m),
            AdminError::Conflict(m) => ApiError(StatusCode::CONFLICT, m),
            AdminError::Store(_) => ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal storage error".to_string(),
            ),
        }
    }
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) gateway: Arc<Gateway>,
    pub(crate) admin: Arc<AdminHandlers>,
    pub(crate) metrics: Arc<Metrics>,
    /// Present when the dev/break-glass admin login is enabled
    /// (`DEV_ADMIN_PASSWORD`); issues and signs the login tokens.
    pub(crate) admin_login: Option<Arc<himadri_auth::AdminLogin>>,
}

#[derive(serde::Deserialize)]
pub(crate) struct DevAdminLoginRequest {
    username: String,
    password: String,
}

/// `POST /auth/admin/login` — dev/break-glass admin login.
///
/// Verifies the `DEV_ADMIN_USERNAME`/`DEV_ADMIN_PASSWORD` credentials in
/// constant time and returns a short-lived admin JWT that the combined auth
/// middleware accepts like any other bearer token. Responds 404 when the
/// mechanism is disabled so the endpoint's existence isn't advertised, and
/// audits + rate-slows failed attempts (this is a password endpoint).
pub(crate) async fn dev_admin_login(
    State(state): State<AppState>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(body): Json<DevAdminLoginRequest>,
) -> Result<Json<himadri_auth::IssuedAdminToken>, ApiError> {
    let Some(login) = &state.admin_login else {
        return Err(ApiError::not_found());
    };

    if !login.verify(&body.username, &body.password) {
        let remote_ip = resolve_remote_ip(peer, &headers);
        state.gateway.audit_log_arc().log_auth_failure(
            himadri_observability::AuditStatus::Unauthorized,
            "dev admin login: invalid credentials",
            remote_ip,
            Some(body.username.clone()),
            None,
        );
        // Blunt brute-force damper; per-IP rate limiting would be better but
        // this endpoint only exists in dev/break-glass configurations.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        return Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "invalid username or password".to_string(),
        ));
    }

    let issued = login
        .issue()
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(issued))
}

pub(crate) async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

/// Prometheus metrics. The output includes per-model token volumes and
/// cost totals, so when a `METRICS_TOKEN` or `MASTER_KEY` is configured a
/// matching bearer token is required; only a fully unconfigured (dev-mode)
/// gateway serves metrics unauthenticated — mirroring the API's dev bypass.
pub(crate) async fn metrics_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Response {
    // The master key may come from the MASTER_KEY env var *or* the config
    // file (main.rs merges the env override into config.admin.master_key),
    // so consult the live config rather than only the environment.
    let expected =
        std::env::var("METRICS_TOKEN")
            .ok()
            .or(state.gateway.get_config().await.admin.master_key);
    if let Some(expected) = expected.filter(|t| !t.is_empty()) {
        let authorized = headers
            .get("Authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .is_some_and(|presented| constant_time_eq(presented.as_bytes(), expected.as_bytes()));
        if !authorized {
            return ApiError(StatusCode::UNAUTHORIZED, "unauthorized".to_string()).into_response();
        }
    }
    state.metrics.encode_metrics().into_response()
}

pub(crate) async fn list_models(State(state): State<AppState>) -> Json<ModelListResponse> {
    // On a store error, fall back to the env-provider catalog below rather
    // than failing the whole endpoint: /v1/models is availability-first and
    // the failure is already logged by the admin facade.
    let admin_models = state
        .admin
        .list_enabled_models_for_api()
        .await
        .unwrap_or_default();
    if !admin_models.is_empty() {
        return Json(ModelListResponse {
            object: "list".to_string(),
            data: admin_models,
        });
    }

    let providers = state.gateway.list_providers();
    let mut models = Vec::new();
    for provider_name in &providers {
        if let Some(provider) = state.gateway.get_provider(provider_name) {
            for model_id in provider.supported_models() {
                models.push(ModelObject {
                    id: model_id,
                    object: "model".to_string(),
                    created: 0,
                    owned_by: provider_name.clone(),
                });
            }
        }
    }

    Json(ModelListResponse {
        object: "list".to_string(),
        data: models,
    })
}

pub(crate) async fn chat_completions(
    State(state): State<AppState>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    axum::extract::Extension(auth): axum::extract::Extension<Option<AuthContext>>,
    Json(request): Json<ChatCompletionRequest>,
) -> Response {
    let remote_ip = resolve_remote_ip(peer, &headers);
    if request.stream {
        use futures::StreamExt;
        match state
            .gateway
            .route_stream(request, auth.as_ref(), remote_ip)
            .await
        {
            Ok(stream) => sse_response(stream.map(
                |chunk: Result<himadri_core::StreamChunk, himadri_provider::ProviderError>| {
                    chunk.and_then(|c| {
                        serde_json::to_string(&c)
                            .map_err(|e| himadri_provider::ProviderError::Parse(e.to_string()))
                    })
                },
            )),
            Err(e) => error_to_response(e),
        }
    } else {
        match state.gateway.route(request, auth.as_ref(), remote_ip).await {
            Ok(response) => Json(response).into_response(),
            Err(e) => error_to_response(e),
        }
    }
}

/// Legacy `/v1/completions` request: a `prompt` (string or array of
/// strings) instead of chat `messages`. Translated to a single-user-message
/// chat request internally; the response is converted back to the
/// `text_completion` wire shape.
#[derive(serde::Deserialize)]
pub(crate) struct LegacyCompletionRequest {
    model: String,
    #[serde(default)]
    prompt: LegacyPrompt,
    #[serde(default)]
    max_tokens: Option<u32>,
    #[serde(default)]
    temperature: Option<f32>,
    #[serde(default)]
    top_p: Option<f32>,
    #[serde(default)]
    stop: Option<serde_json::Value>,
    #[serde(default)]
    stream: bool,
    #[serde(default)]
    user: Option<String>,
}

#[derive(serde::Deserialize, Default)]
#[serde(untagged)]
pub(crate) enum LegacyPrompt {
    #[default]
    None,
    Text(String),
    Many(Vec<String>),
}

impl LegacyPrompt {
    fn into_text(self) -> String {
        match self {
            LegacyPrompt::None => String::new(),
            LegacyPrompt::Text(t) => t,
            LegacyPrompt::Many(parts) => parts.join("\n"),
        }
    }
}

fn legacy_to_chat(request: LegacyCompletionRequest) -> Result<ChatCompletionRequest, ApiError> {
    // The legacy API accepts `stop` as a bare string or an array of strings;
    // the chat type wants an array. Normalize rather than reject (and never
    // panic on user-controlled input).
    let prompt = request.prompt.into_text();
    if prompt.is_empty() {
        // A missing/empty prompt is a malformed legacy request (commonly a
        // chat-shaped body posted to the wrong endpoint); reject loudly
        // instead of silently forwarding an empty prompt upstream.
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "prompt is required (for chat-style messages use /v1/chat/completions)".to_string(),
        ));
    }
    let stop = match request.stop {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(s)) => Some(serde_json::json!([s])),
        Some(other) => Some(other),
    };
    serde_json::from_value(serde_json::json!({
        "model": request.model,
        "messages": [{ "role": "user", "content": prompt }],
        "max_tokens": request.max_tokens,
        "temperature": request.temperature,
        "top_p": request.top_p,
        "stop": stop,
        "stream": request.stream,
        "user": request.user,
    }))
    .map_err(|e| {
        ApiError(
            StatusCode::BAD_REQUEST,
            format!("invalid completion request: {}", e),
        )
    })
}

fn chat_to_text_completion(response: &himadri_core::ChatCompletionResponse) -> serde_json::Value {
    serde_json::json!({
        "id": response.id,
        "object": "text_completion",
        "created": response.created,
        "model": response.model,
        "choices": response.choices.iter().map(|c| serde_json::json!({
            "text": c.message.content.clone().unwrap_or_default(),
            "index": c.index,
            "logprobs": null,
            "finish_reason": c.finish_reason,
        })).collect::<Vec<_>>(),
        "usage": response.usage,
    })
}

pub(crate) async fn completions(
    State(state): State<AppState>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    axum::extract::Extension(auth): axum::extract::Extension<Option<AuthContext>>,
    Json(request): Json<LegacyCompletionRequest>,
) -> Response {
    let remote_ip = resolve_remote_ip(peer, &headers);
    let stream = request.stream;
    let chat_request = match legacy_to_chat(request) {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };

    if stream {
        match state
            .gateway
            .route_stream(chat_request, auth.as_ref(), remote_ip)
            .await
        {
            Ok(chunks) => {
                use futures::StreamExt;
                sse_response(chunks.map(|chunk| {
                    chunk.map(|c| {
                        serde_json::json!({
                            "id": c.id,
                            "object": "text_completion",
                            "created": c.created,
                            "model": c.model,
                            "choices": c.choices.iter().map(|choice| serde_json::json!({
                                "text": choice.delta.content.clone().unwrap_or_default(),
                                "index": choice.index,
                                "logprobs": null,
                                "finish_reason": choice.finish_reason,
                            })).collect::<Vec<_>>(),
                        })
                        .to_string()
                    })
                }))
            }
            Err(e) => error_to_response(e),
        }
    } else {
        match state
            .gateway
            .route(chat_request, auth.as_ref(), remote_ip)
            .await
        {
            Ok(response) => Json(chat_to_text_completion(&response)).into_response(),
            Err(e) => error_to_response(e),
        }
    }
}

pub(crate) async fn embeddings(
    State(state): State<AppState>,
    axum::extract::Extension(auth): axum::extract::Extension<Option<AuthContext>>,
    Json(request): Json<himadri_core::EmbeddingRequest>,
) -> Response {
    match state.gateway.embed(request, auth.as_ref()).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => error_to_response(e),
    }
}

pub(crate) async fn list_keys(
    State(state): State<AppState>,
) -> Result<Json<Vec<himadri_admin::ApiKey>>, ApiError> {
    Ok(Json(state.admin.list_keys().await?))
}

pub(crate) async fn create_key(
    State(state): State<AppState>,
    Json(request): Json<himadri_admin::CreateApiKeyRequest>,
) -> Result<(StatusCode, Json<himadri_admin::ApiKey>), ApiError> {
    state
        .admin
        .create_key(request)
        .await
        .map(|key| (StatusCode::CREATED, Json(key)))
        .map_err(ApiError::from)
}

pub(crate) async fn get_key(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<himadri_admin::ApiKey>, ApiError> {
    state
        .admin
        .get_key(&id)
        .await?
        .map(Json)
        .ok_or_else(ApiError::not_found)
}

pub(crate) async fn update_key(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<himadri_admin::UpdateApiKeyRequest>,
) -> Result<Json<himadri_admin::ApiKey>, ApiError> {
    state
        .admin
        .update_key(&id, request)
        .await?
        .map(Json)
        .ok_or_else(ApiError::not_found)
}

pub(crate) async fn delete_key(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    // A store failure must surface as a 500, not collapse into the 404 arm:
    // the row may still exist, and "not found" would tell the client the
    // delete succeeded.
    match state.admin.delete_key(&id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(ApiError::not_found()),
        Err(e) => Err(e.into()),
    }
}

pub(crate) async fn revoke_key(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    match state.admin.revoke_key(&id).await {
        Ok(true) => Ok(StatusCode::OK),
        Ok(false) => Err(ApiError::not_found()),
        Err(e) => Err(e.into()),
    }
}

pub(crate) async fn rotate_key(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<himadri_admin::ApiKey>, ApiError> {
    state
        .admin
        .rotate_key(&id)
        .await?
        .map(Json)
        .ok_or_else(ApiError::not_found)
}

pub(crate) async fn passthrough(
    State(state): State<AppState>,
    axum::extract::ConnectInfo(_peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    axum::extract::Extension(_auth): axum::extract::Extension<Option<AuthContext>>,
    req: axum::extract::Request,
) -> Response {
    let (parts, body) = req.into_parts();
    let method = parts.method.as_str().to_string();
    let uri = parts.uri.path().to_string();

    // Bound the buffered body so a single request can't exhaust memory
    // (CWE-770). Oversized bodies are rejected rather than silently
    // truncated to empty, which the previous `usize::MAX` + `unwrap_or_default`
    // would have done.
    let body_bytes = match axum::body::to_bytes(body, MAX_PROXY_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                "request body exceeds maximum allowed size",
            )
                .into_response();
        }
    };

    match state
        .gateway
        .proxy(&method, &uri, &parts.headers, body_bytes)
        .await
    {
        Ok((status, resp_headers, resp_body)) => {
            let mut response = axum::response::Response::builder().status(status);
            for (key, value) in resp_headers.iter() {
                response = response.header(key, value);
            }
            response
                .body(resp_body)
                .unwrap_or_else(|e| error_to_response(GatewayError::Internal(e.to_string())))
        }
        Err(e) => error_to_response(e),
    }
}

pub(crate) async fn reload_config(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let new_config = Config::load_from_env().map_err(|e| {
        ApiError(
            StatusCode::BAD_REQUEST,
            format!("Failed to load config: {}", e),
        )
    })?;
    state
        .gateway
        .reload_config(new_config)
        .await
        .map_err(|e| ApiError(StatusCode::BAD_REQUEST, e.to_string()))?;
    state.reassert_db_targets_after_config().await;
    Ok(Json(serde_json::json!({ "status": "reloaded" })))
}

// ─── New Admin Endpoints ─────────────────────────────────────────────

pub(crate) async fn dashboard(
    State(state): State<AppState>,
) -> Json<himadri_admin::DashboardSummary> {
    let key_count = state.admin.list_keys().await.map_or(0, |k| k.len());
    let dashboard = state.gateway.usage_store().get_dashboard(key_count);
    Json(dashboard)
}

pub(crate) async fn usage_stats(State(state): State<AppState>) -> Json<serde_json::Value> {
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

pub(crate) async fn key_usage_stats(
    State(state): State<AppState>,
    Path(key_id): Path<String>,
) -> Result<Json<himadri_admin::UsageStats>, ApiError> {
    let store = state.gateway.usage_store();
    let stats = store.get_key_stats(&key_id);
    Ok(Json(stats))
}

pub(crate) async fn get_config(State(state): State<AppState>) -> Json<himadri_core::Config> {
    let config = state.gateway.get_config().await;
    Json(config)
}

pub(crate) async fn update_config(
    State(state): State<AppState>,
    Json(new_config): Json<himadri_core::Config>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state
        .gateway
        .reload_config(new_config)
        .await
        .map_err(|e| ApiError(StatusCode::BAD_REQUEST, e.to_string()))?;
    state.reassert_db_targets_after_config().await;
    Ok(Json(serde_json::json!({ "status": "updated" })))
}

pub(crate) async fn config_history(State(state): State<AppState>) -> Json<serde_json::Value> {
    let history = state.gateway.config_history().await;
    let config = state.gateway.get_config().await;
    Json(serde_json::json!({
        "data": history,
        "summary": { "total_versions": history.len() },
        "current_config": config,
    }))
}

pub(crate) async fn config_rollback(
    State(state): State<AppState>,
    Path(version): Path<u32>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state
        .gateway
        .rollback_config(version)
        .await
        .map_err(|e| ApiError(StatusCode::BAD_REQUEST, e.to_string()))?;
    state.reassert_db_targets_after_config().await;
    Ok(Json(serde_json::json!({
        "status": "rolled_back",
        "version": version,
    })))
}

pub(crate) async fn list_logs(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<himadri_admin::RequestLogQuery>,
) -> Result<Json<himadri_admin::RequestLogListResult>, ApiError> {
    let result = state
        .gateway
        .request_log()
        .list(query)
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(result))
}

pub(crate) async fn delete_logs(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<himadri_admin::MaintenanceQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let deleted = state
        .gateway
        .request_log()
        .delete(query)
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(serde_json::json!({ "deleted": deleted })))
}

// ─── Provider / Model Handlers ───────────────────────────────────────

impl AppState {
    /// Rebuild the gateway's routing targets from the DB-backed model and
    /// endpoint stores. Must be called after any mutation of either store.
    ///
    /// If either list fails, the previous targets are kept: rebuilding from a
    /// partial read would silently wipe live routing state over a transient
    /// DB error.
    async fn rebuild_targets(&self) {
        let (models, endpoints) = match (
            self.admin.list_models().await,
            self.admin.list_endpoints().await,
        ) {
            (Ok(m), Ok(e)) => (m, e),
            _ => {
                tracing::warn!("skipping target rebuild: model/endpoint stores unavailable");
                return;
            }
        };
        // An empty result means "nothing routes" only when the database
        // already owned routing — there the operator just disabled or deleted
        // the last endpoint, and keeping the old targets would leave a
        // disabled endpoint serving traffic.
        //
        // While env/config targets are live, an empty DB must not replace
        // them: creating the first model of an onboarding flow — before it
        // has any endpoint — produces zero targets, and applying that would
        // take routing down on a mutation the operator never meant as a
        // shutdown. Skips are warned by the rebuild itself.
        let on_empty = if self.gateway.db_owns_routing().await {
            OnEmpty::Apply
        } else {
            OnEmpty::KeepPrevious
        };
        self.gateway
            .rebuild_targets_from_db(&models, &endpoints, on_empty)
            .await;
    }

    /// Re-assert DB-derived routing targets after a config apply.
    ///
    /// Applying a config (reload / update / rollback) overwrites the live
    /// targets with the config document's `targets`. When the provider/model
    /// tables are populated they are the source of truth, so a config save
    /// would otherwise silently drop every DB-registered provider/model from
    /// routing until the next provider/model mutation. Re-running the DB
    /// rebuild here keeps the two in sync. When the DB produces no targets the
    /// config-supplied targets stand (`OnEmpty::KeepPrevious`), preserving
    /// env/file-driven deployments — including when the DB holds enabled rows
    /// whose provider types the registry can't build, a case the old
    /// enabled-flags guard couldn't see and that wiped routing entirely.
    async fn reassert_db_targets_after_config(&self) {
        // A store error keeps the config-supplied targets (same reasoning as
        // `rebuild_targets`: never replace live routing over a failed read).
        let (models, endpoints) = match (
            self.admin.list_models().await,
            self.admin.list_endpoints().await,
        ) {
            (Ok(m), Ok(e)) => (m, e),
            _ => {
                tracing::warn!("skipping DB target reassert: model/endpoint stores unavailable");
                return;
            }
        };
        let outcome = self
            .gateway
            .rebuild_targets_from_db(&models, &endpoints, OnEmpty::KeepPrevious)
            .await;
        if !outcome.applied && !outcome.skipped.is_empty() {
            tracing::warn!(
                "DB produced no routing targets after config apply ({} endpoint(s) unbuildable); config targets kept",
                outcome.skipped.len()
            );
        }
    }

    /// Reject an endpoint whose `provider_type`/`base_url` pair the gateway
    /// could never build a client for.
    ///
    /// Without this the row persists and is then silently skipped on every
    /// rebuild: the endpoint shows up in the admin UI but never routes, and the
    /// only evidence is a log line. Asking the registry — the same thing rebuild
    /// asks — means the answer cannot drift from what actually routes.
    fn validate_endpoint_provider(
        &self,
        provider_type: &str,
        base_url: Option<&str>,
    ) -> Result<(), ApiError> {
        self.gateway
            .provider_registry()
            .validate(provider_type, base_url)
            .map_err(|e| {
                tracing::warn!("Rejected endpoint provider_type on write: {e}");
                ApiError(
                    StatusCode::BAD_REQUEST,
                    format!(
                        "{e}. Use a built-in provider type (see GET /admin/known-providers), \
                         or supply a base_url to route a custom vendor through a generic \
                         OpenAI-compatible client."
                    ),
                )
            })
    }

    /// 201 + entity on success (rebuilding targets); errors map per
    /// [`ApiError::from`] (400 validation, 409 conflict, 500 store).
    async fn created<T>(
        &self,
        result: Result<T, himadri_admin::AdminError>,
    ) -> Result<(StatusCode, Json<T>), ApiError> {
        match result {
            Ok(v) => {
                self.rebuild_targets().await;
                Ok((StatusCode::CREATED, Json(v)))
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Entity on success (rebuilding targets), 404 when the id didn't match,
    /// errors per [`ApiError::from`].
    async fn updated<T>(
        &self,
        result: Result<Option<T>, himadri_admin::AdminError>,
    ) -> Result<Json<T>, ApiError> {
        match result {
            Ok(Some(v)) => {
                self.rebuild_targets().await;
                Ok(Json(v))
            }
            Ok(None) => Err(ApiError::not_found()),
            Err(e) => Err(e.into()),
        }
    }

    /// 204 on success (rebuilding targets), 404 when the id didn't match,
    /// 409 Conflict — carrying the guard's message — when a validation guard
    /// blocked the delete (e.g. "model is enabled"), and 500 when the store
    /// itself failed. Distinguishing the last two was the point of
    /// `AdminError`: a DB outage must not masquerade as a client mistake.
    async fn deleted(
        &self,
        result: Result<bool, himadri_admin::AdminError>,
    ) -> Result<StatusCode, ApiError> {
        match result {
            Ok(true) => {
                self.rebuild_targets().await;
                Ok(StatusCode::NO_CONTENT)
            }
            Ok(false) => Err(ApiError::not_found()),
            Err(e) => Err(e.into()),
        }
    }
}

/// Body of the provider/model toggle endpoints. Type-checked by the
/// extractor instead of hand-walking a `serde_json::Value`.
#[derive(serde::Deserialize)]
pub(crate) struct ToggleBody {
    #[serde(default = "default_true")]
    enabled: bool,
}

fn default_true() -> bool {
    true
}

pub(crate) async fn list_models_api(
    State(state): State<AppState>,
) -> Result<Json<Vec<himadri_admin::Model>>, ApiError> {
    Ok(Json(state.admin.list_models().await?))
}

pub(crate) async fn create_model(
    State(state): State<AppState>,
    Json(request): Json<himadri_admin::CreateModelRequest>,
) -> Result<(StatusCode, Json<himadri_admin::Model>), ApiError> {
    let result = state.admin.create_model(request).await;
    state.created(result).await
}

pub(crate) async fn get_model(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<himadri_admin::Model>, ApiError> {
    state
        .admin
        .get_model(&id)
        .await?
        .map(Json)
        .ok_or_else(ApiError::not_found)
}

pub(crate) async fn update_model(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<himadri_admin::UpdateModelRequest>,
) -> Result<Json<himadri_admin::Model>, ApiError> {
    let result = state.admin.update_model(&id, request).await;
    state.updated(result).await
}

pub(crate) async fn delete_model(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let deleted = state.admin.delete_model(&id).await;
    state.deleted(deleted).await
}

pub(crate) async fn toggle_model(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<ToggleBody>,
) -> Result<Json<himadri_admin::Model>, ApiError> {
    let result = state.admin.toggle_model(&id, body.enabled).await;
    state.updated(result).await
}

// ─── Model Endpoint Handlers ─────────────────────────────────────────

/// Strip the (decrypted) API key before returning an endpoint over HTTP. The
/// admin UI never needs the key back — it only writes it — so redacting here
/// avoids echoing credentials to every list/get caller.
fn redact_endpoint(mut e: himadri_admin::ModelEndpoint) -> himadri_admin::ModelEndpoint {
    if e.api_key.is_some() {
        e.api_key = None;
    }
    e
}

/// All endpoints across every model (keys redacted). Lets the UI compute which
/// models are active and enumerate providers in use without an N+1 fetch.
pub(crate) async fn list_all_model_endpoints(
    State(state): State<AppState>,
) -> Result<Json<Vec<himadri_admin::ModelEndpoint>>, ApiError> {
    Ok(Json(
        state
            .admin
            .list_endpoints()
            .await?
            .into_iter()
            .map(redact_endpoint)
            .collect(),
    ))
}

pub(crate) async fn list_model_endpoints(
    State(state): State<AppState>,
    Path(model_id): Path<String>,
) -> Result<Json<Vec<himadri_admin::ModelEndpoint>>, ApiError> {
    Ok(Json(
        state
            .admin
            .list_endpoints_by_model(&model_id)
            .await?
            .into_iter()
            .map(redact_endpoint)
            .collect(),
    ))
}

pub(crate) async fn create_model_endpoint(
    State(state): State<AppState>,
    Path(model_id): Path<String>,
    Json(request): Json<himadri_admin::CreateModelEndpointRequest>,
) -> Result<(StatusCode, Json<himadri_admin::ModelEndpoint>), ApiError> {
    state.validate_endpoint_provider(&request.provider_type, request.base_url.as_deref())?;

    // Redact before the HTTP response — create may return a plaintext key for
    // internal use, but clients (and logs that capture bodies) must never see it.
    let result = state
        .admin
        .create_endpoint(&model_id, request)
        .await
        .map(redact_endpoint);
    state.created(result).await
}

pub(crate) async fn get_model_endpoint(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<himadri_admin::ModelEndpoint>, ApiError> {
    state
        .admin
        .get_endpoint(&id)
        .await?
        .map(redact_endpoint)
        .map(Json)
        .ok_or_else(ApiError::not_found)
}

pub(crate) async fn update_model_endpoint(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<himadri_admin::UpdateModelEndpointRequest>,
) -> Result<Json<himadri_admin::ModelEndpoint>, ApiError> {
    // Validate the pair the update will actually produce, not the request fields
    // in isolation. Checking those alone is wrong in both directions: a
    // base_url-only edit (including `Some(None)`, which clears it) would skip the
    // check entirely, and switching provider_type to a custom vendor would be
    // rejected for lacking a base_url the stored row already carries.
    //
    // `effective_routing_pair` is the same merge both store backends persist
    // with, so validation and the write cannot disagree about it.
    //
    // Not atomic with the write: two concurrent updates can each validate
    // against a pre-merge snapshot and still land an unroutable pair. Closing
    // that needs validation inside the store's write transaction (its error
    // type would have to carry a client-visible rejection, not just
    // `sqlx::Error` → 500), and a process-local lock would not close it across
    // replicas anyway. The rebuild's skip-and-warn is the backstop.
    let current = state
        .admin
        .get_endpoint(&id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let (provider_type, base_url) = request.effective_routing_pair(&current);
    state.validate_endpoint_provider(&provider_type, base_url.as_deref())?;

    let result = state
        .admin
        .update_endpoint(&id, request)
        .await
        .map(|opt| opt.map(redact_endpoint));
    state.updated(result).await
}

pub(crate) async fn delete_model_endpoint(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let result = state.admin.delete_endpoint(&id).await;
    state.deleted(result).await
}

pub(crate) async fn toggle_model_endpoint(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<ToggleBody>,
) -> Result<Json<himadri_admin::ModelEndpoint>, ApiError> {
    // Enabling re-creates exactly the state create/update reject: a row the
    // registry can't build shows as enabled in the UI and is then skipped on
    // every rebuild, with only a log line as evidence. Disabling needs no
    // check — and must keep working, so an unroutable legacy row can still be
    // turned off.
    if body.enabled {
        let current = state
            .admin
            .get_endpoint(&id)
            .await?
            .ok_or_else(ApiError::not_found)?;
        state.validate_endpoint_provider(&current.provider_type, current.base_url.as_deref())?;
    }

    let result = state
        .admin
        .toggle_endpoint(&id, body.enabled)
        .await
        .map(|opt| opt.map(redact_endpoint));
    state.updated(result).await
}

/// Provider types with a built-in preset, for the admin UI's provider picker.
/// Served from the shared registry so the UI can't drift from what the
/// gateway actually routes.
pub(crate) async fn known_providers() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "data": himadri_core::KNOWN_PROVIDER_TYPES }))
}

/// Build an OpenAI-wire-compatible SSE response from a chunk stream:
/// chunks as `data:` events, a terminal error event (sanitized, then the
/// stream ends — clients must not receive further chunks after an error),
/// and the `data: [DONE]` sentinel OpenAI SDKs use as the end-of-stream
/// signal.
fn sse_response<S>(stream: S) -> Response
where
    S: futures::Stream<Item = Result<String, himadri_provider::ProviderError>> + Send + 'static,
{
    use axum::response::sse::{Event, KeepAlive, Sse};
    use futures::StreamExt;
    use std::convert::Infallible;

    let event_stream = stream
        .scan(false, |errored, chunk| {
            let item = if *errored {
                None
            } else {
                Some(match chunk {
                    Ok(payload) => Ok::<_, Infallible>(Event::default().data(payload)),
                    Err(e) => {
                        *errored = true;
                        tracing::error!(error = %e, "stream error");
                        let error_data = serde_json::json!({
                            "error": {
                                "message": "stream interrupted by upstream error",
                                "type": "gateway_error"
                            }
                        });
                        Ok(Event::default().data(error_data.to_string()))
                    }
                })
            };
            futures::future::ready(item)
        })
        .chain(futures::stream::once(async {
            Ok::<_, Infallible>(Event::default().data("[DONE]"))
        }));

    Sse::new(event_stream)
        .keep_alive(
            KeepAlive::new()
                .interval(std::time::Duration::from_secs(15))
                .text("ping"),
        )
        .into_response()
}

/// Constant-time comparison, mirroring the admin auth middleware, so the
/// metrics token can't be probed byte-by-byte via response timing.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn error_to_response(e: GatewayError) -> Response {
    let status = StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    // 4xx messages are actionable for the caller; 5xx detail (upstream
    // bodies, infrastructure specifics) is logged server-side and replaced
    // with a generic message so nothing internal leaks through the edge.
    let message = if status.is_server_error() {
        tracing::error!(error = %e, "gateway error");
        match &e {
            GatewayError::CircuitOpen(_) | GatewayError::ServiceUnavailable(_) => {
                "upstream provider unavailable".to_string()
            }
            _ => "internal server error".to_string(),
        }
    } else {
        e.to_string()
    };
    let body = Json(serde_json::json!({
        "error": { "message": message, "type": "gateway_error" }
    }));
    let mut response = (status, body).into_response();
    // Clients need backoff guidance on 429s.
    let retry_after_secs = match &e {
        GatewayError::RateLimited { retry_after_secs } => Some(*retry_after_secs),
        GatewayError::QuotaExceeded(_) => Some(60),
        _ => None,
    };
    if let Some(secs) = retry_after_secs {
        if let Ok(v) = axum::http::HeaderValue::from_str(&secs.to_string()) {
            response
                .headers_mut()
                .insert(axum::http::header::RETRY_AFTER, v);
        }
    }
    response
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
    if himadri_core::ip_is_internal(peer_ip) {
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
                    if !himadri_core::ip_is_internal(addr) {
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
                if !himadri_core::ip_is_internal(addr) {
                    return Some(addr.to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod endpoint_provider_validation_tests {
    use super::*;

    /// Real `AppState` over a temp sqlite store, plus a model to hang endpoints
    /// off. The e2e suites wrap `AdminHandlers` directly, so they never exercise
    /// these handlers — this covers them.
    async fn state_with_model() -> (AppState, String) {
        let db_path =
            std::env::temp_dir().join(format!("himadri-epval-{}.db", uuid::Uuid::new_v4()));
        let db_url = format!("sqlite://{}", db_path.display());

        let sqlite = himadri_admin::store::SqliteStore::new(&db_url)
            .await
            .expect("sqlite store should connect");
        let (model_store, endpoint_store) = himadri_admin::connect_model_stores(&db_url, None)
            .await
            .expect("sqlite model stores should connect");
        let admin = AdminHandlers::new(himadri_admin::StoreBackend::Sqlite(Arc::new(sqlite)))
            .with_model_stores(model_store, endpoint_store);

        let gateway = Arc::new(Gateway::new(Config::default(), Arc::new(Metrics::new())));
        let state = AppState {
            metrics: gateway.metrics(),
            gateway,
            admin: Arc::new(admin),
            admin_login: None,
        };

        let model = state
            .admin
            .create_model(himadri_admin::CreateModelRequest {
                name: "m1".to_string(),
                display_name: None,
                enabled: true,
            })
            .await
            .expect("model create should succeed");

        (state, model.id)
    }

    /// Targets reduced to their identity, since `Target` isn't `PartialEq`.
    fn snapshot(targets: &[himadri_core::Target]) -> Vec<(Option<String>, String)> {
        targets
            .iter()
            .map(|t| (t.id.clone(), t.provider.clone()))
            .collect()
    }

    fn create_req(
        provider_type: &str,
        base_url: Option<&str>,
    ) -> himadri_admin::CreateModelEndpointRequest {
        himadri_admin::CreateModelEndpointRequest {
            provider_type: provider_type.to_string(),
            base_url: base_url.map(str::to_string),
            api_key: Some("sk-test".to_string()),
            weight: 1.0,
            enabled: true,
        }
    }

    async fn create(
        state: &AppState,
        model_id: &str,
        provider_type: &str,
        base_url: Option<&str>,
    ) -> Result<himadri_admin::ModelEndpoint, ApiError> {
        create_model_endpoint(
            State(state.clone()),
            Path(model_id.to_string()),
            Json(create_req(provider_type, base_url)),
        )
        .await
        .map(|(_, Json(ep))| ep)
    }

    #[tokio::test]
    async fn create_rejects_unregistered_type_without_base_url() {
        let (state, model_id) = state_with_model().await;

        let err = create(&state, &model_id, "mystery-vendor", None)
            .await
            .expect_err("an unroutable endpoint must not be created");

        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(
            err.1.contains("mystery-vendor"),
            "message should name the offending type, got: {}",
            err.1
        );
    }

    #[tokio::test]
    async fn create_accepts_builtin_type_and_custom_vendor_with_base_url() {
        let (state, model_id) = state_with_model().await;

        assert!(create(&state, &model_id, "openai", None).await.is_ok());
        assert!(
            create(&state, &model_id, "my-vllm", Some("https://vllm.example/v1"))
                .await
                .is_ok(),
            "a custom vendor with an explicit base_url is routable"
        );
    }

    /// The pair that matters is the one the update *produces*. Clearing the
    /// base_url of a custom vendor leaves it unroutable, so it must be rejected —
    /// a check that only looked at `request.provider_type` would miss this
    /// entirely, since provider_type isn't being changed.
    #[tokio::test]
    async fn update_rejects_clearing_the_base_url_a_custom_vendor_depends_on() {
        let (state, model_id) = state_with_model().await;
        let ep = create(&state, &model_id, "my-vllm", Some("https://vllm.example/v1"))
            .await
            .expect("custom vendor with base_url should create");

        let err = update_model_endpoint(
            State(state.clone()),
            Path(ep.id.clone()),
            Json(himadri_admin::UpdateModelEndpointRequest {
                base_url: Some(None), // clear it
                ..Default::default()
            }),
        )
        .await
        .expect_err("clearing the base_url must not leave an unroutable endpoint");

        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    /// The mirror case: switching to a custom vendor while the stored base_url
    /// stays put is valid. Validating `request.provider_type` against
    /// `request.base_url` (which is None here, meaning "unchanged") would reject
    /// this for a missing base_url the row already has.
    #[tokio::test]
    async fn update_accepts_switching_type_when_the_stored_base_url_still_serves_it() {
        let (state, model_id) = state_with_model().await;
        let ep = create(&state, &model_id, "openai", Some("https://proxy.example/v1"))
            .await
            .expect("openai with a base_url override should create");

        let updated = update_model_endpoint(
            State(state.clone()),
            Path(ep.id.clone()),
            Json(himadri_admin::UpdateModelEndpointRequest {
                provider_type: Some("my-vllm".to_string()),
                ..Default::default() // base_url untouched
            }),
        )
        .await
        .expect("the stored base_url still makes this routable");

        assert_eq!(updated.0.provider_type, "my-vllm");
        assert_eq!(
            updated.0.base_url.as_deref(),
            Some("https://proxy.example/v1")
        );
    }

    /// First test for `reassert_db_targets_after_config` (three call sites,
    /// previously untested). The DB holds an enabled model with an enabled but
    /// unbuildable endpoint — created through `state.admin` directly, which
    /// bypasses the handler's registry validation exactly like a row predating
    /// it or one written straight to the DB. The old `db_has_active_targets`
    /// guard approved this shape and the rebuild then wiped the config's own
    /// targets; now the empty result must leave them standing.
    #[tokio::test]
    async fn reassert_keeps_config_targets_when_db_rows_are_unbuildable() {
        let (state, model_id) = state_with_model().await;

        state
            .admin
            .create_endpoint(&model_id, create_req("mystery-vendor", None))
            .await
            .expect("direct store write bypasses handler validation");

        let before = snapshot(&state.gateway.config_handle().read().await.targets);
        assert!(!before.is_empty(), "default config must supply a target");

        state.reassert_db_targets_after_config().await;

        let after = snapshot(&state.gateway.config_handle().read().await.targets);
        assert_eq!(
            after, before,
            "an unbuildable-only DB must not replace config targets"
        );
    }

    /// Onboarding, the natural first step: an env-routed gateway with an empty
    /// database, and the operator creates their first model — which has no
    /// endpoint yet, so the rebuild computes zero targets. Applying that would
    /// wipe the env/config targets and take routing down on a mutation the
    /// operator never meant as a shutdown.
    #[tokio::test]
    async fn creating_a_model_before_its_endpoint_keeps_env_routing() {
        // `state_with_model` writes through the store, so no rebuild has run:
        // the gateway is still on its config/env targets.
        let (state, _) = state_with_model().await;
        let before = snapshot(&state.gateway.config_handle().read().await.targets);
        assert!(!before.is_empty(), "default config must supply an env target");
        assert!(
            !state.gateway.db_owns_routing().await,
            "precondition: env/config owns routing"
        );

        // The handler path is what triggers the rebuild.
        create_model(
            State(state.clone()),
            Json(himadri_admin::CreateModelRequest {
                name: "second-model".to_string(),
                display_name: None,
                enabled: true,
            }),
        )
        .await
        .map(|(_, Json(m))| m)
        .expect("creating a model should succeed");

        let after = snapshot(&state.gateway.config_handle().read().await.targets);
        assert_eq!(
            after, before,
            "an endpoint-less model must not wipe env/config routing"
        );
    }

    /// The mirror: once the database owns routing, disabling the last endpoint
    /// is a real "nothing routes" and must apply — keeping the previous targets
    /// would leave a disabled endpoint serving traffic.
    #[tokio::test]
    async fn disabling_the_last_endpoint_empties_db_routing() {
        let (state, model_id) = state_with_model().await;
        let ep = create(&state, &model_id, "openai", None)
            .await
            .expect("endpoint create should succeed");
        assert!(
            state.gateway.db_owns_routing().await,
            "precondition: the database owns routing once it has an endpoint"
        );

        toggle_model_endpoint(
            State(state.clone()),
            Path(ep.id.clone()),
            Json(ToggleBody { enabled: false }),
        )
        .await
        .map(|Json(ep)| ep)
        .expect("disabling should succeed");

        assert!(
            state
                .gateway
                .config_handle()
                .read()
                .await
                .targets
                .is_empty(),
            "disabling the last endpoint must empty routing, not leave it serving"
        );
    }

    /// A legacy row the registry can't build must not become enableable through
    /// the toggle route — that re-creates the "enabled in the UI, skipped on
    /// every rebuild" state the create/update validation exists to prevent.
    /// Disabling one must stay possible.
    #[tokio::test]
    async fn toggle_rejects_enabling_an_unroutable_endpoint_but_allows_disabling() {
        let (state, model_id) = state_with_model().await;
        // Written through the store, bypassing the handler's validation — the
        // shape a row predating that check (or a direct DB write) has.
        let ep = state
            .admin
            .create_endpoint(&model_id, create_req("mystery-vendor", None))
            .await
            .expect("direct store write bypasses handler validation");

        let err = toggle_model_endpoint(
            State(state.clone()),
            Path(ep.id.clone()),
            Json(ToggleBody { enabled: true }),
        )
        .await
        .expect_err("enabling an unroutable endpoint must be rejected");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(
            err.1.contains("mystery-vendor"),
            "message should name the offending type, got: {}",
            err.1
        );

        toggle_model_endpoint(
            State(state.clone()),
            Path(ep.id.clone()),
            Json(ToggleBody { enabled: false }),
        )
        .await
        .map(|Json(ep)| ep)
        .expect("disabling an unroutable row must stay possible");
    }

    /// A rejected write must not reach the store.
    #[tokio::test]
    async fn rejected_create_persists_nothing() {
        let (state, model_id) = state_with_model().await;

        let _ = create(&state, &model_id, "mystery-vendor", None).await;

        let endpoints = state
            .admin
            .list_endpoints_by_model(&model_id)
            .await
            .expect("listing should succeed");
        assert!(
            endpoints.is_empty(),
            "a rejected endpoint must not be persisted, found: {endpoints:?}"
        );
    }
}

#[cfg(test)]
mod dev_admin_login_tests {
    use super::*;
    use axum::extract::ConnectInfo;

    async fn state(login: Option<Arc<himadri_auth::AdminLogin>>) -> AppState {
        let gateway = Arc::new(Gateway::new(Config::default(), Arc::new(Metrics::new())));
        AppState {
            metrics: gateway.metrics(),
            gateway,
            admin: Arc::new(AdminHandlers::new(himadri_admin::StoreBackend::new().await)),
            admin_login: login,
        }
    }

    fn peer() -> ConnectInfo<std::net::SocketAddr> {
        ConnectInfo("127.0.0.1:9999".parse().unwrap())
    }

    fn req(username: &str, password: &str) -> Json<DevAdminLoginRequest> {
        Json(DevAdminLoginRequest {
            username: username.to_string(),
            password: password.to_string(),
        })
    }

    /// Disabled (no DEV_ADMIN_PASSWORD) → 404, so the endpoint's existence
    /// isn't advertised on production deployments.
    #[tokio::test]
    async fn responds_not_found_when_disabled() {
        let state = state(None).await;
        let err = dev_admin_login(
            State(state),
            peer(),
            axum::http::HeaderMap::new(),
            req("admin", "hunter2"),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn rejects_bad_credentials_with_401() {
        let login = Arc::new(himadri_auth::AdminLogin::new(
            "admin".to_string(),
            "hunter2".to_string(),
            3600,
        ));
        let state = state(Some(login)).await;
        let err = dev_admin_login(
            State(state),
            peer(),
            axum::http::HeaderMap::new(),
            req("admin", "wrong"),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::UNAUTHORIZED);
    }

    /// Valid credentials return a bearer token that the same AdminLogin
    /// instance (i.e. the combined auth middleware) validates as an
    /// Admin-scope principal.
    #[tokio::test]
    async fn issues_admin_token_for_valid_credentials() {
        let login = Arc::new(himadri_auth::AdminLogin::new(
            "admin".to_string(),
            "hunter2".to_string(),
            3600,
        ));
        let state = state(Some(login.clone())).await;
        let Json(issued) = dev_admin_login(
            State(state),
            peer(),
            axum::http::HeaderMap::new(),
            req("admin", "hunter2"),
        )
        .await
        .unwrap();

        assert_eq!(issued.token_type, "Bearer");
        let ctx = login
            .validate(&issued.access_token)
            .expect("issued token must validate")
            .into_auth_context();
        assert_eq!(ctx.scope, himadri_core::AuthScope::Admin);
    }
}
