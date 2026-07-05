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

/// Why a plugin rejected the request. The gateway maps this to the HTTP
/// status the client sees, so a rate-limit rejection surfaces as 429 (with
/// backoff semantics) rather than a generic 400.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectKind {
    /// The request itself is invalid (blocked content, over a hard cap) — 400.
    BadRequest,
    /// A rate limit was hit — 429 with a Retry-After hint.
    RateLimited { retry_after_secs: u64 },
    /// A spend/budget cap was exhausted — 429 (retry after the window resets).
    BudgetExceeded,
    /// The principal is not permitted to do this — 403.
    Forbidden,
}

#[derive(Debug, Error)]
pub enum PluginError {
    #[error("plugin {name} rejected request: {reason}")]
    Rejected {
        name: String,
        reason: String,
        kind: RejectKind,
    },

    #[error("plugin error: {0}")]
    Internal(String),
}

impl PluginError {
    /// Convenience constructor for the common 400-style rejection.
    pub fn rejected(name: impl Into<String>, reason: impl Into<String>) -> Self {
        PluginError::Rejected {
            name: name.into(),
            reason: reason.into(),
            kind: RejectKind::BadRequest,
        }
    }
}
