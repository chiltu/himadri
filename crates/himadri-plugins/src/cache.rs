use async_trait::async_trait;
use moka::future::Cache;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;

use himadri_plugin::context::PluginContext;
use himadri_plugin::traits::{Plugin, PluginError, PluginType, Stage};

#[derive(Clone)]
pub struct CachedResponse {
    pub response: himadri_core::ChatCompletionResponse,
    pub cached_at: chrono::DateTime<chrono::Utc>,
}

pub struct ResponseCachePlugin {
    cache: Cache<String, CachedResponse>,
}

impl ResponseCachePlugin {
    pub fn new(max_capacity: u64, ttl: Duration) -> Arc<Self> {
        Arc::new(Self {
            cache: Cache::builder()
                .max_capacity(max_capacity)
                .time_to_live(ttl)
                .build(),
        })
    }

    /// Look up a previously cached response for this request, if any.
    pub async fn get(
        &self,
        request: &himadri_core::ChatCompletionRequest,
    ) -> Option<himadri_core::ChatCompletionResponse> {
        let key = Self::cache_key(request);
        self.cache.get(&key).await.map(|c| c.response)
    }

    /// Store a response for this request.
    pub async fn insert(
        &self,
        request: &himadri_core::ChatCompletionRequest,
        response: himadri_core::ChatCompletionResponse,
    ) {
        let key = Self::cache_key(request);
        self.cache
            .insert(
                key,
                CachedResponse {
                    response,
                    cached_at: chrono::Utc::now(),
                },
            )
            .await;
    }

    pub fn cache_key(request: &himadri_core::ChatCompletionRequest) -> String {
        let mut hasher = Sha256::new();
        hasher.update(&request.model);

        for message in &request.messages {
            if let Some(content) = &message.content {
                let text = match content {
                    himadri_core::MessageContent::Text(text) => text.clone(),
                    himadri_core::MessageContent::Parts(parts) => parts
                        .iter()
                        .filter_map(|p| match p {
                            himadri_core::ContentPart::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join(""),
                };
                hasher.update(text.as_bytes());
            }
        }

        if let Some(temp) = request.temperature {
            hasher.update(temp.to_le_bytes());
        }

        hex::encode(hasher.finalize())
    }
}

#[async_trait]
impl Plugin for ResponseCachePlugin {
    fn name(&self) -> &str {
        "response-cache"
    }

    fn plugin_type(&self) -> PluginType {
        PluginType::Cache
    }

    fn stage(&self) -> Stage {
        Stage::BeforeRequest
    }

    async fn execute(&self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        // Skip cache for streaming requests
        if ctx.request.stream {
            return Ok(());
        }

        let key = Self::cache_key(&ctx.request);

        if let Some(cached) = self.cache.get(&key).await {
            ctx.set_metadata("cached".to_string(), serde_json::Value::Bool(true));
            ctx.set_metadata(
                "cached_response".to_string(),
                serde_json::to_value(&cached.response).unwrap_or_default(),
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use himadri_core::{
        ChatCompletionRequest, ChatCompletionResponse, Choice, Message, MessageContent,
        ResponseMessage, Role,
    };

    fn request(model: &str, prompt: &str) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: model.to_string(),
            messages: vec![Message {
                role: Role::User,
                content: Some(MessageContent::Text(prompt.to_string())),
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
            tools: None,
            tool_choice: None,
            extra: Default::default(),
        }
    }

    fn response(content: &str) -> ChatCompletionResponse {
        ChatCompletionResponse {
            id: "resp-1".to_string(),
            object: "chat.completion".to_string(),
            created: 0,
            model: "gpt-4".to_string(),
            choices: vec![Choice {
                index: 0,
                message: ResponseMessage {
                    role: Role::Assistant,
                    content: Some(content.to_string()),
                    tool_calls: None,
                },
                finish_reason: Some("stop".to_string()),
            }],
            usage: None,
            system_fingerprint: None,
        }
    }

    #[tokio::test]
    async fn miss_then_hit_roundtrip() {
        let cache = ResponseCachePlugin::new(100, Duration::from_secs(60));
        let req = request("gpt-4", "hello");

        assert!(cache.get(&req).await.is_none(), "cold cache should miss");

        cache.insert(&req, response("hi there")).await;

        let hit = cache.get(&req).await.expect("warm cache should hit");
        assert_eq!(hit.choices[0].message.content.as_deref(), Some("hi there"));
    }

    #[tokio::test]
    async fn different_prompts_do_not_collide() {
        let cache = ResponseCachePlugin::new(100, Duration::from_secs(60));
        cache.insert(&request("gpt-4", "first"), response("A")).await;
        cache
            .insert(&request("gpt-4", "second"), response("B"))
            .await;

        assert_eq!(
            cache
                .get(&request("gpt-4", "first"))
                .await
                .unwrap()
                .choices[0]
                .message
                .content
                .as_deref(),
            Some("A")
        );
        assert_eq!(
            cache
                .get(&request("gpt-4", "second"))
                .await
                .unwrap()
                .choices[0]
                .message
                .content
                .as_deref(),
            Some("B")
        );
    }
}
