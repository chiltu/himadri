use async_trait::async_trait;
use futures::StreamExt;
use serde_json;
use tracing::{debug, instrument};

use crate::error::ProviderError;
use crate::http_client::CLIENT_POOL;
use crate::traits::{BoxStream, Provider};
use himadri_core::{ChatCompletionRequest, ChatCompletionResponse, StreamChunk};

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
        // `ChatCompletionRequest` already serializes to the OpenAI wire shape
        // (lowercase roles, untagged text/parts content, tools, and — via the
        // flattened `extra` map — any passthrough params). Serializing it
        // directly forwards `tool_calls`/`tool_call_id` and passthrough params
        // that the previous hand-built body silently dropped.
        let mut body = serde_json::to_value(request).unwrap_or_else(|_| serde_json::json!({}));
        body["stream"] = serde_json::Value::Bool(stream);
        if stream {
            // Ask for usage in the final chunk (OpenAI streaming sends none
            // otherwise), so the gateway can meter streamed requests.
            body["stream_options"] = serde_json::json!({ "include_usage": true });
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

    /// POST `body` as JSON to `url` with the provider's auth + extra headers,
    /// returning the response on 2xx or a mapped `ProviderError` otherwise.
    /// Shared by `complete` / `complete_stream` / `embed`.
    async fn send<T: serde::Serialize + ?Sized>(
        &self,
        client: &reqwest::Client,
        url: String,
        api_key: &str,
        body: &T,
        streaming: bool,
    ) -> Result<reqwest::Response, ProviderError> {
        let (auth_name, auth_value) = self.build_auth_header(api_key);
        let mut req = client
            .post(url)
            .header(&auth_name, &auth_value)
            .header("Content-Type", "application/json");
        if streaming {
            req = req.header("Accept", "text/event-stream");
        }
        for (name, value) in &self.config.extra_headers {
            req = req.header(name.as_str(), value.as_str());
        }
        let response = req.json(body).send().await?;
        if !response.status().is_success() {
            return Err(self.handle_error(response).await);
        }
        Ok(response)
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

        let response = self
            .send(&client, self.get_url(), api_key, &body, false)
            .await?;

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

        let response = self
            .send(&client, self.get_url(), api_key, &body, true)
            .await?;

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
        let response = self
            .send(&client, self.get_embeddings_url(), api_key, request, false)
            .await?;

        response
            .json::<himadri_core::EmbeddingResponse>()
            .await
            .map_err(|e| ProviderError::Parse(e.to_string()))
    }
}

// ─── Pre-configured Providers ────────────────────────────────────────

impl OpenAiCompatibleConfig {
    /// Build a Bearer-auth preset at the standard `/chat/completions` path.
    /// The vendor presets below differ only in name, URL, extra headers, and
    /// advertised model list.
    fn preset(
        name: &str,
        display_name: &str,
        base_url: &str,
        extra_headers: Vec<(String, String)>,
        models: &[&str],
    ) -> Self {
        Self {
            name: name.to_string(),
            display_name: display_name.to_string(),
            base_url: base_url.to_string(),
            auth_method: AuthMethod::Bearer,
            chat_completions_path: "/chat/completions".to_string(),
            extra_headers,
            models: models.iter().map(|m| m.to_string()).collect(),
        }
    }

    pub fn openai() -> Self {
        Self::preset(
            "openai",
            "OpenAI",
            "https://api.openai.com/v1",
            vec![],
            &[
                "gpt-4",
                "gpt-4-turbo",
                "gpt-4o",
                "gpt-4o-mini",
                "gpt-3.5-turbo",
                "o1",
                "o1-mini",
                "o1-pro",
            ],
        )
    }

    pub fn openrouter() -> Self {
        Self::preset(
            "openrouter",
            "OpenRouter",
            "https://openrouter.ai/api/v1",
            vec![
                (
                    "HTTP-Referer".to_string(),
                    "https://github.com/himadri".to_string(),
                ),
                ("X-Title".to_string(), "himadri".to_string()),
            ],
            &[
                "openrouter/auto",
                "openai/gpt-4o",
                "anthropic/claude-3.5-sonnet",
                "google/gemini-2.0-flash",
            ],
        )
    }

