pub mod anthropic;
pub mod bedrock;
pub mod compatible;
pub mod error;
pub mod gemini;
pub mod http_client;
pub mod sse;
pub mod traits;
pub mod types;

pub use error::ProviderError;
pub use http_client::ProviderHttpClient;
pub use traits::Provider;

// Generic OpenAI-compatible provider (use this for most providers)
pub use compatible::{AuthMethod, OpenAiCompatibleConfig, OpenAiCompatibleProvider};

// Specific provider implementations (only for non-OpenAI-compatible APIs)
pub use anthropic::AnthropicProvider;
pub use bedrock::BedrockProvider;
pub use gemini::GeminiProvider;

// Convenience re-exports for backward compatibility
pub use compatible::OpenAiCompatibleProvider as OpenAiProvider;

// Re-export core types for convenience
pub use himadri_core::{
    ChatCompletionRequest, ChatCompletionResponse, Choice, ContentPart, Delta, MessageContent,
    ResponseMessage, Role, StreamChoice, StreamChunk, Usage,
};

#[cfg(test)]
mod tests;
