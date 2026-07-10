//! The streaming request path: opens a stream via the shared failover
//! machinery, then wraps it with output guardrails and a usage recorder
//! that fires at stream end or client disconnect (via `Drop`).

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures::{Stream, StreamExt};
use tracing::{instrument, warn};

use himadri_core::{AuthContext, ChatCompletionRequest, GatewayError, StreamChunk};
use himadri_observability::{AuditEvent, AuditStatus, Metrics};
use himadri_plugin::traits::ResponseAction;
use himadri_plugin::PluginManager;
use himadri_provider::traits::BoxStream;

use super::audit::{audit_messages, record_request_outcome, RequestOutcome};
use super::route::AttemptError;
use super::Gateway;

impl Gateway {
    #[instrument(skip(self, request, auth), fields(model = %request.model))]
    pub async fn route_stream(
        &self,
        request: ChatCompletionRequest,
        auth: Option<&AuthContext>,
        remote_ip: Option<String>,
    ) -> Result<
        BoxStream<'static, Result<StreamChunk, himadri_provider::ProviderError>>,
        GatewayError,
    > {
        // Guards + before-request plugins (shared with route).
        let ctx = self.prepare_request(&request, auth, remote_ip).await?;

        let ordered = self.select_targets(&request, auth).await?;

        // Try targets in priority order until one opens a stream. Failover for
        // streaming only applies before the first chunk is produced — once a
        // stream is established we cannot transparently switch providers, and
        // the circuit breaker records success when the stream opens.
        let request_ref = &request;
        let (target, result, _latency) = self
            .with_failover(&ordered, false, |provider, api_key| async move {
                provider.complete_stream(request_ref, &api_key).await
            })
            .await
            .ok_or_else(|| GatewayError::Internal("No targets configured".to_string()))?;

        let stream = result.map_err(|e| match e {
            AttemptError::Provider(e) => GatewayError::from(e),
            AttemptError::CircuitOpen(p) => GatewayError::CircuitOpen(p),
            AttemptError::ProviderNotFound(p) => GatewayError::ProviderNotFound(p),
            AttemptError::ApiKey(e) => e,
            AttemptError::NoTargets => {
                GatewayError::Internal("No targets produced a stream".to_string())
            }
        })?;

        // Log audit event for stream start
        {
            let messages = audit_messages(&request);

            let event = AuditEvent {
                request_id: ctx.request_id.clone(),
                timestamp: chrono::Utc::now(),
                org_id: auth.and_then(|a| a.org_id.clone()),
                team_id: auth.and_then(|a| a.team_id.clone()),
                user_id: auth.and_then(|a| a.user_id.clone()),
                key_id: auth.and_then(|a| a.key_id.clone()),
                model: request.model.clone(),
                provider: Some(target.provider.clone()),
                messages,
                response: None,
                latency_ms: 0,
                tokens_prompt: None,
                tokens_completion: None,
                tokens_total: None,
                status: AuditStatus::Success,
                error: None,
                guardrail_actions: Vec::new(),
                stream: true,
            };
            self.audit_log.log(event);
        }

        // Wrap stream with output guardrails and usage accounting. The
        // recorder fires at stream end (or client disconnect, via Drop),
        // covering the usage/metrics recording that `route` does inline.
        let recorder = StreamUsageRecorder {
            metrics: self.metrics.clone(),
            usage_store: self.usage_store.clone(),
            request_log: self.request_log.clone(),
            provider: target.provider.clone(),
            model: request.model.clone(),
            api_key_id: auth.and_then(|a| a.key_id.clone()),
            started: std::time::Instant::now(),
            usage: None,
            error: None,
            recorded: false,
        };
        let plugin_manager = self.plugin_manager.clone();
        let auth_clone = auth.cloned();
        let request_clone = request.clone();
        let wrapped_stream = wrap_stream_with_guardrails(
            stream,
            plugin_manager,
            request_clone,
            auth_clone,
            recorder,
        );

        Ok(Box::pin(wrapped_stream))
    }
}

