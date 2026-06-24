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

    fn cache_key(request: &himadri_core::ChatCompletionRequest) -> String {
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
