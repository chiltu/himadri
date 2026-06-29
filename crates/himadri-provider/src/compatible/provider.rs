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

/// Authentication method for OpenAI-compatible providers.
#[derive(Debug, Clone)]
pub enum AuthMethod {
    /// Bearer token in Authorization header (default)
    Bearer,
    /// API key in custom header (e.g., Azure uses "api-key" header)
    Header { header_name: String },
}

/// Configuration for an OpenAI-compatible provider.
#[derive(Debug, Clone)]
pub struct OpenAiCompatibleConfig {
    /// Provider identifier (e.g., "openai", "azure", "openrouter")
    pub name: String,
    /// Provider display name
    pub display_name: String,
    /// Base URL (e.g., "https://api.openai.com/v1")
    pub base_url: String,
    /// Authentication method
    pub auth_method: AuthMethod,
    /// URL path template for chat completions
    /// - OpenAI: "/chat/completions"
    /// - Azure: "/openai/deployments/{deployment}/completions?api-version={version}"
    pub chat_completions_path: String,
    /// Extra headers to send with every request
    pub extra_headers: Vec<(String, String)>,
    /// Supported models list
    pub models: Vec<String>,
}

/// A generic OpenAI-compatible provider that works with any API
/// following the OpenAI chat completions format.
///
/// This eliminates the need to implement separate providers for:
/// - OpenAI
/// - Azure OpenAI
/// - OpenRouter
/// - Together AI
/// - Groq
/// - Fireworks
/// - And many others
pub struct OpenAiCompatibleProvider {
    config: OpenAiCompatibleConfig,
}

impl OpenAiCompatibleProvider {
    pub fn new(config: OpenAiCompatibleConfig) -> Self {
        Self { config }
    }

    /// Create a simple provider with Bearer auth.
    pub fn bearer(name: &str, base_url: &str) -> Self {
        Self::new(OpenAiCompatibleConfig {
            name: name.to_string(),
            display_name: name.to_string(),
            base_url: base_url.to_string(),
            auth_method: AuthMethod::Bearer,
            chat_completions_path: "/chat/completions".to_string(),
            extra_headers: vec![],
            models: vec![],
        })
    }

    /// Create an Azure OpenAI provider.
    pub fn azure(_api_key: &str, base_url: &str, deployment: &str, api_version: &str) -> Self {
        let base_url = base_url.trim_end_matches('/').to_string();
        let path = format!(
            "/openai/deployments/{}/completions?api-version={}",
            deployment, api_version
        );

        Self::new(OpenAiCompatibleConfig {
            name: "azure-openai".to_string(),
            display_name: "Azure OpenAI".to_string(),
            base_url,
            auth_method: AuthMethod::Header {
                header_name: "api-key".to_string(),
            },
            chat_completions_path: path,
            extra_headers: vec![],
            models: vec![deployment.to_string()],
        })
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
            "model": request.model,
            "messages": messages,
            "stream": stream,
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
        if let Some(tools) = &request.tools {
            body["tools"] = serde_json::json!(tools);
        }
        if let Some(tool_choice) = &request.tool_choice {
            body["tool_choice"] = tool_choice.clone();
        }

        body
    }

    fn parse_response(
        &self,
        response: serde_json::Value,
    ) -> Result<ChatCompletionResponse, ProviderError> {
        let id = response["id"].as_str().unwrap_or("").to_string();
        let model = response["model"].as_str().unwrap_or("").to_string();
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
                            tool_calls: serde_json::from_value(
                                c["message"]["tool_calls"].clone(),
                            )
                            .ok()
                            .filter(|tc: &Vec<himadri_core::ToolCall>| !tc.is_empty()),
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
        let model = chunk["model"].as_str().unwrap_or("").to_string();
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
                                tool_calls: serde_json::from_value(delta["tool_calls"].clone())
                                    .ok()
                                    .filter(|tc: &Vec<himadri_core::ToolCallDelta>| !tc.is_empty()),
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

    fn build_auth_header(&self, api_key: &str) -> (String, String) {
        match &self.config.auth_method {
            AuthMethod::Bearer => ("Authorization".to_string(), format!("Bearer {}", api_key)),
            AuthMethod::Header { header_name } => (header_name.clone(), api_key.to_string()),
        }
    }

    fn get_url(&self) -> String {
        format!(
            "{}{}",
            self.config.base_url, self.config.chat_completions_path
        )
    }

    fn get_embeddings_url(&self) -> String {
        format!("{}/embeddings", self.config.base_url.trim_end_matches('/'))
    }
}

#[async_trait]
impl Provider for OpenAiCompatibleProvider {
    fn name(&self) -> &str {
        &self.config.name
    }

