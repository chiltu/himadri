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

const DEFAULT_REGION: &str = "us-east-1";
const BEDROCK_RUNTIME_ENDPOINT: &str = "bedrock-runtime";

pub struct BedrockProvider {
    region: String,
    access_key_id: String,
    secret_access_key: String,
    session_token: Option<String>,
}

impl BedrockProvider {
    pub fn new(
        region: Option<&str>,
        access_key_id: &str,
        secret_access_key: &str,
        session_token: Option<&str>,
    ) -> Self {
        Self {
            region: region.unwrap_or(DEFAULT_REGION).to_string(),
            access_key_id: access_key_id.to_string(),
            secret_access_key: secret_access_key.to_string(),
            session_token: session_token.map(|s| s.to_string()),
        }
    }

    fn get_endpoint(&self) -> String {
        format!(
            "https://{}.{}.amazonaws.com",
            BEDROCK_RUNTIME_ENDPOINT, self.region
        )
    }

    fn build_request_body(&self, request: &ChatCompletionRequest) -> serde_json::Value {
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
        });

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
            body["stop_sequences"] = serde_json::json!(stop);
        }

        // Bedrock Claude models use the Anthropic tool schema.
        if let Some(tools) = &request.tools {
            let anthropic_tools: Vec<serde_json::Value> = tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "name": t.function.name,
                        "description": t.function.description,
                        "input_schema": t.function.parameters
                            .clone()
                            .unwrap_or_else(|| serde_json::json!({"type": "object"})),
                    })
                })
                .collect();
            body["tools"] = serde_json::json!(anthropic_tools);

            if let Some(choice) = &request.tool_choice {
                let translated = match choice {
                    serde_json::Value::String(s) => match s.as_str() {
                        "auto" => Some(serde_json::json!({"type": "auto"})),
                        "required" => Some(serde_json::json!({"type": "any"})),
                        "none" => Some(serde_json::json!({"type": "none"})),
                        _ => None,
                    },
                    serde_json::Value::Object(obj) => obj
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .map(|n| serde_json::json!({"type": "tool", "name": n})),
                    _ => None,
                };
                if let Some(tc) = translated {
                    body["tool_choice"] = tc;
                }
            }
        }

        body
    }

    fn build_stream_request_body(&self, request: &ChatCompletionRequest) -> serde_json::Value {
        let mut body = self.build_request_body(request);
        body["stream"] = serde_json::json!(true);
        body
    }

    fn parse_response(
        &self,
        response: serde_json::Value,
        model: &str,
    ) -> Result<ChatCompletionResponse, ProviderError> {
        let id = response["requestId"].as_str().unwrap_or("").to_string();

        let content = response["output"]["message"]["content"]
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|c| c["text"].as_str())
            .map(|s| s.to_string());

        let stop_reason = response["stopReason"].as_str().map(|r| match r {
            "end_turn" => "stop".to_string(),
            "max_tokens" => "length".to_string(),
            "stop_sequence" => "stop".to_string(),
            _ => r.to_string(),
        });

        let choices = vec![Choice {
            index: 0,
            message: ResponseMessage {
                role: himadri_core::Role::Assistant,
                content,
                tool_calls: None,
            },
            finish_reason: stop_reason,
        }];

        let usage = response["usage"].as_object().map(|u| Usage {
            prompt_tokens: u["inputTokens"].as_u64().unwrap_or(0) as u32,
            completion_tokens: u["outputTokens"].as_u64().unwrap_or(0) as u32,
            total_tokens: u["inputTokens"].as_u64().unwrap_or(0) as u32
                + u["outputTokens"].as_u64().unwrap_or(0) as u32,
        });

        Ok(ChatCompletionResponse {
            id,
            object: "chat.completion".to_string(),
            created: chrono::Utc::now().timestamp() as u64,
            model: model.to_string(),
            choices,
            usage,
            system_fingerprint: None,
        })
    }

    #[allow(dead_code)]
    fn parse_stream_chunk(
        &self,
        chunk: serde_json::Value,
        model: &str,
    ) -> Result<StreamChunk, ProviderError> {
        let content = chunk["contentBlockDelta"]["delta"]["text"]
            .as_str()
            .map(|s| s.to_string());

        let stop_reason = chunk["metadata"]["stopReason"].as_str().map(|r| match r {
            "end_turn" => "stop".to_string(),
            "max_tokens" => "length".to_string(),
            "stop_sequence" => "stop".to_string(),
            _ => r.to_string(),
        });

        let usage = chunk["usage"].as_object().map(|u| Usage {
            prompt_tokens: u["inputTokens"].as_u64().unwrap_or(0) as u32,
            completion_tokens: u["outputTokens"].as_u64().unwrap_or(0) as u32,
            total_tokens: u["inputTokens"].as_u64().unwrap_or(0) as u32
                + u["outputTokens"].as_u64().unwrap_or(0) as u32,
        });

        Ok(StreamChunk {
            id: format!("bedrock-{}", uuid::Uuid::new_v4()),
            object: "chat.completion.chunk".to_string(),
            created: chrono::Utc::now().timestamp() as u64,
            model: model.to_string(),
            choices: vec![StreamChoice {
                index: 0,
                delta: Delta {
                    role: None,
                    content,
                    tool_calls: None,
                },
                finish_reason: stop_reason,
            }],
            usage,
            system_fingerprint: None,
        })
    }

    async fn handle_error(&self, response: reqwest::Response) -> ProviderError {
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();

        let message = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| {
                v["message"]
                    .as_str()
                    .map(|s| s.to_string())
                    .or_else(|| v["__type"].as_str().map(|s| s.to_string()))
            })
            .unwrap_or(body);

        match status {
            403 => ProviderError::Auth(message),
            429 => ProviderError::RateLimited {
                retry_after_secs: 60,
            },
            404 => ProviderError::ModelNotFound(message),
            _ => ProviderError::Api { status, message },
        }
    }
}

