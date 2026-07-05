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

    /// Hash a canonical JSON form of every request field that affects the
    /// completion. Serializing one JSON document (instead of concatenating
    /// raw field bytes) makes the key unambiguous — `model "gpt-4o"` +
    /// prompt `"hello"` can no longer collide with `model "gpt-4"` +
    /// prompt `"ohello"` — and includes message *roles*, so a system prompt
    /// and a user message with identical text hash differently.
    pub fn cache_key(request: &himadri_core::ChatCompletionRequest) -> String {
        let canonical = serde_json::json!({
            "model": request.model,
            "messages": request.messages,
            "temperature": request.temperature,
            "top_p": request.top_p,
            "max_tokens": request.max_tokens,
            "stop": request.stop,
            "presence_penalty": request.presence_penalty,
            "frequency_penalty": request.frequency_penalty,
            "tools": request.tools,
            "tool_choice": request.tool_choice,
            "user": request.user,
            // Pass-through params (n, seed, response_format, logit_bias, …)
            // land in the flattened `extra` map and materially change the
            // completion; serde_json's default (BTree-backed) map keeps the
            // serialization deterministic.
            "extra": request.extra,
        });
        let bytes = serde_json::to_vec(&canonical).unwrap_or_default();
        hex::encode(Sha256::digest(bytes))
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

    #[test]
    fn key_is_unambiguous_across_field_boundaries() {
        // Regression: raw byte concatenation let model+prompt pairs collide.
        let a = request("gpt-4o", "hello");
        let b = request("gpt-4", "ohello");
        assert_ne!(
            ResponseCachePlugin::cache_key(&a),
            ResponseCachePlugin::cache_key(&b)
        );
    }

    #[test]
    fn key_includes_roles_and_sampling_params() {
        // Same text as a system prompt vs a user message must differ.
        let user = request("gpt-4", "be terse");
        let mut system = request("gpt-4", "be terse");
        system.messages[0].role = Role::System;
        assert_ne!(
            ResponseCachePlugin::cache_key(&user),
            ResponseCachePlugin::cache_key(&system)
        );

        // max_tokens / top_p materially change the completion.
        let mut capped = request("gpt-4", "be terse");
        capped.max_tokens = Some(5);
        assert_ne!(
            ResponseCachePlugin::cache_key(&user),
            ResponseCachePlugin::cache_key(&capped)
        );
    }

    #[test]
    fn key_includes_passthrough_extra_params() {
        // Regression: `response_format`, `seed`, `n`, … ride in the
        // flattened `extra` map and materially change the completion — two
        // requests differing only there must not share a cache entry.
        let plain = request("gpt-4", "give me json");
        let mut json_mode = request("gpt-4", "give me json");
        json_mode.extra.insert(
            "response_format".to_string(),
            serde_json::json!({"type": "json_object"}),
        );
        assert_ne!(
            ResponseCachePlugin::cache_key(&plain),
            ResponseCachePlugin::cache_key(&json_mode)
        );

        let mut with_user = request("gpt-4", "give me json");
        with_user.user = Some("tenant-a".to_string());
        assert_ne!(
            ResponseCachePlugin::cache_key(&plain),
            ResponseCachePlugin::cache_key(&with_user)
        );
    }

    #[tokio::test]
    async fn different_prompts_do_not_collide() {
        let cache = ResponseCachePlugin::new(100, Duration::from_secs(60));
        cache
            .insert(&request("gpt-4", "first"), response("A"))
            .await;
        cache
            .insert(&request("gpt-4", "second"), response("B"))
            .await;

        assert_eq!(
            cache.get(&request("gpt-4", "first")).await.unwrap().choices[0]
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
