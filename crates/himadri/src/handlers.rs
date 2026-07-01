use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response, Sse},
    Json,
};
use std::convert::Infallible;
use std::sync::Arc;
use tokio_stream::StreamExt;

use himadri_core::{
    AuthContext, ChatCompletionRequest, Config, GatewayError, ModelListResponse, ModelObject,
};

/// Check if an IP address is loopback or private (RFC 1918 / link-local / loopback).
fn is_private_or_loopback(ip: std::net::IpAddr) -> bool {
    ip.is_loopback()
        || match ip {
            std::net::IpAddr::V4(v4) => {
                v4.is_private()
                    || v4.is_link_local()
                    || v4.is_unspecified()
                    || v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64
                    || v4.octets()[0] == 169 && v4.octets()[1] == 254
            }
            std::net::IpAddr::V6(v6) => {
                v6.is_loopback()
                    || v6.is_unspecified()
                    || v6.is_unique_local()
                    || v6.is_unicast_link_local()
            }
        }
}

/// Resolve the client's IP address from TCP peer and proxy headers.
pub(crate) fn resolve_remote_ip(
    peer: std::net::SocketAddr,
    headers: &axum::http::HeaderMap,
) -> Option<String> {
    let peer_ip = peer.ip();
    if is_private_or_loopback(peer_ip) {
        if let Some(ip) = trusted_proxy_ip(headers) {
            return Some(ip);
        }
    }
    Some(peer_ip.to_string())
}

fn trusted_proxy_ip(headers: &axum::http::HeaderMap) -> Option<String> {
    if let Some(val) = headers.get("x-forwarded-for") {
        if let Ok(s) = val.to_str() {
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

use crate::gateway::Gateway;

pub struct Routes {
    gateway: Arc<Gateway>,
}

impl Routes {
    pub fn new(gateway: Arc<Gateway>) -> Self {
        Self { gateway }
    }

    pub async fn health() -> Json<serde_json::Value> {
        Json(serde_json::json!({
            "status": "ok",
            "version": env!("CARGO_PKG_VERSION")
        }))
    }

    pub async fn list_models(State(routes): State<Arc<Self>>) -> Json<ModelListResponse> {
        let providers = routes.gateway.list_providers();
        let mut models = Vec::new();

        for provider_name in &providers {
            if let Some(provider) = routes.gateway.get_provider(provider_name) {
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

    pub async fn chat_completions(
        State(routes): State<Arc<Self>>,
        axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
        headers: axum::http::HeaderMap,
        axum::extract::Extension(auth): axum::extract::Extension<Option<AuthContext>>,
        Json(request): Json<ChatCompletionRequest>,
    ) -> Response {
        let remote_ip = resolve_remote_ip(peer, &headers);
        if request.stream {
            match routes
                .gateway
                .route_stream(request, auth.as_ref(), remote_ip)
                .await
            {
                Ok(stream) => {
                    let event_stream = stream.filter_map(|chunk| match chunk {
                        Ok(chunk) => {
                            let data = serde_json::to_string(&chunk).unwrap_or_default();
                            Some(Ok::<_, Infallible>(
                                axum::response::sse::Event::default().data(data),
                            ))
                        }
                        Err(e) => {
                            let error_data = serde_json::json!({
                                "error": {
                                    "message": e.to_string(),
                                    "type": "gateway_error"
                                }
                            });
                            Some(Ok(
                                axum::response::sse::Event::default().data(error_data.to_string())
                            ))
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
            match routes
                .gateway
                .route(request, auth.as_ref(), remote_ip)
                .await
            {
                Ok(response) => Json(response).into_response(),
                Err(e) => error_to_response(e),
            }
        }
    }

    pub async fn completions(
        State(routes): State<Arc<Self>>,
        axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
        headers: axum::http::HeaderMap,
        axum::extract::Extension(auth): axum::extract::Extension<Option<AuthContext>>,
        Json(request): Json<ChatCompletionRequest>,
    ) -> Response {
        let remote_ip = resolve_remote_ip(peer, &headers);
        match routes
            .gateway
            .route(request, auth.as_ref(), remote_ip)
            .await
        {
            Ok(response) => Json(response).into_response(),
            Err(e) => error_to_response(e),
        }
    }

    pub async fn metrics(State(_routes): State<Arc<Self>>) -> String {
        "himadri_requests_total 0\n".to_string()
    }

    pub async fn passthrough(
        State(_routes): State<Arc<Self>>,
        _request: axum::extract::Request,
    ) -> Response {
        error_to_response(GatewayError::NotFound(
            "Endpoint not implemented".to_string(),
        ))
    }

    // ─── Admin Endpoints ─────────────────────────────────────────────

    pub async fn dashboard(
        State(routes): State<Arc<Self>>,
    ) -> Json<himadri_admin::DashboardSummary> {
        let key_count = routes.gateway.list_providers().len();
        let dashboard = routes.gateway.usage_store().get_dashboard(key_count);
        Json(dashboard)
    }

    pub async fn usage_stats(State(routes): State<Arc<Self>>) -> Json<serde_json::Value> {
        let store = routes.gateway.usage_store();
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

    pub async fn key_usage_stats(
        State(routes): State<Arc<Self>>,
        axum::extract::Path(key_id): axum::extract::Path<String>,
    ) -> Result<Json<himadri_admin::UsageStats>, StatusCode> {
        let store = routes.gateway.usage_store();
        let stats = store.get_key_stats(&key_id);
        Ok(Json(stats))
    }

    pub async fn get_config(State(routes): State<Arc<Self>>) -> Json<himadri_core::Config> {
        let config = routes.gateway.get_config().await;
        Json(config)
    }

    pub async fn update_config(
        State(routes): State<Arc<Self>>,
        Json(new_config): Json<himadri_core::Config>,
    ) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
        routes
            .gateway
            .reload_config(new_config)
            .await
            .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
        Ok(Json(serde_json::json!({ "status": "updated" })))
    }

    pub async fn config_history(State(routes): State<Arc<Self>>) -> Json<serde_json::Value> {
        // For now, return current config version
        let config = routes.gateway.get_config().await;
        Json(serde_json::json!({
            "data": [],
            "summary": { "total_versions": 1 },
            "current_config": config,
        }))
    }

    pub async fn config_rollback(
        State(_routes): State<Arc<Self>>,
        axum::extract::Path(_version): axum::extract::Path<u32>,
    ) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
        // TODO: Implement rollback
        Err((
            StatusCode::NOT_IMPLEMENTED,
            "Rollback not yet implemented".to_string(),
        ))
    }

    pub async fn reload_config(
        State(routes): State<Arc<Self>>,
    ) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
        let new_config = Config::load_from_env().map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                format!("Failed to load config: {}", e),
            )
        })?;
        routes
            .gateway
            .reload_config(new_config)
            .await
            .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
        Ok(Json(serde_json::json!({ "status": "reloaded" })))
    }
}

fn error_to_response(e: GatewayError) -> Response {
    let status = StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let body = Json(serde_json::json!({
        "error": {
            "message": e.to_string(),
            "type": "gateway_error"
        }
    }));

    (status, body).into_response()
}