    fn display_name(&self) -> &str {
        &self.config.display_name
    }

    fn supported_models(&self) -> Vec<String> {
        self.config.models.clone()
    }

    #[instrument(skip(self, request, api_key), fields(model = %request.model))]
    async fn complete(
        &self,
        request: &ChatCompletionRequest,
        api_key: &str,
    ) -> Result<ChatCompletionResponse, ProviderError> {
        let client = CLIENT_POOL.for_provider(&self.config.name);
        let body = self.build_request_body(request, false);

        debug!("Sending request to {}", self.config.display_name);

        let (auth_header_name, auth_header_value) = self.build_auth_header(api_key);

        let mut req_builder = client
            .post(self.get_url())
            .header(&auth_header_name, &auth_header_value)
            .header("Content-Type", "application/json");

        for (name, value) in &self.config.extra_headers {
            req_builder = req_builder.header(name.as_str(), value.as_str());
        }

        let response = req_builder.json(&body).send().await?;

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

        debug!("Sending streaming request to {}", self.config.display_name);

        let (auth_header_name, auth_header_value) = self.build_auth_header(api_key);

        let mut req_builder = client
            .post(self.get_url())
            .header(&auth_header_name, &auth_header_value)
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream");

        for (name, value) in &self.config.extra_headers {
            req_builder = req_builder.header(name.as_str(), value.as_str());
        }

        let response = req_builder.json(&body).send().await?;

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

    #[instrument(skip(self, request, api_key), fields(model = %request.model))]
    async fn embed(
        &self,
        request: &himadri_core::EmbeddingRequest,
        api_key: &str,
    ) -> Result<himadri_core::EmbeddingResponse, ProviderError> {
        let client = CLIENT_POOL.for_provider(&self.config.name);
        let (auth_header_name, auth_header_value) = self.build_auth_header(api_key);

        let mut req_builder = client
            .post(self.get_embeddings_url())
            .header(&auth_header_name, &auth_header_value)
            .header("Content-Type", "application/json");

        for (name, value) in &self.config.extra_headers {
            req_builder = req_builder.header(name.as_str(), value.as_str());
        }

        let response = req_builder.json(request).send().await?;

        if !response.status().is_success() {
            return Err(self.handle_error(response).await);
        }

        response
            .json::<himadri_core::EmbeddingResponse>()
            .await
            .map_err(|e| ProviderError::Parse(e.to_string()))
    }
}

impl Clone for OpenAiCompatibleProvider {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
        }
    }
}

// ─── Pre-configured Providers ────────────────────────────────────────

impl OpenAiCompatibleConfig {
    pub fn openai() -> Self {
        Self {
            name: "openai".to_string(),
            display_name: "OpenAI".to_string(),
            base_url: "https://api.openai.com/v1".to_string(),
            auth_method: AuthMethod::Bearer,
            chat_completions_path: "/chat/completions".to_string(),
            extra_headers: vec![],
            models: vec![
                "gpt-4".to_string(),
                "gpt-4-turbo".to_string(),
                "gpt-4o".to_string(),
                "gpt-4o-mini".to_string(),
                "gpt-3.5-turbo".to_string(),
                "o1".to_string(),
                "o1-mini".to_string(),
                "o1-pro".to_string(),
            ],
        }
    }

    pub fn openrouter() -> Self {
        Self {
            name: "openrouter".to_string(),
            display_name: "OpenRouter".to_string(),
            base_url: "https://openrouter.ai/api/v1".to_string(),
            auth_method: AuthMethod::Bearer,
            chat_completions_path: "/chat/completions".to_string(),
            extra_headers: vec![
                (
                    "HTTP-Referer".to_string(),
                    "https://github.com/himadri".to_string(),
                ),
                ("X-Title".to_string(), "himadri".to_string()),
            ],
            models: vec![
                "openrouter/auto".to_string(),
                "openai/gpt-4o".to_string(),
                "anthropic/claude-3.5-sonnet".to_string(),
                "google/gemini-2.0-flash".to_string(),
            ],
        }
    }

    pub fn together_ai() -> Self {
        Self {
            name: "together".to_string(),
            display_name: "Together AI".to_string(),
            base_url: "https://api.together.xyz/v1".to_string(),
            auth_method: AuthMethod::Bearer,
            chat_completions_path: "/chat/completions".to_string(),
            extra_headers: vec![],
            models: vec![
                "meta-llama/Llama-3-70b-chat-hf".to_string(),
                "meta-llama/Llama-3-8b-chat-hf".to_string(),
                "mistralai/Mixtral-8x7B-Instruct-v0.1".to_string(),
            ],
        }
    }

