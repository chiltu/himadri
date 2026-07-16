pub mod provider;

use std::sync::Arc;

use crate::registry::MapProviderRegistry;

pub use provider::AuthMethod;
pub use provider::OpenAiCompatibleConfig;
pub use provider::OpenAiCompatibleProvider;

/// A preset vendor: its `provider_type` name and the factory for its built-in
/// [`OpenAiCompatibleConfig`] (default base URL, auth scheme, extra headers).
///
/// The factory's `name` field always equals the `provider_type` here, and its
/// `display_name` is the vendor's human-readable label — callers should read
/// both off the config rather than keeping their own copies.
pub type Preset = (&'static str, fn() -> OpenAiCompatibleConfig);

/// The OpenAI-compatible vendors that ship with a built-in preset. Adding a
/// vendor is one row plus its [`OpenAiCompatibleConfig`] factory.
const PRESETS: &[Preset] = &[
    ("openai", OpenAiCompatibleConfig::openai),
    ("openrouter", OpenAiCompatibleConfig::openrouter),
    ("together", OpenAiCompatibleConfig::together_ai),
    ("groq", OpenAiCompatibleConfig::groq),
    ("fireworks", OpenAiCompatibleConfig::fireworks),
    ("deepinfra", OpenAiCompatibleConfig::deepinfra),
    ("cerebras", OpenAiCompatibleConfig::cerebras),
    ("novita", OpenAiCompatibleConfig::novita),
];

/// Every OpenAI-compatible vendor that ships with a preset.
///
/// The single list of built-in vendors: DB mode builds its registry from it, and
/// ENV mode gates each one on its API-key env var. Adding a row to [`PRESETS`]
/// enables the vendor in both modes.
pub fn presets() -> impl Iterator<Item = Preset> {
    PRESETS.iter().copied()
}

/// Register every preset OpenAI-compatible vendor under its own type name.
pub fn register(registry: &mut MapProviderRegistry) {
    for &(name, preset) in PRESETS {
        registry.register(name, move |base_url| {
            let mut config = preset();
            // The client is registered under the endpoint id by the caller, so
            // `name` here is only a label.
            config.name = name.to_string();
            if let Some(base_url) = base_url {
                config.base_url = base_url.to_string();
            }
            Ok(Arc::new(OpenAiCompatibleProvider::new(config)))
        });
    }
}
