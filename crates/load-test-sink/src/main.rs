use axum::{
    extract::{Json, State},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::post,
    Router,
};
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;
use tracing::info;

#[derive(Clone)]
struct SinkState {
    latency_ms: u64,
    /// Default streaming behaviour when a request does not specify `stream`.
    stream_default: bool,
    response_text: String,
    /// When set, every endpoint returns this HTTP status with an error body.
    /// Used to simulate an unhealthy upstream (e.g. 503) for failover tests.
    error_status: Option<u16>,
}

/// Build an error response for the configured `error_status`, if any.
fn error_response(state: &SinkState) -> Option<Response> {
    state.error_status.map(|code| {
        let status = axum::http::StatusCode::from_u16(code)
            .unwrap_or(axum::http::StatusCode::SERVICE_UNAVAILABLE);
        (
            status,
            Json(serde_json::json!({
                "error": { "message": format!("sink configured to return {}", code) }
            })),
        )
            .into_response()
    })
}

async fn chat_completion(
    State(state): State<Arc<SinkState>>,
    Json(request): Json<serde_json::Value>,
) -> Response {
    tokio::time::sleep(Duration::from_millis(state.latency_ms)).await;
    if let Some(resp) = error_response(&state) {
        return resp;
    }
    let model = request["model"].as_str().unwrap_or("unknown");

    // Respect the per-request `stream` flag, falling back to the configured default.
    let stream = request["stream"].as_bool().unwrap_or(state.stream_default);

    // If the caller supplied tools, echo a tool call so tool-calling can be
    // exercised end-to-end.
    let has_tools = request
        .get("tools")
        .and_then(|t| t.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false);

    if stream {
        let text = state.response_text.clone();
        let model = model.to_string();
        let stream = async_stream::stream! {
            let id = format!("mock-{}", uuid::Uuid::new_v4());
            let created = chrono::Utc::now().timestamp() as u64;
            yield Ok::<_, Infallible>(Event::default().data(serde_json::json!({
                "id": id, "object": "chat.completion.chunk", "created": created, "model": model,
                "choices": [{"index": 0, "delta": {"role": "assistant", "content": null}, "finish_reason": null}]
            }).to_string()));
            for word in text.split_whitespace() {
                yield Ok(Event::default().data(serde_json::json!({
                    "id": id, "object": "chat.completion.chunk", "created": created, "model": model,
                    "choices": [{"index": 0, "delta": {"content": format!("{} ", word)}, "finish_reason": null}]
                }).to_string()));
            }
            yield Ok(Event::default().data(serde_json::json!({
                "id": id, "object": "chat.completion.chunk", "created": created, "model": model,
                "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
            }).to_string()));
        };
        Sse::new(stream)
            .keep_alive(
                KeepAlive::new()
                    .interval(Duration::from_secs(15))
                    .text("ping"),
            )
            .into_response()
    } else {
        let message = if has_tools {
            serde_json::json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_mock_1",
                    "type": "function",
                    "function": {
                        "name": "get_weather",
                        "arguments": "{\"city\":\"Paris\"}"
                    }
                }]
            })
        } else {
            serde_json::json!({ "role": "assistant", "content": &state.response_text })
        };
        let finish = if has_tools { "tool_calls" } else { "stop" };
        Json(serde_json::json!({
            "id": format!("mock-{}", uuid::Uuid::new_v4()), "object": "chat.completion",
            "created": chrono::Utc::now().timestamp(), "model": model,
            "choices": [{"index": 0, "message": message, "finish_reason": finish}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 20, "total_tokens": 30}
        }))
        .into_response()
    }
}

async fn embeddings(
    State(state): State<Arc<SinkState>>,
    Json(request): Json<serde_json::Value>,
) -> Response {
    tokio::time::sleep(Duration::from_millis(state.latency_ms)).await;
    if let Some(resp) = error_response(&state) {
        return resp;
    }
    let model = request["model"].as_str().unwrap_or("unknown");

    // `input` may be a single string or an array of strings.
    let count = match &request["input"] {
        serde_json::Value::Array(arr) => arr.len().max(1),
        _ => 1,
    };

    let data: Vec<serde_json::Value> = (0..count)
        .map(|i| {
            serde_json::json!({
                "object": "embedding",
                "index": i,
                // Deterministic, small fixed-dimension vector.
                "embedding": [0.01_f32, 0.02, 0.03, 0.04]
            })
        })
        .collect();

    Json(serde_json::json!({
        "object": "list",
        "data": data,
        "model": model,
        "usage": { "prompt_tokens": 5, "total_tokens": 5 }
    }))
    .into_response()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let latency_ms: u64 = std::env::var("SINK_LATENCY_MS")
        .unwrap_or_else(|_| "100".to_string())
        .parse()
        .unwrap_or(100);
    let stream_default: bool = std::env::var("SINK_STREAM")
        .unwrap_or_else(|_| "true".to_string())
        .parse()
        .unwrap_or(true);
    let response_text = std::env::var("SINK_RESPONSE").unwrap_or_else(|_| {
        "Hello from the mock LLM server. Simulated response for load testing.".to_string()
    });
    let port: u16 = std::env::var("SINK_PORT")
        .unwrap_or_else(|_| "8081".to_string())
        .parse()
        .unwrap_or(8081);
    let error_status: Option<u16> = std::env::var("SINK_STATUS")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .filter(|c| *c != 200);
    let state = Arc::new(SinkState {
        latency_ms,
        stream_default,
        response_text,
        error_status,
    });
    let app = Router::new()
        .route("/v1/chat/completions", post(chat_completion))
        .route("/v1/embeddings", post(embeddings))
        .layer(TraceLayer::new_for_http())
        .with_state(state);
    let listener = TcpListener::bind(format!("0.0.0.0:{}", port)).await?;
    info!(
        "Mock LLM sink on port {} (latency={}ms, stream_default={})",
        port, latency_ms, stream_default
    );
    axum::serve(listener, app).await?;
    Ok(())
}
