use futures::{Stream, StreamExt};

use crate::error::ProviderError;

/// Upper bound on a single buffered SSE line. A well-behaved provider sends
/// lines far smaller than this; without a cap, an upstream that never sends
/// a newline could grow the buffer without bound.
const MAX_SSE_LINE_BYTES: usize = 1024 * 1024; // 1 MiB

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
///
/// Lines are consumed with a cursor and a single `drain` per network chunk
/// (not a tail reallocation per line), and the buffer is capped at
/// [`MAX_SSE_LINE_BYTES`] so a newline-less upstream cannot exhaust memory.
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

                    let mut consumed = 0;
                    while let Some(rel_pos) = buffer[consumed..].find('\n') {
                        let line = buffer[consumed..consumed + rel_pos].trim();
                        consumed += rel_pos + 1;

                        if line.is_empty() {
                            current_event = None;
                            continue;
                        }

                        // Per the SSE spec the colon may be followed by at
                        // most one optional space; accept both forms.
                        if let Some(event) = line.strip_prefix("event:") {
                            current_event = Some(event.trim_start().to_string());
                            continue;
                        }

                        if let Some(data) = line.strip_prefix("data:") {
                            let data = data.trim_start();
                            if data == "[DONE]" {
                                return;
                            }
                            yield Ok(SseEvent {
                                event: current_event.clone(),
                                data: data.to_string(),
                            });
                        }
                    }
                    buffer.drain(..consumed);

                    if buffer.len() > MAX_SSE_LINE_BYTES {
                        yield Err(ProviderError::Parse(
                            "SSE line exceeds maximum buffered size".to_string(),
                        ));
                        return;
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
    async fn accepts_data_prefix_without_space() {
        // The SSE spec allows `data:` with no space after the colon.
        let events = collect(vec!["data:{\"a\":1}\nevent:delta\ndata:{\"b\":2}\n"]).await;
        assert_eq!(events[0].as_ref().unwrap().data, "{\"a\":1}");
        assert_eq!(events[1].as_ref().unwrap().event.as_deref(), Some("delta"));
        assert_eq!(events[1].as_ref().unwrap().data, "{\"b\":2}");
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

    #[tokio::test]
    async fn caps_unbounded_lines() {
        // An upstream that never sends a newline must be cut off with a
        // parse error, not buffered without bound.
        let big = "x".repeat(MAX_SSE_LINE_BYTES + 1);
        let byte_stream = stream::iter(vec![Ok::<_, std::io::Error>(big.as_bytes())]);
        let events: Vec<_> = sse_events(byte_stream).collect().await;
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], Err(ProviderError::Parse(_))));
    }

    #[tokio::test]
    async fn many_lines_in_one_chunk() {
        // Exercises the cursor + single-drain path.
        let mut input = String::new();
        for i in 0..500 {
            input.push_str(&format!("data: {{\"i\":{}}}\n", i));
        }
        let events = collect(vec![&input]).await;
        assert_eq!(events.len(), 500);
        assert_eq!(events[499].as_ref().unwrap().data, "{\"i\":499}");
    }
}
