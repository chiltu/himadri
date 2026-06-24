use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;

use crate::error::ProviderError;
use himadri_core::{ChatCompletionRequest, ChatCompletionResponse, StreamChunk};

pub type BoxStream<'a, T> = Pin<Box<dyn Stream<Item = T> + Send + 'a>>;

#[async_trait]
pub trait Provider: Send + Sync {
    /// Provider identifier (e.g., "openai", "anthropic")
    fn name(&self) -> &str;

    /// Provider display name
    fn display_name(&self) -> &str {
        self.name()
    }

    /// Supported models (if known)
    fn supported_models(&self) -> Vec<String> {
        vec![]
    }

    /// Non-streaming completion
    async fn complete(
        &self,
        request: &ChatCompletionRequest,
        api_key: &str,
    ) -> Result<ChatCompletionResponse, ProviderError>;

    /// Streaming completion
    async fn complete_stream(
        &self,
        request: &ChatCompletionRequest,
        api_key: &str,
    ) -> Result<BoxStream<'static, Result<StreamChunk, ProviderError>>, ProviderError>;
}
