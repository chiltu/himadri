use std::collections::HashMap;
use std::time::Duration;

use himadri_core::{AuthContext, ChatCompletionRequest, ChatCompletionResponse};

#[derive(Debug, Clone)]
pub struct PluginContext {
    pub request_id: String,
    pub request: ChatCompletionRequest,
    pub auth: Option<AuthContext>,
    pub metadata: HashMap<String, serde_json::Value>,
    pub provider: Option<String>,
    pub latency: Option<Duration>,
    pub tokens_used: Option<u32>,
    pub error: Option<String>,
    pub response: Option<ChatCompletionResponse>,
    pub response_text: Option<String>,
    pub response_chunks: Vec<String>,
    pub remote_ip: Option<String>,
}

impl PluginContext {
    pub fn from_request(request: &ChatCompletionRequest, auth: Option<&AuthContext>) -> Self {
        Self {
            request_id: uuid::Uuid::new_v4().to_string(),
            request: request.clone(),
            auth: auth.cloned(),
            metadata: HashMap::new(),
            provider: None,
            latency: None,
            tokens_used: None,
            error: None,
            response: None,
            response_text: None,
            response_chunks: Vec::new(),
            remote_ip: None,
        }
    }

    pub fn set_provider(&mut self, provider: String) {
        self.provider = Some(provider);
    }

    pub fn set_latency(&mut self, latency: Duration) {
        self.latency = Some(latency);
    }

    pub fn set_tokens(&mut self, tokens: u32) {
        self.tokens_used = Some(tokens);
    }

    pub fn set_error(&mut self, error: String) {
        self.error = Some(error);
    }

    pub fn set_metadata(&mut self, key: String, value: serde_json::Value) {
        self.metadata.insert(key, value);
    }

    pub fn get_metadata(&self, key: &str) -> Option<&serde_json::Value> {
        self.metadata.get(key)
    }

    pub fn org_id(&self) -> Option<&str> {
        self.auth.as_ref().and_then(|a| a.org_id.as_deref())
    }

    pub fn team_id(&self) -> Option<&str> {
        self.auth.as_ref().and_then(|a| a.team_id.as_deref())
    }

    pub fn user_id(&self) -> Option<&str> {
        self.auth.as_ref().and_then(|a| a.user_id.as_deref())
    }

    pub fn key_id(&self) -> Option<&str> {
        self.auth.as_ref().and_then(|a| a.key_id.as_deref())
    }

    pub fn set_response(&mut self, response: ChatCompletionResponse) {
        self.response = Some(response);
    }

    pub fn set_response_text(&mut self, text: String) {
        self.response_text = Some(text);
    }

    pub fn push_response_chunk(&mut self, chunk: String) {
        self.response_chunks.push(chunk);
    }

    pub fn get_full_response_text(&self) -> String {
        if let Some(ref text) = self.response_text {
            return text.clone();
        }
        self.response_chunks.join("")
    }
}
