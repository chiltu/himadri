use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};

use himadri_core::{
    ChatCompletionRequest, ChatCompletionResponse, Choice, Delta, ResponseMessage, StreamChoice,
    StreamChunk, Usage,
};
use himadri_provider::error::ProviderError;
use himadri_provider::traits::{BoxStream, Provider};

/// Mock provider for testing. Simulates LLM responses without network calls.
pub struct MockProvider {
    name: String,
    response_text: String,
    call_count: AtomicUsize,
    latency_ms: u64,
}

impl MockProvider {
    pub fn new(name: &str, response_text: &str) -> Self {
        Self {
            name: name.to_string(),
            response_text: response_text.to_string(),
            call_count: AtomicUsize::new(0),
            latency_ms: 0,
        }
    }

    #[allow(dead_code)]
    pub fn with_latency(name: &str, response_text: &str, latency_ms: u64) -> Self {
        Self {
            name: name.to_string(),
            response_text: response_text.to_string(),
            call_count: AtomicUsize::new(0),
            latency_ms,
        }
    }

    pub fn call_count(&self) -> usize {
        self.call_count.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl Provider for MockProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn display_name(&self) -> &str {
        &self.name
    }

    fn supported_models(&self) -> Vec<String> {
        vec![
            format!("mock-{}", self.name),
            format!("mock-{}-large", self.name),
        ]
    }

    async fn complete(
        &self,
        request: &ChatCompletionRequest,
        _api_key: &str,
    ) -> Result<ChatCompletionResponse, ProviderError> {
        self.call_count.fetch_add(1, Ordering::Relaxed);

        if self.latency_ms > 0 {
            tokio::time::sleep(tokio::time::Duration::from_millis(self.latency_ms)).await;
        }

        // Simulate error for specific model names
        if request.model.contains("error") {
            return Err(ProviderError::Api {
                status: 500,
                message: "Mock provider error".to_string(),
            });
        }

        if request.model.contains("rate-limit") {
            return Err(ProviderError::RateLimited {
                retry_after_secs: 60,
            });
        }

        if request.model.contains("auth") {
            return Err(ProviderError::Auth("Invalid API key".to_string()));
        }

        let content = format!("{} (model: {})", self.response_text, request.model);

        Ok(ChatCompletionResponse {
            id: format!("mock-{}", uuid::Uuid::new_v4()),
            object: "chat.completion".to_string(),
            created: chrono::Utc::now().timestamp() as u64,
            model: request.model.clone(),
            choices: vec![Choice {
                index: 0,
                message: ResponseMessage {
                    role: himadri_core::Role::Assistant,
                    content: Some(content),
                    tool_calls: None,
                },
                finish_reason: Some("stop".to_string()),
            }],
            usage: Some(Usage {
                prompt_tokens: 10,
                completion_tokens: 20,
                total_tokens: 30,
            }),
            system_fingerprint: None,
        })
    }

    async fn complete_stream(
        &self,
        request: &ChatCompletionRequest,
        _api_key: &str,
    ) -> Result<BoxStream<'static, Result<StreamChunk, ProviderError>>, ProviderError> {
        self.call_count.fetch_add(1, Ordering::Relaxed);

        if self.latency_ms > 0 {
            tokio::time::sleep(tokio::time::Duration::from_millis(self.latency_ms)).await;
        }

        if request.model.contains("error") {
            return Err(ProviderError::Api {
                status: 500,
                message: "Mock provider error".to_string(),
            });
        }

        let model = request.model.clone();
        let words: Vec<String> = self
            .response_text
            .split_whitespace()
            .map(|w| w.to_string())
            .collect();

        let stream = async_stream::stream! {
            let id = format!("mock-stream-{}", uuid::Uuid::new_v4());
            let created = chrono::Utc::now().timestamp() as u64;

            // First chunk: role
            yield Ok(StreamChunk {
                id: id.clone(),
                object: "chat.completion.chunk".to_string(),
                created,
                model: model.clone(),
                choices: vec![StreamChoice {
                    index: 0,
                    delta: Delta {
                        role: Some(himadri_core::Role::Assistant),
                        content: None,
                        tool_calls: None,
                    },
                    finish_reason: None,
                }],
                usage: None,
                system_fingerprint: None,
            });

            // Content chunks: one per word
            for word in &words {
                yield Ok(StreamChunk {
                    id: id.clone(),
                    object: "chat.completion.chunk".to_string(),
                    created,
                    model: model.clone(),
                    choices: vec![StreamChoice {
                        index: 0,
                        delta: Delta {
                            role: None,
                            content: Some(format!("{} ", word)),
                            tool_calls: None,
                        },
                        finish_reason: None,
                    }],
                    usage: None,
                    system_fingerprint: None,
                });
            }

            // Final chunk: finish
            yield Ok(StreamChunk {
                id: id.clone(),
                object: "chat.completion.chunk".to_string(),
                created,
                model: model.clone(),
                choices: vec![StreamChoice {
                    index: 0,
                    delta: Delta {
                        role: None,
                        content: None,
                        tool_calls: None,
                    },
                    finish_reason: Some("stop".to_string()),
                }],
                usage: Some(Usage {
                    prompt_tokens: 10,
                    completion_tokens: words.len() as u32,
                    total_tokens: 10 + words.len() as u32,
                }),
                system_fingerprint: None,
            });
        };

        Ok(Box::pin(stream))
    }
}
