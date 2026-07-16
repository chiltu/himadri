pub mod provider;

use std::sync::Arc;

use crate::registry::MapProviderRegistry;

pub use provider::GeminiProvider;

/// Register Gemini's native API under `"gemini"`.
pub fn register(registry: &mut MapProviderRegistry) {
    registry.register("gemini", |base_url| {
        Ok(Arc::new(GeminiProvider::new(base_url)))
    });
}
