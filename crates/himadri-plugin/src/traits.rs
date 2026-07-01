use async_trait::async_trait;
use thiserror::Error;

use crate::context::PluginContext;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginType {
    Guardrail,
    Middleware,
    Cache,
    Logger,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    BeforeRequest,
    AfterRequest,
    AfterResponse,
    OnError,
}

#[async_trait]
pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;

    fn plugin_type(&self) -> PluginType;

    fn stage(&self) -> Stage;

    /// Whether this plugin should *also* run during the after-request stage, in
    /// addition to its primary `stage()`. Used by plugins that both gate a
    /// request (before) and record an outcome from the response (after), such as
    /// the budget plugin. Defaults to `false`.
    fn also_after_request(&self) -> bool {
        false
    }

    async fn execute(&self, ctx: &mut PluginContext) -> Result<(), PluginError>;
}

#[async_trait]
pub trait ResponseGuardrail: Send + Sync {
    fn name(&self) -> &str;

    async fn check_response(
        &self,
        ctx: &PluginContext,
        response: &str,
    ) -> Result<ResponseAction, PluginError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResponseAction {
    Allow,
    Reject(String),
    Redact(String),
}

#[derive(Debug, Error)]
pub enum PluginError {
    #[error("plugin {name} rejected request: {reason}")]
    Rejected { name: String, reason: String },

    #[error("plugin error: {0}")]
    Internal(String),
}