/// Records usage, metrics and a request-log entry for a streamed request,
/// mirroring what `route` records for non-streaming ones. Usage is taken
/// from the last stream chunk that carries it (OpenAI-style final-chunk
/// usage, Anthropic `message_delta`). Recording happens once, at stream end
/// or — via `Drop` — when the client disconnects mid-stream.
struct StreamUsageRecorder {
    metrics: Arc<Metrics>,
    usage_store: Arc<himadri_admin::UsageStore>,
    request_log: Arc<dyn himadri_admin::RequestLogStore>,
    provider: String,
    model: String,
    api_key_id: Option<String>,
    started: std::time::Instant,
    usage: Option<himadri_core::Usage>,
    error: Option<String>,
    recorded: bool,
}

impl StreamUsageRecorder {
    fn observe_chunk(&mut self, chunk: &StreamChunk) {
        if let Some(usage) = &chunk.usage {
            self.usage = Some(usage.clone());
        }
    }

    fn observe_error(&mut self, e: &himadri_provider::ProviderError) {
        self.error = Some(e.to_string());
    }

    fn finish(&mut self) {
        if self.recorded {
            return;
        }
        self.recorded = true;

        record_request_outcome(&RequestOutcome {
            metrics: &self.metrics,
            usage_store: &self.usage_store,
            request_log: self.request_log.as_ref(),
            provider: &self.provider,
            model: &self.model,
            api_key_id: self.api_key_id.as_deref(),
            usage: self.usage.clone(),
            error: self.error.clone(),
            latency_ms: self.started.elapsed().as_millis() as u64,
        });
    }
}

impl Drop for StreamUsageRecorder {
    fn drop(&mut self) {
        self.finish();
    }
}

struct GuardrailStream<S> {
    inner: S,
    buffer: String,
    buffer_limit: usize,
    plugin_manager: Arc<PluginManager>,
    request: ChatCompletionRequest,
    auth: Option<AuthContext>,
    guardrails_ran: bool,
    recorder: StreamUsageRecorder,
}

const DEFAULT_STREAM_BUFFER_LIMIT: usize = 1024 * 1024; // 1MB

