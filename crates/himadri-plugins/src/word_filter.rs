use async_trait::async_trait;
use std::sync::Arc;

use himadri_plugin::context::PluginContext;
use himadri_plugin::traits::{Plugin, PluginError, PluginType, Stage};

pub struct WordFilterPlugin {
    blocked_words: Vec<String>,
}

impl WordFilterPlugin {
    pub fn new(blocked_words: Vec<String>) -> Arc<Self> {
        Arc::new(Self {
            blocked_words: blocked_words
                .into_iter()
                .map(|w| w.to_lowercase())
                .collect(),
        })
    }
}

#[async_trait]
impl Plugin for WordFilterPlugin {
    fn name(&self) -> &str {
        "word-filter"
    }

    fn plugin_type(&self) -> PluginType {
        PluginType::Guardrail
    }

    fn stage(&self) -> Stage {
        Stage::BeforeRequest
    }

    async fn execute(&self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        for message in &ctx.request.messages {
            if let Some(content) = &message.content {
                let lower_text = content.flat_text().to_lowercase();
                for word in &self.blocked_words {
                    if lower_text.contains(word) {
                        return Err(PluginError::Rejected {
                            name: self.name().to_string(),
                            reason: format!("Blocked word detected: {}", word),
                            kind: himadri_plugin::traits::RejectKind::BadRequest,
                        });
                    }
                }
            }
        }

        Ok(())
    }
}
