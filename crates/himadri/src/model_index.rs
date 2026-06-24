use std::collections::HashMap;

use himadri_core::Target;

/// Pre-built index for fast model-to-provider lookups.
/// Rebuilt when providers are registered/unregistered.
#[derive(Debug, Default)]
pub struct ModelLookupIndex {
    /// Model name -> list of provider names that support it
    exact_providers: HashMap<String, Vec<String>>,
}

impl ModelLookupIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuild the index from a list of providers and their supported models.
    pub fn rebuild(&mut self, providers: &[(String, Vec<String>)]) {
        self.exact_providers.clear();
        for (provider_name, models) in providers {
            for model in models {
                self.exact_providers
                    .entry(model.clone())
                    .or_default()
                    .push(provider_name.clone());
            }
        }
    }

    /// Look up providers that support a specific model.
    /// Returns empty vec if no providers support the model.
    pub fn lookup(&self, model: &str) -> Vec<String> {
        self.exact_providers.get(model).cloned().unwrap_or_default()
    }

    /// Check if any provider supports a model.
    pub fn has_provider_for_model(&self, model: &str) -> bool {
        self.exact_providers.contains_key(model)
    }

    /// Get all indexed models.
    pub fn all_models(&self) -> Vec<&str> {
        self.exact_providers.keys().map(|s| s.as_str()).collect()
    }
}

/// Select the best target for a model from available targets.
pub fn select_target_for_model(
    model: &str,
    targets: &[Target],
    model_index: &ModelLookupIndex,
) -> Option<Target> {
    // Phase 1: Exact index lookup
    let supported_providers = model_index.lookup(model);

    // Find first target whose provider supports this model
    for target in targets {
        if supported_providers.contains(&target.provider) {
            return Some(target.clone());
        }
    }

    // Phase 2: Fallback - return first target (provider may support via prefix matching)
    targets.first().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_index_lookup() {
        let mut index = ModelLookupIndex::new();
        index.rebuild(&[
            (
                "openai".to_string(),
                vec!["gpt-4".to_string(), "gpt-4o".to_string()],
            ),
            ("anthropic".to_string(), vec!["claude-3".to_string()]),
        ]);

        assert_eq!(index.lookup("gpt-4"), vec!["openai"]);
        assert_eq!(index.lookup("claude-3"), vec!["anthropic"]);
        assert!(index.lookup("nonexistent").is_empty());
        assert!(index.has_provider_for_model("gpt-4"));
        assert!(!index.has_provider_for_model("nonexistent"));
    }

    #[test]
    fn test_select_target_for_model() {
        let mut index = ModelLookupIndex::new();
        index.rebuild(&[
            ("openai".to_string(), vec!["gpt-4".to_string()]),
            ("anthropic".to_string(), vec!["claude-3".to_string()]),
        ]);

        let targets = vec![
            Target {
                provider: "openai".to_string(),
                weight: 1.0,
                models: None,
                api_key_env: None,
                base_url: None,
            },
            Target {
                provider: "anthropic".to_string(),
                weight: 1.0,
                models: None,
                api_key_env: None,
                base_url: None,
            },
        ];

        let result = select_target_for_model("gpt-4", &targets, &index);
        assert_eq!(result.unwrap().provider, "openai");

        let result = select_target_for_model("claude-3", &targets, &index);
        assert_eq!(result.unwrap().provider, "anthropic");
    }
}
