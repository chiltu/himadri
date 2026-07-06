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

const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Map a Gemini finish reason to the OpenAI `finish_reason` vocabulary.
fn map_finish_reason(gemini: &str) -> String {
    match gemini {
        "STOP" => "stop".to_string(),
        "MAX_TOKENS" => "length".to_string(),
        other => other.to_string(),
    }
}

#[derive(Clone)]
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
                    Some(content) => content.flat_text().into_owned(),
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

        // A candidate's parts hold `text` and/or `functionCall` entries.
        // Concatenate the text and translate functionCall parts into
        // OpenAI-style `tool_calls` (previously dropped).
        let parts = candidate["content"]["parts"].as_array();
        let content = parts
            .map(|arr| {
                arr.iter()
                    .filter_map(|p| p["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .filter(|s| !s.is_empty());

        let tool_calls = parts
            .map(|arr| {
                arr.iter()
                    .filter_map(|p| p.get("functionCall"))
                    .map(|fc| ToolCall {
                        id: format!("call-{}", uuid::Uuid::new_v4()),
                        tool_type: "function".to_string(),
                        function: FunctionCall {
                            name: fc["name"].as_str().unwrap_or("").to_string(),
                            // OpenAI carries arguments as a JSON string.
                            arguments: fc["args"].to_string(),
                        },
                    })
                    .collect::<Vec<_>>()
            })
            .filter(|v| !v.is_empty());

        let finish_reason = candidate["finishReason"].as_str().map(map_finish_reason);

        let choices = vec![Choice {
            index: 0,
            message: ResponseMessage {
                role: himadri_core::Role::Assistant,
                content,
                tool_calls,
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

            let finish = candidate["finishReason"].as_str().map(map_finish_reason);

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
        ProviderError::from_response(response, &[401, 403], |v| {
            v["error"]["message"]
                .as_str()
                .or_else(|| v["message"].as_str())
                .map(str::to_string)
        })
        .await
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

        // Pass the key via the `x-goog-api-key` header, never the URL query
        // string: reqwest includes the full URL in its error `Display`, which
        // flows into `ProviderError::Network` and thus logs / audit / 502
        // bodies — leaking the provider credential.
        let url = format!("{}/models/{}:generateContent", self.base_url, request.model);

        let response = client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("x-goog-api-key", api_key)
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

        // Key travels in the `x-goog-api-key` header, not the URL (see
        // `complete` above — a URL-embedded key leaks through error text).
        let url = format!(
            "{}/models/{}:streamGenerateContent?alt=sse",
            self.base_url, request.model
        );

        let response = client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("x-goog-api-key", api_key)
            .header("Accept", "text/event-stream")
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(self.handle_error(response).await);
        }

        let model = request.model.clone();
        let provider = self.clone();

        // Each SSE event carries an array of candidates, so one event can
        // fan out into multiple stream chunks.
        let stream = crate::sse::sse_events(response.bytes_stream()).flat_map(move |event| {
            let results: Vec<Result<StreamChunk, ProviderError>> = match event {
                Ok(event) => match serde_json::from_str::<serde_json::Value>(&event.data) {
                    Ok(chunk) => chunk["candidates"]
                        .as_array()
                        .map(|candidates| {
                            candidates
                                .iter()
                                .map(|candidate| {
                                    let single_chunk = serde_json::json!({
                                        "candidates": [candidate],
                                        "usageMetadata": chunk["usageMetadata"]
                                    });
                                    provider.parse_stream_chunk(single_chunk, &model)
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                    Err(e) => vec![Err(ProviderError::Parse(e.to_string()))],
                },
                Err(e) => vec![Err(e)],
            };
            futures::stream::iter(results)
        });

        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tool_tests {
    use super::*;
    use himadri_core::{ChatCompletionRequest, Message, Tool, ToolFunction};

    fn request_with_tools(tool_choice: serde_json::Value) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "gemini-1.5-pro".to_string(),
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
    fn parse_response_surfaces_function_calls() {
        // Regression (R36): functionCall parts were dropped.
        let provider = GeminiProvider::new(None);
        let response = serde_json::json!({
            "candidates": [{
                "content": { "parts": [
                    { "text": "Checking." },
                    { "functionCall": { "name": "get_weather", "args": { "city": "Paris" } } }
                ]},
                "finishReason": "STOP"
            }]
        });
        let parsed = provider.parse_response(response, "gemini-1.5-pro").unwrap();
        let choice = &parsed.choices[0];
        let calls = choice
            .message
            .tool_calls
            .as_ref()
            .expect("tool_calls present");
        assert_eq!(calls[0].function.name, "get_weather");
        assert!(calls[0].function.arguments.contains("Paris"));
        assert_eq!(choice.message.content.as_deref(), Some("Checking."));
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
