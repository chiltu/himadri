use async_trait::async_trait;
use futures::StreamExt;
use serde_json;
use tracing::{debug, instrument};

use crate::error::ProviderError;
use crate::http_client::CLIENT_POOL;
use crate::traits::{BoxStream, Provider};
use himadri_core::{
    ChatCompletionRequest, ChatCompletionResponse, Choice, Delta, FunctionCall, ResponseMessage,
    StreamChoice, StreamChunk, ToolCall, Usage,
};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com/v1";

/// Map an Anthropic stop reason to the OpenAI `finish_reason` vocabulary.
fn map_finish_reason(anthropic: &str) -> String {
    match anthropic {
        "end_turn" => "stop".to_string(),
        "max_tokens" => "length".to_string(),
        "tool_use" => "tool_calls".to_string(),
        other => other.to_string(),
    }
}

/// Build an OpenAI-shaped stream chunk with a single choice; `id`/`model` are
/// left empty (only `message_start` carries them) and filled in by the caller
/// when needed.
fn stream_chunk(delta: Delta, finish_reason: Option<String>, usage: Option<Usage>) -> StreamChunk {
    StreamChunk {
        object: "chat.completion.chunk".to_string(),
        created: chrono::Utc::now().timestamp() as u64,
        choices: vec![StreamChoice {
            delta,
            finish_reason,
            ..Default::default()
        }],
        usage,
        ..Default::default()
    }
}

#[derive(Clone)]
pub struct AnthropicProvider {
    base_url: String,
}

impl AnthropicProvider {
    pub fn new(base_url: Option<&str>) -> Self {
        Self {
            base_url: base_url.unwrap_or(DEFAULT_BASE_URL).to_string(),
        }
    }

