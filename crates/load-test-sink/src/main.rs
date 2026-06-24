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
    stream: bool,
    response_text: String,
}

async fn chat_completion(
    State(state): State<Arc<SinkState>>,
    Json(request): Json<serde_json::Value>,
) -> Response {
    tokio::time::sleep(Duration::from_millis(state.latency_ms)).await;
    let model = request["model"].as_str().unwrap_or("unknown");

    if state.stream {
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
        Json(serde_json::json!({
            "id": format!("mock-{}", uuid::Uuid::new_v4()), "object": "chat.completion",
            "created": chrono::Utc::now().timestamp(), "model": model,
            "choices": [{"index": 0, "message": {"role": "assistant", "content": &state.response_text}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 20, "total_tokens": 30}
        })).into_response()
    }
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
    let stream: bool = std::env::var("SINK_STREAM")
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
    let state = Arc::new(SinkState {
        latency_ms,
        stream,
        response_text,
    });
    let app = Router::new()
        .route("/v1/chat/completions", post(chat_completion))
        .layer(TraceLayer::new_for_http())
        .with_state(state);
    let listener = TcpListener::bind(format!("0.0.0.0:{}", port)).await?;
    info!(
        "Mock LLM sink on port {} (latency={}ms, stream={})",
        port, latency_ms, stream
    );
    axum::serve(listener, app).await?;
    Ok(())
}
