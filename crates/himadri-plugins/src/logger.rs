use async_trait::async_trait;
use std::sync::Arc;
use tracing::info;

use himadri_plugin::context::PluginContext;
use himadri_plugin::traits::{Plugin, PluginError, PluginType, Stage};

pub struct RequestLoggerPlugin;

impl RequestLoggerPlugin {
    pub fn new() -> Arc<Self> {
        Arc::new(Self)
    }
}

#[async_trait]
impl Plugin for RequestLoggerPlugin {
    fn name(&self) -> &str {
        "request-logger"
    }

    fn plugin_type(&self) -> PluginType {
        PluginType::Logger
    }

    fn stage(&self) -> Stage {
        Stage::AfterRequest
    }

    async fn execute(&self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        let model = &ctx.request.model;
        let provider = ctx.provider.as_deref().unwrap_or("unknown");
        let latency = ctx
            .latency
            .map(|l| format!("{}ms", l.as_millis()))
            .unwrap_or_default();
        let tokens = ctx.tokens_used.map(|t| t.to_string()).unwrap_or_default();
        let error = ctx.error.as_deref().unwrap_or("");

        info!(
            model = %model,
            provider = %provider,
            latency = %latency,
            tokens = %tokens,
            error = %error,
            "Request completed"
        );

        Ok(())
    }
}

impl Default for RequestLoggerPlugin {
    fn default() -> Self {
        Self
    }
}
