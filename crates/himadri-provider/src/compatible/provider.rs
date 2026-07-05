use async_trait::async_trait;
use futures::StreamExt;
use serde_json;
use tracing::{debug, instrument};

use crate::error::ProviderError;
use crate::http_client::CLIENT_POOL;
use crate::traits::{BoxStream, Provider};
use himadri_core::{
    ChatCompletionRequest, ChatCompletionResponse, ContentPart, MessageContent, StreamChunk,
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
#[derive(Clone)]
pub struct OpenAiCompatibleProvider {
    config: OpenAiCompatibleConfig,
}

/// Some providers emit `"tool_calls": []`; normalize to `None` so clients
/// (and the empty-vs-absent distinction in our own tests) see the OpenAI
/// convention of omitting the field entirely.
fn normalize_tool_calls<T>(tool_calls: &mut Option<Vec<T>>) {
    if tool_calls.as_ref().is_some_and(Vec::is_empty) {
        *tool_calls = None;
    }
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
            "/openai/deployments/{}/chat/completions?api-version={}",
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

        if stream {
            // Ask for usage in the final chunk (OpenAI streaming sends none
            // otherwise), so the gateway can meter streamed requests.
            body["stream_options"] = serde_json::json!({ "include_usage": true });
        }

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
        let mut parsed: ChatCompletionResponse = serde_json::from_value(response)
            .map_err(|e| ProviderError::Parse(format!("malformed provider response: {}", e)))?;
        parsed.object = "chat.completion".to_string();
        for choice in &mut parsed.choices {
            normalize_tool_calls(&mut choice.message.tool_calls);
        }
        Ok(parsed)
    }

    fn parse_stream_chunk(&self, data: &str) -> Result<StreamChunk, ProviderError> {
        let mut chunk: StreamChunk = serde_json::from_str(data)
            .map_err(|e| ProviderError::Parse(format!("malformed stream chunk: {}", e)))?;
        chunk.object = "chat.completion.chunk".to_string();
        for choice in &mut chunk.choices {
            normalize_tool_calls(&mut choice.delta.tool_calls);
        }
        Ok(chunk)
    }

    async fn handle_error(&self, response: reqwest::Response) -> ProviderError {
        ProviderError::from_openai_response(response).await
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

        let provider = self.clone();
        let stream = crate::sse::sse_events(response.bytes_stream())
            .map(move |event| event.and_then(|event| provider.parse_stream_chunk(&event.data)));

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
