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

const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

pub struct GeminiProvider {
    base_url: String,
}

impl GeminiProvider {
    pub fn new(base_url: Option<&str>) -> Self {
        Self {
            base_url: base_url.unwrap_or(DEFAULT_BASE_URL).to_string(),
        }
    }

    fn build_request_body(&self, request: &ChatCompletionRequest) -> serde_json::Value {
        let contents: Vec<serde_json::Value> = request
            .messages
            .iter()
            .filter_map(|m| {
                let role = match m.role {
                    himadri_core::Role::System => return None, // Handle system separately
                    himadri_core::Role::User => "user",
                    himadri_core::Role::Assistant => "model",
                    himadri_core::Role::Tool => "user", // Map tool to user for Gemini
                };

                let text = match &m.content {
                    Some(MessageContent::Text(text)) => text.clone(),
                    Some(MessageContent::Parts(parts)) => parts
                        .iter()
                        .filter_map(|p| match p {
                            ContentPart::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join(""),
                    None => return None,
                };

                Some(serde_json::json!({
                    "role": role,
                    "parts": [{"text": text}]
                }))
            })
            .collect();

        // Extract system instruction
        let system_instruction = request
            .messages
            .iter()
            .find(|m| m.role == himadri_core::Role::System)
            .and_then(|m| m.content.as_ref())
            .and_then(|c| c.as_text())
            .map(|text| {
                serde_json::json!({
                    "parts": [{"text": text}]
                })
            });

        let mut generation_config = serde_json::json!({});

        if let Some(temp) = request.temperature {
            generation_config["temperature"] = serde_json::json!(temp);
        }
        if let Some(top_p) = request.top_p {
            generation_config["topP"] = serde_json::json!(top_p);
        }
        if let Some(max_tokens) = request.max_tokens {
            generation_config["maxOutputTokens"] = serde_json::json!(max_tokens);
        }
        if let Some(stop) = &request.stop {
            generation_config["stopSequences"] = serde_json::json!(stop);
        }

        let mut body = serde_json::json!({
            "contents": contents,
            "generationConfig": generation_config,
        });

        if let Some(system) = system_instruction {
            body["systemInstruction"] = system;
        }

        // Translate OpenAI-shaped tools into Gemini's functionDeclarations.
        if let Some(tools) = &request.tools {
            let declarations: Vec<serde_json::Value> = tools
                .iter()
                .map(|t| {
                    let mut decl = serde_json::json!({ "name": t.function.name });
                    if let Some(desc) = &t.function.description {
                        decl["description"] = serde_json::json!(desc);
                    }
                    if let Some(params) = &t.function.parameters {
                        decl["parameters"] = params.clone();
                    }
                    decl
                })
                .collect();
            body["tools"] = serde_json::json!([{ "functionDeclarations": declarations }]);

            if let Some(mode) = request
                .tool_choice
                .as_ref()
                .and_then(Self::translate_tool_choice_mode)
            {
                body["toolConfig"] = serde_json::json!({
                    "functionCallingConfig": { "mode": mode }
                });
            }
        }

        body
    }

    /// Map an OpenAI `tool_choice` to Gemini's functionCallingConfig mode.
    /// `"auto"` -> AUTO, `"required"` / a specific function -> ANY, `"none"` -> NONE.
    fn translate_tool_choice_mode(choice: &serde_json::Value) -> Option<String> {
        match choice {
            serde_json::Value::String(s) => match s.as_str() {
                "auto" => Some("AUTO".to_string()),
                "required" => Some("ANY".to_string()),
                "none" => Some("NONE".to_string()),
                _ => None,
            },
            // An explicit tool object forces a call.
            serde_json::Value::Object(_) => Some("ANY".to_string()),
            _ => None,
        }
    }

    fn parse_response(
        &self,
        response: serde_json::Value,
        model: &str,
    ) -> Result<ChatCompletionResponse, ProviderError> {
        let candidate = response["candidates"]
            .as_array()
            .and_then(|arr| arr.first())
            .ok_or_else(|| ProviderError::Internal("No candidates in response".to_string()))?;

        let content = candidate["content"]["parts"]
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|p| p["text"].as_str())
            .map(|s| s.to_string());

        let finish_reason = candidate["finishReason"].as_str().map(|r| match r {
            "STOP" => "stop".to_string(),
            "MAX_TOKENS" => "length".to_string(),
            _ => r.to_string(),
        });

        let choices = vec![Choice {
            index: 0,
            message: ResponseMessage {
                role: himadri_core::Role::Assistant,
                content,
                tool_calls: None,
            },
            finish_reason,
        }];

        let usage = response["usageMetadata"].as_object().map(|u| Usage {
            prompt_tokens: u["promptTokenCount"].as_u64().unwrap_or(0) as u32,
            completion_tokens: u["candidatesTokenCount"].as_u64().unwrap_or(0) as u32,
            total_tokens: u["totalTokenCount"].as_u64().unwrap_or(0) as u32,
        });

        Ok(ChatCompletionResponse {
            id: format!("gemini-{}", uuid::Uuid::new_v4()),
            object: "chat.completion".to_string(),
            created: chrono::Utc::now().timestamp() as u64,
            model: model.to_string(),
            choices,
            usage,
            system_fingerprint: None,
        })
    }

    fn parse_stream_chunk(
        &self,
        chunk: serde_json::Value,
        model: &str,
    ) -> Result<StreamChunk, ProviderError> {
        let candidate = chunk["candidates"].as_array().and_then(|arr| arr.first());

        let (content, finish_reason) = if let Some(candidate) = candidate {
            let text = candidate["content"]["parts"]
                .as_array()
                .and_then(|arr| arr.first())
                .and_then(|p| p["text"].as_str())
                .map(|s| s.to_string());

            let finish = candidate["finishReason"].as_str().map(|r| match r {
                "STOP" => "stop".to_string(),
                "MAX_TOKENS" => "length".to_string(),
                _ => r.to_string(),
            });

            (text, finish)
        } else {
            (None, None)
        };

        let usage = chunk["usageMetadata"].as_object().map(|u| Usage {
            prompt_tokens: u["promptTokenCount"].as_u64().unwrap_or(0) as u32,
            completion_tokens: u["candidatesTokenCount"].as_u64().unwrap_or(0) as u32,
            total_tokens: u["totalTokenCount"].as_u64().unwrap_or(0) as u32,
        });

        Ok(StreamChunk {
            id: format!("gemini-{}", uuid::Uuid::new_v4()),
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
                finish_reason,
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
                v["error"]["message"]
                    .as_str()
                    .map(|s| s.to_string())
                    .or_else(|| v["message"].as_str().map(|s| s.to_string()))
            })
            .unwrap_or(body);

        match status {
            401 | 403 => ProviderError::Auth(message),
            429 => {
                let retry_after = 60;
                ProviderError::RateLimited {
                    retry_after_secs: retry_after,
                }
            }
            404 => ProviderError::ModelNotFound(message),
            _ => ProviderError::Api { status, message },
        }
    }
}

#[async_trait]
impl Provider for GeminiProvider {
    fn name(&self) -> &str {
        "gemini"
    }

