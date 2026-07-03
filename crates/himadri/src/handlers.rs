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

use crate::gateway::Gateway;

/// Maximum buffered body size for the `/v1/*` passthrough proxy (10 MiB).
/// Large enough for typical multimodal/base64 payloads, bounded to prevent
/// memory-exhaustion DoS.
const MAX_PROXY_BODY_BYTES: usize = 10 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) gateway: Arc<Gateway>,
    pub(crate) admin: Arc<AdminHandlers>,
    pub(crate) metrics: Arc<Metrics>,
}

pub(crate) async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

pub(crate) async fn metrics_handler(State(state): State<AppState>) -> String {
    state.metrics.encode_metrics()
}

pub(crate) async fn list_models(State(state): State<AppState>) -> Json<ModelListResponse> {
    let admin_models = state.admin.list_enabled_models_for_api().await;
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

pub(crate) async fn completions(
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
) -> Result<Json<Vec<himadri_admin::ApiKey>>, (StatusCode, String)> {
    Ok(Json(state.admin.list_keys().await))
}

pub(crate) async fn create_key(
    State(state): State<AppState>,
    Json(request): Json<himadri_admin::CreateApiKeyRequest>,
) -> Result<(StatusCode, Json<himadri_admin::ApiKey>), (StatusCode, String)> {
    state
        .admin
        .create_key(request)
        .await
        .map(|key| (StatusCode::CREATED, Json(key)))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

pub(crate) async fn get_key(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<himadri_admin::ApiKey>, StatusCode> {
    state
        .admin
        .get_key(&id)
        .await
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

pub(crate) async fn update_key(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<himadri_admin::UpdateApiKeyRequest>,
) -> Result<Json<himadri_admin::ApiKey>, StatusCode> {
    state
        .admin
        .update_key(&id, request)
        .await
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

pub(crate) async fn delete_key(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    if state.admin.delete_key(&id).await {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

pub(crate) async fn revoke_key(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    if state.admin.revoke_key(&id).await {
        Ok(StatusCode::OK)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

pub(crate) async fn rotate_key(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<himadri_admin::ApiKey>, StatusCode> {
    state
        .admin
        .rotate_key(&id)
        .await
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
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
                .body(axum::body::Body::from(resp_body))
                .unwrap_or_else(|e| error_to_response(GatewayError::Internal(e.to_string())))
        }
        Err(e) => error_to_response(e),
    }
}

pub(crate) async fn reload_config(
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

pub(crate) async fn dashboard(
    State(state): State<AppState>,
) -> Json<himadri_admin::DashboardSummary> {
    let key_count = state.admin.list_keys().await.len();
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
) -> Result<Json<himadri_admin::UsageStats>, StatusCode> {
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
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .gateway
        .reload_config(new_config)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
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
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .gateway
        .rollback_config(version)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(Json(serde_json::json!({
        "status": "rolled_back",
        "version": version,
    })))
}

pub(crate) async fn list_logs(
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

pub(crate) async fn delete_logs(
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

// ─── Provider / Model Handlers ───────────────────────────────────────

impl AppState {
    /// Rebuild the gateway's routing targets from the DB-backed provider and
    /// model stores. Must be called after any mutation of either store.
    async fn rebuild_targets(&self) {
        let providers = self.admin.list_providers().await;
        let models = self.admin.list_models().await;
        self.gateway
            .rebuild_targets_from_db(&providers, &models)
            .await;
    }

    /// 201 + entity on success (rebuilding targets), 500 with `label` on failure.
    async fn created<T>(
        &self,
        result: Option<T>,
        label: &str,
    ) -> Result<(StatusCode, Json<T>), (StatusCode, String)> {
        match result {
            Some(v) => {
                self.rebuild_targets().await;
                Ok((StatusCode::CREATED, Json(v)))
            }
            None => Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to create {}", label),
            )),
        }
    }

    /// Entity on success (rebuilding targets), 404 when the id didn't match.
    async fn updated<T>(&self, result: Option<T>) -> Result<Json<T>, StatusCode> {
        match result {
            Some(v) => {
                self.rebuild_targets().await;
                Ok(Json(v))
            }
            None => Err(StatusCode::NOT_FOUND),
        }
    }

    /// 204 on success (rebuilding targets), 404 when the id didn't match.
    async fn deleted(&self, deleted: bool) -> Result<StatusCode, StatusCode> {
        if deleted {
            self.rebuild_targets().await;
            Ok(StatusCode::NO_CONTENT)
        } else {
            Err(StatusCode::NOT_FOUND)
        }
    }
}

fn parse_enabled(body: &serde_json::Value) -> bool {
    body.get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
}

pub(crate) async fn list_providers(
    State(state): State<AppState>,
) -> Json<Vec<himadri_admin::Provider>> {
    Json(state.admin.list_providers().await)
}

pub(crate) async fn create_provider(
    State(state): State<AppState>,
    Json(request): Json<himadri_admin::CreateProviderRequest>,
) -> Result<(StatusCode, Json<himadri_admin::Provider>), (StatusCode, String)> {
    let result = state.admin.create_provider(request).await;
    state.created(result, "provider").await
}

pub(crate) async fn get_provider(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<himadri_admin::Provider>, StatusCode> {
    state
        .admin
        .get_provider(&id)
        .await
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

pub(crate) async fn update_provider(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<himadri_admin::UpdateProviderRequest>,
) -> Result<Json<himadri_admin::Provider>, StatusCode> {
    let result = state.admin.update_provider(&id, request).await;
    state.updated(result).await
}

pub(crate) async fn delete_provider(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    let deleted = state.admin.delete_provider(&id).await;
    state.deleted(deleted).await
}

pub(crate) async fn toggle_provider(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<himadri_admin::Provider>, StatusCode> {
    let result = state.admin.toggle_provider(&id, parse_enabled(&body)).await;
    state.updated(result).await
}

pub(crate) async fn list_models_api(
    State(state): State<AppState>,
) -> Json<Vec<himadri_admin::Model>> {
    Json(state.admin.list_models().await)
}

pub(crate) async fn create_model(
    State(state): State<AppState>,
    Json(request): Json<himadri_admin::CreateModelRequest>,
) -> Result<(StatusCode, Json<himadri_admin::Model>), (StatusCode, String)> {
    let result = state.admin.create_model(request).await;
    state.created(result, "model").await
}

pub(crate) async fn get_model(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<himadri_admin::Model>, StatusCode> {
    state
        .admin
        .get_model(&id)
        .await
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

pub(crate) async fn update_model(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<himadri_admin::UpdateModelRequest>,
) -> Result<Json<himadri_admin::Model>, StatusCode> {
    let result = state.admin.update_model(&id, request).await;
    state.updated(result).await
}

pub(crate) async fn delete_model(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    let deleted = state.admin.delete_model(&id).await;
    state.deleted(deleted).await
}

pub(crate) async fn toggle_model(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<himadri_admin::Model>, StatusCode> {
    let result = state.admin.toggle_model(&id, parse_enabled(&body)).await;
    state.updated(result).await
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