    pub fn together_ai() -> Self {
        Self::preset(
            "together",
            "Together AI",
            "https://api.together.xyz/v1",
            vec![],
            &[
                "meta-llama/Llama-3-70b-chat-hf",
                "meta-llama/Llama-3-8b-chat-hf",
                "mistralai/Mixtral-8x7B-Instruct-v0.1",
            ],
        )
    }

    pub fn groq() -> Self {
        Self::preset(
            "groq",
            "Groq",
            "https://api.groq.com/openai/v1",
            vec![],
            &[
                "llama3-70b-8192",
                "llama3-8b-8192",
                "mixtral-8x7b-32768",
                "gemma-7b-it",
            ],
        )
    }

    pub fn fireworks() -> Self {
        Self::preset(
            "fireworks",
            "Fireworks AI",
            "https://api.fireworks.ai/inference/v1",
            vec![],
            &[
                "accounts/fireworks/models/llama-v3p1-70b-instruct",
                "accounts/fireworks/models/mixtral-8x7b-instruct",
            ],
        )
    }

    pub fn deepinfra() -> Self {
        Self::preset(
            "deepinfra",
            "DeepInfra",
            "https://api.deepinfra.com/v1/openai",
            vec![],
            &[
                "meta-llama/Meta-Llama-3-70B-Instruct",
                "mistralai/Mixtral-8x7B-Instruct-v0.1",
            ],
        )
    }

    pub fn cerebras() -> Self {
        Self::preset(
            "cerebras",
            "Cerebras",
            "https://api.cerebras.ai/v1",
            vec![],
            &["llama3.1-70b", "llama3.1-8b"],
        )
    }

    pub fn novita() -> Self {
        Self::preset(
            "novita",
            "Novita AI",
            "https://api.novita.ai/v3/openai",
            vec![],
            &[
                "meta-llama/llama-3.1-70b-instruct",
                "meta-llama/llama-3.1-8b-instruct",
            ],
        )
    }
}

#[cfg(test)]
mod tool_tests {
    use super::*;
    use himadri_core::{Message, MessageContent, Role, Tool, ToolFunction};

    fn provider() -> OpenAiCompatibleProvider {
        OpenAiCompatibleProvider::new(OpenAiCompatibleConfig::openai())
    }

    fn request_with_tools() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "gpt-4o".to_string(),
            messages: vec![Message::user("What is the weather?")],
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
            ..Default::default()
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
    fn build_request_body_forwards_tool_calls_and_passthrough_params() {
        // Regression (R1): the previous hand-built body dropped assistant
        // `tool_calls`, `role: tool` `tool_call_id`, and everything in the
        // flattened `extra` map (response_format, seed, n, …).
        let mut request = ChatCompletionRequest {
            model: "gpt-4o".to_string(),
            messages: vec![
                Message {
                    role: Role::Assistant,
                    tool_calls: Some(vec![himadri_core::ToolCall {
                        id: "call_1".to_string(),
                        tool_type: "function".to_string(),
                        function: himadri_core::FunctionCall {
                            name: "get_weather".to_string(),
                            arguments: "{\"city\":\"Paris\"}".to_string(),
                        },
                    }]),
                    ..Default::default()
                },
                Message {
                    role: Role::Tool,
                    content: Some(MessageContent::text("sunny")),
                    tool_call_id: Some("call_1".to_string()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        request.extra.insert(
            "response_format".to_string(),
            serde_json::json!({ "type": "json_object" }),
        );
        request
            .extra
            .insert("seed".to_string(), serde_json::json!(42));

        let body = provider().build_request_body(&request, false);
        assert_eq!(
            body["messages"][0]["tool_calls"][0]["id"], "call_1",
            "assistant tool_calls must survive"
        );
        assert_eq!(
            body["messages"][1]["tool_call_id"], "call_1",
            "tool message tool_call_id must survive"
        );
        assert_eq!(
            body["response_format"]["type"], "json_object",
            "passthrough extra params must survive"
        );
        assert_eq!(body["seed"], 42);
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
