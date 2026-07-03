use futures::{Stream, StreamExt};

use crate::error::ProviderError;

/// A single server-sent event: the optional `event:` field seen before the
/// `data:` line, plus the raw `data:` payload.
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

/// Decode a raw response byte stream into SSE events.
///
/// Buffers partial lines across chunk boundaries, tracks `event:` types
/// (reset on blank lines, per the SSE spec), and terminates on the
/// OpenAI-style `data: [DONE]` sentinel. Transport errors surface as
/// `ProviderError::Network`.
pub fn sse_events<S, B, E>(byte_stream: S) -> impl Stream<Item = Result<SseEvent, ProviderError>>
where
    S: Stream<Item = Result<B, E>>,
    B: AsRef<[u8]>,
    E: std::fmt::Display,
{
    async_stream::stream! {
        let mut byte_stream = std::pin::pin!(byte_stream);
        let mut buffer = String::new();
        let mut current_event: Option<String> = None;

        while let Some(chunk) = byte_stream.next().await {
            match chunk {
                Ok(bytes) => {
                    buffer.push_str(&String::from_utf8_lossy(bytes.as_ref()));

                    while let Some(newline_pos) = buffer.find('\n') {
                        let line = buffer[..newline_pos].trim().to_string();
                        buffer = buffer[newline_pos + 1..].to_string();

                        if line.is_empty() {
                            current_event = None;
                            continue;
                        }

                        if let Some(event) = line.strip_prefix("event: ") {
                            current_event = Some(event.to_string());
                            continue;
                        }

                        if let Some(data) = line.strip_prefix("data: ") {
                            if data == "[DONE]" {
                                return;
                            }
                            yield Ok(SseEvent {
                                event: current_event.clone(),
                                data: data.to_string(),
                            });
                        }
                    }
                }
                Err(e) => {
                    yield Err(ProviderError::Network(e.to_string()));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;

    async fn collect(chunks: Vec<&str>) -> Vec<Result<SseEvent, ProviderError>> {
        let byte_stream = stream::iter(
            chunks
                .into_iter()
                .map(|c| Ok::<_, std::io::Error>(c.as_bytes())),
        );
        sse_events(byte_stream).collect().await
    }

    #[tokio::test]
    async fn parses_data_lines_and_stops_at_done() {
        let events = collect(vec!["data: {\"a\":1}\n\ndata: [DONE]\n\ndata: {\"b\":2}\n"]).await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].as_ref().unwrap().data, "{\"a\":1}");
    }

    #[tokio::test]
    async fn buffers_lines_split_across_chunks() {
        let events = collect(vec!["data: {\"a\"", ":1}\nda", "ta: {\"b\":2}\n"]).await;
        let data: Vec<_> = events
            .iter()
            .map(|e| e.as_ref().unwrap().data.clone())
            .collect();
        assert_eq!(data, vec!["{\"a\":1}", "{\"b\":2}"]);
    }

    #[tokio::test]
    async fn tracks_event_types_and_resets_on_blank_line() {
        let events = collect(vec!["event: delta\ndata: {}\n\ndata: {}\n"]).await;
        assert_eq!(events[0].as_ref().unwrap().event.as_deref(), Some("delta"));
        assert_eq!(events[1].as_ref().unwrap().event, None);
    }

    #[tokio::test]
    async fn surfaces_transport_errors() {
        let byte_stream = stream::iter(vec![
            Ok("data: {}\n".as_bytes()),
            Err(std::io::Error::other("boom")),
        ]);
        let events: Vec<_> = sse_events(byte_stream).collect().await;
        assert!(events[0].is_ok());
        assert!(matches!(events[1], Err(ProviderError::Network(_))));
    }
}