    fn display_name(&self) -> &str {
        "Google Gemini"
    }

    fn supported_models(&self) -> Vec<String> {
        vec![
            "gemini-2.0-flash".to_string(),
            "gemini-1.5-pro".to_string(),
            "gemini-1.5-flash".to_string(),
            "gemini-pro".to_string(),
        ]
    }

    #[instrument(skip(self, request, api_key), fields(model = %request.model))]
    async fn complete(
        &self,
        request: &ChatCompletionRequest,
        api_key: &str,
    ) -> Result<ChatCompletionResponse, ProviderError> {
        let client = CLIENT_POOL.for_provider("gemini");
        let body = self.build_request_body(request);

        debug!("Sending request to Gemini");

        let url = format!(
            "{}/models/{}:generateContent?key={}",
            self.base_url, request.model, api_key
        );

        let response = client
            .post(&url)
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
        let mut body = self.build_request_body(request);
        body["generationConfig"]["responseModalities"] = serde_json::json!(["TEXT"]);

        debug!("Sending streaming request to Gemini");

        let url = format!(
            "{}/models/{}:streamGenerateContent?alt=sse&key={}",
            self.base_url, request.model, api_key
        );

        let response = client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
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

                            if let Some(data) = line.strip_prefix("data: ") {
                                match serde_json::from_str::<serde_json::Value>(data) {
                                    Ok(chunk) => {
                                        // Gemini sends array of candidates
                                        if let Some(candidates) = chunk["candidates"].as_array() {
                                            for candidate in candidates {
                                                let single_chunk = serde_json::json!({
                                                    "candidates": [candidate],
                                                    "usageMetadata": chunk["usageMetadata"]
                                                });
                                                match (GeminiProvider { base_url: String::new() }).parse_stream_chunk(single_chunk, &model) {
                                                    Ok(parsed) => yield Ok(parsed),
                                                    Err(e) => yield Err(e),
                                                }
                                            }
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

impl Clone for GeminiProvider {
    fn clone(&self) -> Self {
        Self {
            base_url: self.base_url.clone(),
        }
    }
}

#[cfg(test)]
mod tool_tests {
    use super::*;
    use himadri_core::{ChatCompletionRequest, Message, Role, Tool, ToolFunction};

    fn request_with_tools(tool_choice: serde_json::Value) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "gemini-1.5-pro".to_string(),
            messages: vec![Message {
                role: Role::User,
                content: Some(MessageContent::Text("weather?".to_string())),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            }],
            stream: false,
            temperature: None,
            top_p: None,
            max_tokens: None,
            stop: None,
            presence_penalty: None,
            frequency_penalty: None,
            user: None,
            tools: Some(vec![Tool {
                tool_type: "function".to_string(),
                function: ToolFunction {
                    name: "get_weather".to_string(),
                    description: Some("Get weather".to_string()),
                    parameters: Some(serde_json::json!({"type": "object"})),
                },
            }]),
            tool_choice: Some(tool_choice),
            extra: Default::default(),
        }
    }

    #[test]
    fn tools_translated_to_function_declarations() {
        let provider = GeminiProvider::new(None);
        let body = provider.build_request_body(&request_with_tools(serde_json::json!("required")));
        assert_eq!(
            body["tools"][0]["functionDeclarations"][0]["name"],
            "get_weather"
        );
        assert_eq!(body["toolConfig"]["functionCallingConfig"]["mode"], "ANY");
    }

    #[test]
    fn tool_choice_modes_map_correctly() {
        assert_eq!(
            GeminiProvider::translate_tool_choice_mode(&serde_json::json!("auto")).unwrap(),
            "AUTO"
        );
        assert_eq!(
            GeminiProvider::translate_tool_choice_mode(&serde_json::json!("none")).unwrap(),
            "NONE"
        );
    }
}
