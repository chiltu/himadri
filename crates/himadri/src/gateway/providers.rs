//! Provider-client construction for DB-configured model endpoints.

use std::sync::Arc;

use himadri_provider::traits::Provider;

/// Build a provider client for a model endpoint given its `provider_type` and
/// optional `base_url`. Known vendor types start from their built-in preset
/// (correct default base URL, auth, extra headers); `base_url`, when set,
/// overrides it. Non-preset types require an explicit `base_url` and are served
/// by a generic Bearer OpenAI-compatible client; without one this returns
/// `None` (the caller logs and skips). The client is registered under the
/// endpoint id by the caller, so its internal `name()` is only a label.
pub(super) fn build_provider_client(
    provider_type: &str,
    base_url: Option<&str>,
) -> Option<Arc<dyn Provider>> {
    use himadri_provider::{
        AnthropicProvider, GeminiProvider, OpenAiCompatibleConfig, OpenAiCompatibleProvider,
    };

    let base_url = base_url.map(str::trim).filter(|s| !s.is_empty());

    match provider_type {
        "anthropic" => Some(Arc::new(AnthropicProvider::new(base_url))),
        "gemini" => Some(Arc::new(GeminiProvider::new(base_url))),
        name => {
            let preset = match name {
                "openai" => Some(OpenAiCompatibleConfig::openai()),
                "openrouter" => Some(OpenAiCompatibleConfig::openrouter()),
                "together" => Some(OpenAiCompatibleConfig::together_ai()),
                "groq" => Some(OpenAiCompatibleConfig::groq()),
                "fireworks" => Some(OpenAiCompatibleConfig::fireworks()),
                "deepinfra" => Some(OpenAiCompatibleConfig::deepinfra()),
                "cerebras" => Some(OpenAiCompatibleConfig::cerebras()),
                "novita" => Some(OpenAiCompatibleConfig::novita()),
                _ => None,
            };

            let mut config = match preset {
                Some(config) => config,
                // Unknown vendor: a generic Bearer client needs an explicit URL.
                None => {
                    let base_url = base_url?;
                    return Some(Arc::new(OpenAiCompatibleProvider::bearer(name, base_url)));
                }
            };
            config.name = provider_type.to_string();
            if let Some(base_url) = base_url {
                config.base_url = base_url.to_string();
            }
            Some(Arc::new(OpenAiCompatibleProvider::new(config)))
        }
    }
}
