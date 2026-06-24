use async_trait::async_trait;
use serde_json;
use tracing::{debug, instrument};

use crate::error::ProviderError;
use crate::http_client::CLIENT_POOL;
use crate::traits::{BoxStream, Provider};
use himadri_core::{
    ChatCompletionRequest, ChatCompletionResponse, Choice, ContentPart, Delta, MessageContent,
    ResponseMessage, StreamChoice, StreamChunk, Usage,
};

const DEFAULT_API_VERSION: &str = "2024-10-21";

pub struct AzureOpenAiProvider {
    api_key: String,
    base_url: String,
    deployment_name: String,
    api_version: String,
}

impl AzureOpenAiProvider {
    pub fn new(
        api_key: &str,
        base_url: &str,
        deployment_name: &str,
        api_version: Option<&str>,
    ) -> Self {
        Self {
            api_key: api_key.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
            deployment_name: deployment_name.to_string(),
            api_version: api_version.unwrap_or(DEFAULT_API_VERSION).to_string(),
        }
    }

    fn build_request_body(
        &self,
        request: &ChatCompletionRequest,
        stream: bool,
    ) -> serde_json::Value {
        let messages: Vec<serde_json::Value> = request
            .messages
            .iter()
            .map(|m| {
                let mut msg = serde_json::json!({
                    "role": match m.role {
                        himadri_core::Role::System => "system",
                        himadri_core::Role::User => "user",
                        himadri_core::Role::Assistant => "assistant",
                        himadri_core::Role::Tool => "tool",
                    },
                });

                if let Some(content) = &m.content {
                    match content {
                        MessageContent::Text(text) => {
                            msg["content"] = serde_json::Value::String(text.clone());
                        }
                        MessageContent::Parts(parts) => {
                            let content_parts: Vec<serde_json::Value> = parts
                                .iter()
                                .map(|p| match p {
                                    ContentPart::Text { text } => {
                                        serde_json::json!({
                                            "type": "text",
                                            "text": text
                                        })
                                    }
                                    ContentPart::ImageUrl { image_url } => {
                                        serde_json::json!({
                                            "type": "image_url",
                                            "image_url": {
                                                "url": image_url.url,
                                                "detail": image_url.detail
                                            }
                                        })
                                    }
                                })
                                .collect();
                            msg["content"] = serde_json::Value::Array(content_parts);
                        }
                    }
                }

                if let Some(name) = &m.name {
                    msg["name"] = serde_json::Value::String(name.clone());
                }

                msg
            })
            .collect();

        let mut body = serde_json::json!({
            "messages": messages,
            "stream": stream,
        });

        // Azure uses deployment_name as the model parameter
        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        if let Some(top_p) = request.top_p {
            body["top_p"] = serde_json::json!(top_p);
        }
        if let Some(max_tokens) = request.max_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }
        if let Some(stop) = &request.stop {
            body["stop"] = serde_json::json!(stop);
        }
        if let Some(presence_penalty) = request.presence_penalty {
            body["presence_penalty"] = serde_json::json!(presence_penalty);
        }
        if let Some(frequency_penalty) = request.frequency_penalty {
            body["frequency_penalty"] = serde_json::json!(frequency_penalty);
        }
        if let Some(user) = &request.user {
            body["user"] = serde_json::Value::String(user.clone());
        }

