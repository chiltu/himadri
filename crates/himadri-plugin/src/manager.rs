use std::sync::Arc;
use tracing::{debug, error, instrument};

use crate::context::PluginContext;
use crate::traits::{Plugin, PluginError, ResponseAction, ResponseGuardrail, Stage};

pub struct PluginManager {
    before_request: Vec<Arc<dyn Plugin>>,
    after_request: Vec<Arc<dyn Plugin>>,
    after_response: Vec<Arc<dyn Plugin>>,
    on_error: Vec<Arc<dyn Plugin>>,
    response_guardrails: Vec<Arc<dyn ResponseGuardrail>>,
}

impl PluginManager {
    pub fn new() -> Self {
        Self {
            before_request: Vec::new(),
            after_request: Vec::new(),
            after_response: Vec::new(),
            on_error: Vec::new(),
            response_guardrails: Vec::new(),
        }
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

    #[instrument(skip(self, ctx), fields(plugin_count = self.before_request.len()))]
    pub async fn run_before(&self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        for plugin in &self.before_request {
            debug!("Running before-request plugin: {}", plugin.name());
            plugin.execute(ctx).await?;
        }
        Ok(())
    }

    /// After-request plugins observe an already-produced (and paid-for)
    /// response, so their failures are logged, never propagated — the
    /// signature encodes that contract.
    #[instrument(skip(self, ctx), fields(plugin_count = self.after_request.len()))]
    pub async fn run_after(&self, ctx: &mut PluginContext) {
        for plugin in &self.after_request {
            debug!("Running after-request plugin: {}", plugin.name());
            if let Err(e) = plugin.execute(ctx).await {
                error!("Plugin {} failed: {}", plugin.name(), e);
            }
        }
    }

    #[instrument(skip(self, ctx), fields(plugin_count = self.after_response.len()))]
    pub async fn run_after_response(&self, ctx: &mut PluginContext) {
        for plugin in &self.after_response {
            debug!("Running after-response plugin: {}", plugin.name());
            if let Err(e) = plugin.execute(ctx).await {
                error!("After-response plugin {} failed: {}", plugin.name(), e);
            }
        }
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

    #[instrument(skip(self, ctx), fields(plugin_count = self.on_error.len()))]
    pub async fn run_on_error(&self, ctx: &mut PluginContext) {
        for plugin in &self.on_error {
            debug!("Running on-error plugin: {}", plugin.name());
            if let Err(e) = plugin.execute(ctx).await {
                error!("Error plugin {} failed: {}", plugin.name(), e);
            }
        }
    }
}

impl Default for PluginManager {
    fn default() -> Self {
        Self::new()
    }
}