    fn build_request_body(
        &self,
        request: &ChatCompletionRequest,
        stream: bool,
    ) -> serde_json::Value {
        // Extract system message
        let mut system = None;
        let mut messages = Vec::new();

        for m in &request.messages {
            match m.role {
                himadri_core::Role::System => {
                    if let Some(content) = &m.content {
                        system = content.as_text().map(|s| s.to_string());
                    }
                }
                _ => {
                    let content = m
                        .content
                        .as_ref()
                        .map(|c| c.flat_text().into_owned())
                        .unwrap_or_default();

                    messages.push(serde_json::json!({
                        "role": match m.role {
                            himadri_core::Role::User => "user",
                            himadri_core::Role::Assistant => "assistant",
                            _ => "user",
                        },
                        "content": content,
                    }));
                }
            }
        }

        let mut body = serde_json::json!({
            "model": request.model,
            "messages": messages,
            "stream": stream,
            "max_tokens": request.max_tokens.unwrap_or(4096),
        });

        if let Some(system) = system {
            body["system"] = serde_json::Value::String(system);
        }

        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        if let Some(top_p) = request.top_p {
            body["top_p"] = serde_json::json!(top_p);
        }
        if let Some(stop) = &request.stop {
            body["stop_sequences"] = serde_json::json!(stop);
        }

        // Translate OpenAI-shaped tools into Anthropic's schema.
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
                if let Some(translated) = Self::translate_tool_choice(choice) {
                    body["tool_choice"] = translated;
                }
            }
        }

        body
    }

    /// Map an OpenAI `tool_choice` to Anthropic's `tool_choice` object.
    /// `"auto"` -> {type:auto}, `"required"` -> {type:any}, `"none"` -> {type:none},
    /// `{type:function, function:{name}}` -> {type:tool, name}.
    fn translate_tool_choice(choice: &serde_json::Value) -> Option<serde_json::Value> {
        match choice {
            serde_json::Value::String(s) => match s.as_str() {
                "auto" => Some(serde_json::json!({"type": "auto"})),
                "required" => Some(serde_json::json!({"type": "any"})),
                "none" => Some(serde_json::json!({"type": "none"})),
                _ => None,
            },
            serde_json::Value::Object(obj) => {
                let name = obj
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str());
                name.map(|n| serde_json::json!({"type": "tool", "name": n}))
            }
            _ => None,
        }
    }

    fn parse_response(
        &self,
        response: serde_json::Value,
    ) -> Result<ChatCompletionResponse, ProviderError> {
        let id = response["id"].as_str().unwrap_or("").to_string();
        let model = response["model"].as_str().unwrap_or("").to_string();

        // Anthropic returns an array of content blocks: `text` blocks and
        // `tool_use` blocks. Concatenate the text and translate any tool_use
        // blocks into OpenAI-style `tool_calls` (previously dropped).
        let blocks = response["content"].as_array();
        let content = blocks
            .map(|arr| {
                arr.iter()
                    .filter(|c| c["type"] == "text")
                    .filter_map(|c| c["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .filter(|s| !s.is_empty());

        let tool_calls = blocks
            .map(|arr| {
                arr.iter()
                    .filter(|c| c["type"] == "tool_use")
                    .map(|c| ToolCall {
                        id: c["id"].as_str().unwrap_or("").to_string(),
                        tool_type: "function".to_string(),
                        function: FunctionCall {
                            name: c["name"].as_str().unwrap_or("").to_string(),
                            // OpenAI carries arguments as a JSON string.
                            arguments: c["input"].to_string(),
                        },
                    })
                    .collect::<Vec<_>>()
            })
            .filter(|v| !v.is_empty());

        let finish_reason = response["stop_reason"].as_str().map(map_finish_reason);

        let choices = vec![Choice {
            index: 0,
            message: ResponseMessage {
                role: himadri_core::Role::Assistant,
                content,
                tool_calls,
            },
            finish_reason,
        }];

        let usage = response["usage"].as_object().map(|u| Usage {
            prompt_tokens: u["input_tokens"].as_u64().unwrap_or(0) as u32,
            completion_tokens: u["output_tokens"].as_u64().unwrap_or(0) as u32,
            total_tokens: u["input_tokens"].as_u64().unwrap_or(0) as u32
                + u["output_tokens"].as_u64().unwrap_or(0) as u32,
        });

        Ok(ChatCompletionResponse {
            id,
            object: "chat.completion".to_string(),
            created: chrono::Utc::now().timestamp() as u64,
            model,
            choices,
            usage,
            system_fingerprint: None,
        })
    }

    fn parse_stream_event(
        &self,
        event_type: &str,
        data: &serde_json::Value,
        input_tokens: &mut u32,
    ) -> Option<Result<StreamChunk, ProviderError>> {
        match event_type {
            "message_start" => {
                let message = &data["message"];
                // Anthropic reports prompt tokens only here; stash them for
                // the final message_delta usage.
                *input_tokens = message["usage"]["input_tokens"].as_u64().unwrap_or(0) as u32;

                let mut chunk = stream_chunk(
                    Delta {
                        role: Some(himadri_core::Role::Assistant),
                        ..Default::default()
                    },
                    None,
                    None,
                );
                chunk.id = message["id"].as_str().unwrap_or("").to_string();
                chunk.model = message["model"].as_str().unwrap_or("").to_string();
                Some(Ok(chunk))
            }
            "content_block_delta" => {
                let text = data["delta"]["text"].as_str().unwrap_or("").to_string();
                let content = if text.is_empty() { None } else { Some(text) };
                Some(Ok(stream_chunk(
                    Delta {
                        content,
                        ..Default::default()
                    },
                    None,
                    None,
                )))
            }
            "message_delta" => {
                let stop_reason = data["delta"]["stop_reason"].as_str().map(map_finish_reason);

                let usage = data["usage"].as_object().map(|u| {
                    let output = u["output_tokens"].as_u64().unwrap_or(0) as u32;
                    Usage {
                        prompt_tokens: *input_tokens,
                        completion_tokens: output,
                        total_tokens: *input_tokens + output,
                    }
                });

                Some(Ok(stream_chunk(Delta::default(), stop_reason, usage)))
            }
            "message_stop" => None,
            "ping" => None,
            _ => None,
        }
    }

    async fn handle_error(&self, response: reqwest::Response) -> ProviderError {
        ProviderError::from_openai_response(response).await
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }

    fn display_name(&self) -> &str {
        "Anthropic"
    }

    fn supported_models(&self) -> Vec<String> {
        vec![
            "claude-3-5-sonnet-20241022".to_string(),
            "claude-3-5-haiku-20241022".to_string(),
            "claude-3-opus-20240229".to_string(),
            "claude-3-sonnet-20240229".to_string(),
            "claude-3-haiku-20240307".to_string(),
        ]
    }

    #[instrument(skip(self, request, api_key), fields(model = %request.model))]
    async fn complete(
        &self,
        request: &ChatCompletionRequest,
        api_key: &str,
    ) -> Result<ChatCompletionResponse, ProviderError> {
        let client = CLIENT_POOL.for_provider("anthropic");
        let body = self.build_request_body(request, false);

        debug!("Sending request to Anthropic");

        let response = client
            .post(format!("{}/messages", self.base_url))
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
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

    #[instrument(skip(self, request, api_key), fields(model = %request.model))]
    async fn complete_stream(
        &self,
        request: &ChatCompletionRequest,
        api_key: &str,
    ) -> Result<BoxStream<'static, Result<StreamChunk, ProviderError>>, ProviderError> {
        let client = CLIENT_POOL.shared_streaming();
        let body = self.build_request_body(request, true);

        debug!("Sending streaming request to Anthropic");

        let response = client
            .post(format!("{}/messages", self.base_url))
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(self.handle_error(response).await);
        }

        let provider = self.clone();
        let mut input_tokens: u32 = 0;
        let stream = crate::sse::sse_events(response.bytes_stream()).filter_map(move |event| {
            let result = match event {
                Ok(event) => {
                    let event_type = event.event.unwrap_or_default();
                    // Malformed data lines are skipped, matching the
                    // pre-existing lenient behavior for Anthropic events.
                    serde_json::from_str::<serde_json::Value>(&event.data)
                        .ok()
                        .and_then(|data| {
                            provider.parse_stream_event(&event_type, &data, &mut input_tokens)
                        })
                }
                Err(e) => Some(Err(e)),
            };
            async move { result }
        });

        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod stream_usage_tests {
    use super::*;

    /// Regression: streamed Anthropic requests used to hardcode
    /// `prompt_tokens: 0` — the `message_start` input count must flow into
    /// the final `message_delta` usage.
    #[test]
    fn message_start_input_tokens_flow_into_final_usage() {
        let provider = AnthropicProvider::new(None);
        let mut input_tokens = 0u32;

        let start = serde_json::json!({
            "message": {
                "id": "msg_1",
                "model": "claude-3-5-sonnet-20241022",
                "usage": { "input_tokens": 9, "output_tokens": 1 }
            }
        });
        provider
            .parse_stream_event("message_start", &start, &mut input_tokens)
            .unwrap()
            .unwrap();
        assert_eq!(input_tokens, 9);

        let delta = serde_json::json!({
            "delta": { "stop_reason": "end_turn" },
            "usage": { "output_tokens": 5 }
        });
        let chunk = provider
            .parse_stream_event("message_delta", &delta, &mut input_tokens)
            .unwrap()
            .unwrap();
        let usage = chunk.usage.expect("final chunk carries usage");
        assert_eq!(usage.prompt_tokens, 9);
        assert_eq!(usage.completion_tokens, 5);
        assert_eq!(usage.total_tokens, 14);
    }
}

#[cfg(test)]
mod tool_tests {
    use super::*;
    use himadri_core::{ChatCompletionRequest, Message, Tool, ToolFunction};

    fn request_with_tools(tool_choice: serde_json::Value) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "claude-3-5-sonnet".to_string(),
            messages: vec![Message::user("weather?")],
            tools: Some(vec![Tool {
                tool_type: "function".to_string(),
                function: ToolFunction {
                    name: "get_weather".to_string(),
                    description: Some("Get weather".to_string()),
                    parameters: Some(serde_json::json!({"type": "object"})),
                },
            }]),
            tool_choice: Some(tool_choice),
            ..Default::default()
        }
    }

    #[test]
    fn tools_translated_to_anthropic_schema() {
        let provider = AnthropicProvider::new(None);
        let body =
            provider.build_request_body(&request_with_tools(serde_json::json!("auto")), false);
        // Anthropic uses name/description/input_schema, not {type, function}.
        assert_eq!(body["tools"][0]["name"], "get_weather");
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
        assert!(body["tools"][0].get("function").is_none());
        assert_eq!(body["tool_choice"]["type"], "auto");
    }

    #[test]
    fn parse_response_surfaces_tool_use_blocks() {
        // Regression (R36): tool_use blocks were dropped, so a tool-calling
        // response came back empty.
        let provider = AnthropicProvider::new(None);
        let response = serde_json::json!({
            "id": "msg_1",
            "model": "claude-3-5-sonnet",
            "stop_reason": "tool_use",
            "content": [
                { "type": "text", "text": "Let me check." },
                { "type": "tool_use", "id": "toolu_1", "name": "get_weather",
                  "input": { "city": "Paris" } }
            ],
            "usage": { "input_tokens": 5, "output_tokens": 3 }
        });
        let parsed = provider.parse_response(response).unwrap();
        let choice = &parsed.choices[0];
        assert_eq!(choice.finish_reason.as_deref(), Some("tool_calls"));
        let calls = choice
            .message
            .tool_calls
            .as_ref()
            .expect("tool_calls present");
        assert_eq!(calls[0].function.name, "get_weather");
        assert_eq!(calls[0].id, "toolu_1");
        assert!(calls[0].function.arguments.contains("Paris"));
        assert_eq!(choice.message.content.as_deref(), Some("Let me check."));
    }

    #[test]
    fn tool_choice_required_maps_to_any() {
        assert_eq!(
            AnthropicProvider::translate_tool_choice(&serde_json::json!("required")).unwrap(),
            serde_json::json!({"type": "any"})
        );
    }

    #[test]
    fn tool_choice_specific_function_maps_to_tool() {
        let choice = serde_json::json!({"type": "function", "function": {"name": "get_weather"}});
        assert_eq!(
            AnthropicProvider::translate_tool_choice(&choice).unwrap(),
            serde_json::json!({"type": "tool", "name": "get_weather"})
        );
    }
}