        body
    }

    fn parse_response(
        &self,
        response: serde_json::Value,
    ) -> Result<ChatCompletionResponse, ProviderError> {
        let id = response["id"].as_str().unwrap_or("").to_string();
        let model = response["model"]
            .as_str()
            .unwrap_or(&self.deployment_name)
            .to_string();
        let created = response["created"].as_u64().unwrap_or(0);

        let choices = response["choices"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|c| Choice {
                        index: c["index"].as_u64().unwrap_or(0) as u32,
                        message: ResponseMessage {
                            role: himadri_core::Role::Assistant,
                            content: c["message"]["content"].as_str().map(|s| s.to_string()),
                            tool_calls: None,
                        },
                        finish_reason: c["finish_reason"].as_str().map(|s| s.to_string()),
                    })
                    .collect()
            })
            .unwrap_or_default();

        let usage = response["usage"].as_object().map(|u| Usage {
            prompt_tokens: u["prompt_tokens"].as_u64().unwrap_or(0) as u32,
            completion_tokens: u["completion_tokens"].as_u64().unwrap_or(0) as u32,
            total_tokens: u["total_tokens"].as_u64().unwrap_or(0) as u32,
        });

        Ok(ChatCompletionResponse {
            id,
            object: "chat.completion".to_string(),
            created,
            model,
            choices,
            usage,
            system_fingerprint: response["system_fingerprint"]
                .as_str()
                .map(|s| s.to_string()),
        })
    }

    fn parse_stream_chunk(&self, chunk: serde_json::Value) -> Result<StreamChunk, ProviderError> {
        let id = chunk["id"].as_str().unwrap_or("").to_string();
        let model = chunk["model"]
            .as_str()
            .unwrap_or(&self.deployment_name)
            .to_string();
        let created = chunk["created"].as_u64().unwrap_or(0);

        let choices = chunk["choices"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|c| {
                        let delta = &c["delta"];
                        StreamChoice {
                            index: c["index"].as_u64().unwrap_or(0) as u32,
                            delta: Delta {
                                role: delta["role"].as_str().map(|r| match r {
                                    "system" => himadri_core::Role::System,
                                    "user" => himadri_core::Role::User,
                                    "assistant" => himadri_core::Role::Assistant,
                                    "tool" => himadri_core::Role::Tool,
                                    _ => himadri_core::Role::Assistant,
                                }),
                                content: delta["content"].as_str().map(|s| s.to_string()),
                                tool_calls: None,
                            },
                            finish_reason: c["finish_reason"].as_str().map(|s| s.to_string()),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        let usage = chunk["usage"].as_object().map(|u| Usage {
            prompt_tokens: u["prompt_tokens"].as_u64().unwrap_or(0) as u32,
            completion_tokens: u["completion_tokens"].as_u64().unwrap_or(0) as u32,
            total_tokens: u["total_tokens"].as_u64().unwrap_or(0) as u32,
        });

        Ok(StreamChunk {
            id,
            object: "chat.completion.chunk".to_string(),
            created,
            model,
            choices,
            usage,
            system_fingerprint: chunk["system_fingerprint"].as_str().map(|s| s.to_string()),
        })
    }

    async fn handle_error(&self, response: reqwest::Response) -> ProviderError {
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();

        let message = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| {
                v["error"]["message"]
                    .as_str()
                    .map(|s| s.to_string())
                    .or_else(|| v["message"].as_str().map(|s| s.to_string()))
            })
            .unwrap_or(body);

        match status {
            401 => ProviderError::Auth(message),
            429 => ProviderError::RateLimited {
                retry_after_secs: 60,
            },
            404 => ProviderError::ModelNotFound(message),
            _ => ProviderError::Api { status, message },
        }
    }
}

#[async_trait]
impl Provider for AzureOpenAiProvider {
    fn name(&self) -> &str {
        "azure-openai"
    }

    fn display_name(&self) -> &str {
        "Azure OpenAI"
    }

    fn supported_models(&self) -> Vec<String> {
        // Azure returns deployment name as the model
        vec![self.deployment_name.clone()]
    }

    #[instrument(skip(self, request, api_key), fields(model = %self.deployment_name))]
    async fn complete(
        &self,
        request: &ChatCompletionRequest,
        api_key: &str,
    ) -> Result<ChatCompletionResponse, ProviderError> {
        let client = CLIENT_POOL.for_provider("azure-openai");
        let body = self.build_request_body(request, false);

        debug!("Sending request to Azure OpenAI");

        let url = format!(
            "{}/openai/deployments/{}/completions?api-version={}",
            self.base_url, self.deployment_name, self.api_version
        );

        let response = client
            .post(&url)
            .header("api-key", api_key)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(self.handle_error(response).await);
        }

        let response_body: serde_json::Value = response.json().await?;
        self.parse_response(response_body)
    }

    #[instrument(skip(self, request, api_key), fields(model = %self.deployment_name))]
    async fn complete_stream(
        &self,
        request: &ChatCompletionRequest,
        api_key: &str,
    ) -> Result<BoxStream<'static, Result<StreamChunk, ProviderError>>, ProviderError> {
        let client = CLIENT_POOL.shared_streaming();
        let body = self.build_request_body(request, true);

        debug!("Sending streaming request to Azure OpenAI");

        let url = format!(
            "{}/openai/deployments/{}/completions?api-version={}",
            self.base_url, self.deployment_name, self.api_version
        );

        let response = client
            .post(&url)
            .header("api-key", api_key)
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(self.handle_error(response).await);
        }

        let byte_stream = response.bytes_stream();
        let provider = self.clone();

        let stream = async_stream::stream! {
            use futures::StreamExt;

            let mut buffer = String::new();
            let mut lines = byte_stream.map(|r| r.map(|b| String::from_utf8_lossy(&b).to_string()));

            while let Some(line_result) = lines.next().await {
                match line_result {
                    Ok(line) => {
                        buffer.push_str(&line);

                        while let Some(newline_pos) = buffer.find('\n') {
                            let line = buffer[..newline_pos].trim().to_string();
                            buffer = buffer[newline_pos + 1..].to_string();

                            if line.is_empty() {
                                continue;
                            }

                            if line == "data: [DONE]" {
                                return;
                            }

                            if let Some(data) = line.strip_prefix("data: ") {
                                match serde_json::from_str::<serde_json::Value>(data) {
                                    Ok(chunk) => {
                                        match provider.parse_stream_chunk(chunk) {
                                            Ok(parsed) => yield Ok(parsed),
                                            Err(e) => yield Err(e),
                                        }
                                    }
                                    Err(e) => {
                                        yield Err(ProviderError::Parse(e.to_string()));
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        yield Err(ProviderError::Network(e.to_string()));
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }
}

impl Clone for AzureOpenAiProvider {
    fn clone(&self) -> Self {
        Self {
            api_key: self.api_key.clone(),
            base_url: self.base_url.clone(),
            deployment_name: self.deployment_name.clone(),
            api_version: self.api_version.clone(),
        }
    }
}
