use async_trait::async_trait;
use std::sync::Arc;

use himadri_plugin::context::PluginContext;
use himadri_plugin::traits::{Plugin, PluginError, PluginType, Stage};

pub struct MaxTokenPlugin {
    max_tokens: u32,
}

impl MaxTokenPlugin {
    pub fn new(max_tokens: u32) -> Arc<Self> {
        Arc::new(Self { max_tokens })
    }
}

#[async_trait]
impl Plugin for MaxTokenPlugin {
    fn name(&self) -> &str {
        "max-token"
    }

    fn plugin_type(&self) -> PluginType {
        PluginType::Guardrail
    }

    fn stage(&self) -> Stage {
        Stage::BeforeRequest
    }

    async fn execute(&self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        if let Some(max_tokens) = ctx.request.max_tokens {
            if max_tokens > self.max_tokens {
                return Err(PluginError::Rejected {
                    name: self.name().to_string(),
                    reason: format!(
                        "max_tokens {} exceeds limit of {}",
                        max_tokens, self.max_tokens
                    ),
                });
            }
        }

        Ok(())
    }
}
