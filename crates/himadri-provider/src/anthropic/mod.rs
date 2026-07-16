pub mod provider;

use std::sync::Arc;

use crate::registry::MapProviderRegistry;

pub use provider::AnthropicProvider;

/// Register Anthropic's native API under `"anthropic"`.
pub fn register(registry: &mut MapProviderRegistry) {
    registry.register("anthropic", |base_url| {
        Ok(Arc::new(AnthropicProvider::new(base_url)))
    });
}