impl<S> Stream for GuardrailStream<S>
where
    S: Stream<Item = Result<StreamChunk, himadri_provider::ProviderError>> + Unpin,
{
    type Item = Result<StreamChunk, himadri_provider::ProviderError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.inner.poll_next_unpin(cx) {
            Poll::Ready(Some(Ok(chunk))) => {
                self.recorder.observe_chunk(&chunk);
                // Accumulate response text from chunks, but cap at buffer limit
                if self.buffer.len() < self.buffer_limit {
                    for choice in &chunk.choices {
                        if let Some(ref delta) = choice.delta.content {
                            self.buffer.push_str(delta);
                        }
                    }
                }
                Poll::Ready(Some(Ok(chunk)))
            }
            Poll::Ready(Some(Err(e))) => {
                self.recorder.observe_error(&e);
                Poll::Ready(Some(Err(e)))
            }
            Poll::Ready(None) => {
                self.recorder.finish();
                // Stream ended — run guardrails on accumulated content
                if !self.buffer.is_empty() && !self.guardrails_ran {
                    self.guardrails_ran = true;
                    let pm = self.plugin_manager.clone();
                    let ctx = himadri_plugin::PluginContext::from_request(
                        &self.request,
                        self.auth.as_ref(),
                    );
                    let buffer = self.buffer.clone();

                    // Post-hoc only: the stream already delivered its chunks,
                    // so a rejection here can merely be logged. Truncating
                    // mid-stream guardrails would need windowed scanning of
                    // chunks before forwarding them — a deliberate non-goal
                    // for now.
                    tokio::spawn(async move {
                        match pm.run_response_guardrails(&ctx, &buffer).await {
                            Ok(ResponseAction::Reject(reason)) => {
                                warn!("Stream response guardrail rejected: {}", reason);
                            }
                            Ok(ResponseAction::Redact(_)) => {
                                warn!("Stream response guardrail redacted content (partial stream already sent)");
                            }
                            _ => {}
                        }
                    });
                }
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

fn wrap_stream_with_guardrails<S>(
    stream: S,
    plugin_manager: Arc<PluginManager>,
    request: ChatCompletionRequest,
    auth: Option<AuthContext>,
    recorder: StreamUsageRecorder,
) -> GuardrailStream<S>
where
    S: Stream<Item = Result<StreamChunk, himadri_provider::ProviderError>> + Unpin + Send + 'static,
{
    GuardrailStream {
        inner: stream,
        buffer: String::new(),
        buffer_limit: DEFAULT_STREAM_BUFFER_LIMIT,
        plugin_manager,
        request,
        auth,
        guardrails_ran: false,
        recorder,
    }
}

#[cfg(test)]
mod stream_usage_tests {
    use super::*;
    use futures::stream;
    use himadri_observability::Metrics;

    fn chunk(content: &str, usage: Option<himadri_core::Usage>) -> StreamChunk {
        StreamChunk {
            id: "c1".to_string(),
            object: "chat.completion.chunk".to_string(),
            created: 0,
            model: "gpt-4o".to_string(),
            choices: vec![himadri_core::StreamChoice {
                index: 0,
                delta: himadri_core::Delta {
                    role: None,
                    content: Some(content.to_string()),
                    tool_calls: None,
                },
                finish_reason: None,
            }],
            usage,
            system_fingerprint: None,
        }
    }

    fn recorder(
        usage_store: &Arc<himadri_admin::UsageStore>,
        request_log: &Arc<himadri_admin::InMemoryRequestLogStore>,
    ) -> StreamUsageRecorder {
        StreamUsageRecorder {
            metrics: Arc::new(Metrics::new()),
            usage_store: usage_store.clone(),
            request_log: request_log.clone() as Arc<dyn himadri_admin::RequestLogStore>,
            provider: "openai".to_string(),
            model: "gpt-4o".to_string(),
            api_key_id: Some("key-1".to_string()),
            started: std::time::Instant::now(),
            usage: None,
            error: None,
            recorded: false,
        }
    }

    fn request() -> ChatCompletionRequest {
        serde_json::from_value(serde_json::json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true,
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn records_usage_from_final_chunk_at_stream_end() {
        let usage_store = Arc::new(himadri_admin::UsageStore::new());
        let request_log = Arc::new(himadri_admin::InMemoryRequestLogStore::new());

        let chunks: Vec<Result<StreamChunk, himadri_provider::ProviderError>> = vec![
            Ok(chunk("hel", None)),
            Ok(chunk(
                "lo",
                Some(himadri_core::Usage {
                    prompt_tokens: 7,
                    completion_tokens: 5,
                    total_tokens: 12,
                }),
            )),
        ];
        let wrapped = wrap_stream_with_guardrails(
            stream::iter(chunks),
            Arc::new(PluginManager::new()),
            request(),
            None,
            recorder(&usage_store, &request_log),
        );
        let out: Vec<_> = wrapped.collect().await;
        assert_eq!(out.len(), 2);

        let dashboard = usage_store.get_dashboard(0);
        assert_eq!(dashboard.total_requests, 1);
        assert_eq!(dashboard.total_tokens, 12);
        let stats = usage_store.get_key_stats("key-1");
        assert_eq!(stats.total_tokens, 12);
    }

    #[tokio::test]
    async fn records_error_when_client_disconnects_mid_stream() {
        let usage_store = Arc::new(himadri_admin::UsageStore::new());
        let request_log = Arc::new(himadri_admin::InMemoryRequestLogStore::new());

        let chunks: Vec<Result<StreamChunk, himadri_provider::ProviderError>> = vec![
            Ok(chunk("partial", None)),
            Err(himadri_provider::ProviderError::Network(
                "reset".to_string(),
            )),
        ];
        let mut wrapped = Box::pin(wrap_stream_with_guardrails(
            stream::iter(chunks),
            Arc::new(PluginManager::new()),
            request(),
            None,
            recorder(&usage_store, &request_log),
        ));
        // Consume both items, then drop without polling to completion —
        // Drop must still record, marking the request as failed.
        let _ = wrapped.next().await;
        let _ = wrapped.next().await;
        drop(wrapped);

        let dashboard = usage_store.get_dashboard(0);
        assert_eq!(dashboard.total_requests, 1);
        assert!(dashboard.error_rate > 0.0);
    }
}
