use std::sync::Arc;
use tracing::{debug, error, instrument};

use crate::context::PluginContext;
use crate::traits::{Plugin, PluginError, ResponseAction, ResponseGuardrail, Stage};

#[derive(Default)]
pub struct PluginManager {
    before_request: Vec<Arc<dyn Plugin>>,
    after_request: Vec<Arc<dyn Plugin>>,
    after_response: Vec<Arc<dyn Plugin>>,
    on_error: Vec<Arc<dyn Plugin>>,
    response_guardrails: Vec<Arc<dyn ResponseGuardrail>>,
}

impl PluginManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, plugin: Arc<dyn Plugin>) {
        // Opt-in second registration in the after-request stage (e.g. budget
        // plugin records cost from the response). Skip if the primary stage is
        // already AfterRequest to avoid running it twice.
        if plugin.also_after_request() && plugin.stage() != Stage::AfterRequest {
            self.after_request.push(plugin.clone());
        }
        match plugin.stage() {
            Stage::BeforeRequest => self.before_request.push(plugin),
            Stage::AfterRequest => self.after_request.push(plugin),
            Stage::AfterResponse => self.after_response.push(plugin),
            Stage::OnError => self.on_error.push(plugin),
        }
    }

    pub fn register_response_guardrail(&mut self, guardrail: Arc<dyn ResponseGuardrail>) {
        self.response_guardrails.push(guardrail);
    }

    /// Names of the plugins registered for a stage, in execution order.
    /// This is how composition is observed (wire-up tests, diagnostics) —
    /// the stage vecs themselves stay private.
    pub fn stage_names(&self, stage: Stage) -> Vec<&str> {
        let plugins = match stage {
            Stage::BeforeRequest => &self.before_request,
            Stage::AfterRequest => &self.after_request,
            Stage::AfterResponse => &self.after_response,
            Stage::OnError => &self.on_error,
        };
        plugins.iter().map(|p| p.name()).collect()
    }

    /// Names of the response guardrails, in execution order.
    pub fn response_guardrail_names(&self) -> Vec<&str> {
        self.response_guardrails.iter().map(|g| g.name()).collect()
    }

    #[instrument(skip(self, ctx), fields(plugin_count = self.before_request.len()))]
    pub async fn run_before(&self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        for plugin in &self.before_request {
            debug!("Running before-request plugin: {}", plugin.name());
            plugin.execute(ctx).await?;
        }
        Ok(())
    }

    /// Run a stage's plugins, logging (never propagating) failures. These
    /// stages observe an already-produced (and often paid-for) response, so
    /// a failing observer must not fail the request — the `()` return encodes
    /// that contract.
    async fn run_logged(&self, plugins: &[Arc<dyn Plugin>], ctx: &mut PluginContext, stage: &str) {
        for plugin in plugins {
            debug!("Running {stage} plugin: {}", plugin.name());
            if let Err(e) = plugin.execute(ctx).await {
                error!("{stage} plugin {} failed: {}", plugin.name(), e);
            }
        }
    }

    pub async fn run_after(&self, ctx: &mut PluginContext) {
        self.run_logged(&self.after_request, ctx, "after-request")
            .await;
    }

    pub async fn run_after_response(&self, ctx: &mut PluginContext) {
        self.run_logged(&self.after_response, ctx, "after-response")
            .await;
    }

    #[instrument(skip(self, ctx), fields(guardrail_count = self.response_guardrails.len()))]
    pub async fn run_response_guardrails(
        &self,
        ctx: &PluginContext,
        response: &str,
    ) -> Result<ResponseAction, PluginError> {
        for guardrail in &self.response_guardrails {
            debug!("Running response guardrail: {}", guardrail.name());
            match guardrail.check_response(ctx, response).await? {
                ResponseAction::Allow => {}
                action => return Ok(action),
            }
        }
        Ok(ResponseAction::Allow)
    }

    pub async fn run_on_error(&self, ctx: &mut PluginContext) {
        self.run_logged(&self.on_error, ctx, "on-error").await;
    }
}