#[async_trait]
impl Provider for BedrockProvider {
    fn name(&self) -> &str {
        "bedrock"
    }

    fn display_name(&self) -> &str {
        "AWS Bedrock"
    }

    fn supported_models(&self) -> Vec<String> {
        vec![
            "anthropic.claude-3-5-sonnet-20241022-v2:0".to_string(),
            "anthropic.claude-3-opus-20240229-v1:0".to_string(),
            "anthropic.claude-3-haiku-20240307-v1:0".to_string(),
            "amazon.titan-text-express-v1".to_string(),
            "amazon.titan-text-lite-v1".to_string(),
            "meta.llama3-70b-instruct-v1:0".to_string(),
            "meta.llama3-8b-instruct-v1:0".to_string(),
            "mistral.mistral-7b-instruct-v0:2".to_string(),
            "mistral.mixtral-8x7b-instruct-v0:1".to_string(),
        ]
    }

    #[instrument(skip(self, request, api_key), fields(model = %request.model))]
    async fn complete(
        &self,
        request: &ChatCompletionRequest,
        api_key: &str,
    ) -> Result<ChatCompletionResponse, ProviderError> {
        let client = CLIENT_POOL.for_provider("bedrock");
        let body = self.build_request_body(request);

        debug!("Sending request to AWS Bedrock");

        let url = format!("{}/model/{}/invoke", self.get_endpoint(), request.model);

        // Note: In production, you would sign this request with AWS Signature V4
        // For now, we use a simplified approach with Bearer token
        let response = client
            .post(&url)
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(self.handle_error(response).await);
        }

        let response_body: serde_json::Value = response.json().await?;
        self.parse_response(response_body, &request.model)
    }

    #[instrument(skip(self, request, api_key), fields(model = %request.model))]
    async fn complete_stream(
        &self,
        request: &ChatCompletionRequest,
        api_key: &str,
    ) -> Result<BoxStream<'static, Result<StreamChunk, ProviderError>>, ProviderError> {
        let client = CLIENT_POOL.shared_streaming();
        let body = self.build_stream_request_body(request);

        debug!("Sending streaming request to AWS Bedrock");

        let url = format!(
            "{}/model/{}/invoke-with-response-stream",
            self.get_endpoint(),
            request.model
        );

        let response = client
            .post(&url)
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .header("Accept", "application/vnd.amazon.eventstream")
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(self.handle_error(response).await);
        }

        let byte_stream = response.bytes_stream();
        let model = request.model.clone();

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

                            // Bedrock uses event: headers and data: bodies
                            if let Some(data) = line.strip_prefix("data: ") {
                                match serde_json::from_str::<serde_json::Value>(data) {
                                    Ok(chunk) => {
                                        let id = chunk["contentBlockDelta"]["outputIndex"]
                                            .as_u64()
                                            .map(|i| format!("bedrock-{}", i))
                                            .unwrap_or_else(|| "bedrock-0".to_string());

                                        let content = chunk["contentBlockDelta"]["delta"]["text"]
                                            .as_str()
                                            .map(|s| s.to_string());

                                        let stop_reason = chunk["metadata"]["stopReason"]
                                            .as_str()
                                            .map(|r| match r {
                                                "end_turn" => "stop".to_string(),
                                                "max_tokens" => "length".to_string(),
                                                _ => r.to_string(),
                                            });

                                        yield Ok(StreamChunk {
                                            id,
                                            object: "chat.completion.chunk".to_string(),
                                            created: chrono::Utc::now().timestamp() as u64,
                                            model: model.clone(),
                                            choices: vec![StreamChoice {
                                                index: 0,
                                                delta: Delta {
                                                    role: None,
                                                    content,
                                                    tool_calls: None,
                                                },
                                                finish_reason: stop_reason,
                                            }],
                                            usage: None,
                                            system_fingerprint: None,
                                        });
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

impl Clone for BedrockProvider {
    fn clone(&self) -> Self {
        Self {
            region: self.region.clone(),
            access_key_id: self.access_key_id.clone(),
            secret_access_key: self.secret_access_key.clone(),
            session_token: self.session_token.clone(),
        }
    }
}