    pub fn groq() -> Self {
        Self {
            name: "groq".to_string(),
            display_name: "Groq".to_string(),
            base_url: "https://api.groq.com/openai/v1".to_string(),
            auth_method: AuthMethod::Bearer,
            chat_completions_path: "/chat/completions".to_string(),
            extra_headers: vec![],
            models: vec![
                "llama3-70b-8192".to_string(),
                "llama3-8b-8192".to_string(),
                "mixtral-8x7b-32768".to_string(),
                "gemma-7b-it".to_string(),
            ],
        }
    }

    pub fn fireworks() -> Self {
        Self {
            name: "fireworks".to_string(),
            display_name: "Fireworks AI".to_string(),
            base_url: "https://api.fireworks.ai/inference/v1".to_string(),
            auth_method: AuthMethod::Bearer,
            chat_completions_path: "/chat/completions".to_string(),
            extra_headers: vec![],
            models: vec![
                "accounts/fireworks/models/llama-v3p1-70b-instruct".to_string(),
                "accounts/fireworks/models/mixtral-8x7b-instruct".to_string(),
            ],
        }
    }

    pub fn deepinfra() -> Self {
        Self {
            name: "deepinfra".to_string(),
            display_name: "DeepInfra".to_string(),
            base_url: "https://api.deepinfra.com/v1/openai".to_string(),
            auth_method: AuthMethod::Bearer,
            chat_completions_path: "/chat/completions".to_string(),
            extra_headers: vec![],
            models: vec![
                "meta-llama/Meta-Llama-3-70B-Instruct".to_string(),
                "mistralai/Mixtral-8x7B-Instruct-v0.1".to_string(),
            ],
        }
    }

    pub fn cerebras() -> Self {
        Self {
            name: "cerebras".to_string(),
            display_name: "Cerebras".to_string(),
            base_url: "https://api.cerebras.ai/v1".to_string(),
            auth_method: AuthMethod::Bearer,
            chat_completions_path: "/chat/completions".to_string(),
            extra_headers: vec![],
            models: vec!["llama3.1-70b".to_string(), "llama3.1-8b".to_string()],
        }
    }

    pub fn novita() -> Self {
        Self {
            name: "novita".to_string(),
            display_name: "Novita AI".to_string(),
            base_url: "https://api.novita.ai/v3/openai".to_string(),
            auth_method: AuthMethod::Bearer,
            chat_completions_path: "/chat/completions".to_string(),
            extra_headers: vec![],
            models: vec![
                "meta-llama/llama-3.1-70b-instruct".to_string(),
                "meta-llama/llama-3.1-8b-instruct".to_string(),
            ],
        }
    }
}

#[cfg(test)]
mod tool_tests {
    use super::*;
    use himadri_core::{Message, Role, Tool, ToolFunction};

    fn provider() -> OpenAiCompatibleProvider {
        OpenAiCompatibleProvider::new(OpenAiCompatibleConfig::openai())
    }

    fn request_with_tools() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "gpt-4o".to_string(),
            messages: vec![Message {
                role: Role::User,
                content: Some(MessageContent::Text("What is the weather?".to_string())),
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
                    description: Some("Get the weather".to_string()),
                    parameters: Some(serde_json::json!({
                        "type": "object",
                        "properties": { "city": { "type": "string" } }
                    })),
                },
            }]),
            tool_choice: Some(serde_json::json!("auto")),
            extra: Default::default(),
        }
    }

    #[test]
    fn build_request_body_forwards_tools_and_choice() {
        let body = provider().build_request_body(&request_with_tools(), false);
        assert_eq!(body["tools"][0]["function"]["name"], "get_weather");
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tool_choice"], "auto");
    }

    #[test]
    fn parse_response_surfaces_tool_calls() {
        let response = serde_json::json!({
            "id": "chatcmpl-1",
            "model": "gpt-4o",
            "created": 1,
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "get_weather", "arguments": "{\"city\":\"Paris\"}" }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });
        let parsed = provider().parse_response(response).unwrap();
        let tool_calls = parsed.choices[0]
            .message
            .tool_calls
            .as_ref()
            .expect("tool_calls should be surfaced");
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].function.name, "get_weather");
        assert_eq!(tool_calls[0].id, "call_1");
    }

    #[test]
    fn parse_response_without_tool_calls_is_none() {
        let response = serde_json::json!({
            "id": "chatcmpl-2",
            "model": "gpt-4o",
            "created": 1,
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "hello" },
                "finish_reason": "stop"
            }]
        });
        let parsed = provider().parse_response(response).unwrap();
        assert!(parsed.choices[0].message.tool_calls.is_none());
    }
}
