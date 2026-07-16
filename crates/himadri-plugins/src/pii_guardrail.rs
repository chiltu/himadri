//! Inline PII guardrail: scans request messages before provider dispatch
//! and redacts, blocks, or observes according to configuration
//! (docs/SPEC_GUARDRAILS.md §6.3).
//!
//! The plugin mutates `ctx.request`; the gateway forwards the pipeline's
//! copy of the request (see `Gateway::prepare_request`), so a redaction
//! here is what every provider attempt, the response cache, and the audit
//! log see.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::warn;

use himadri_core::{
    Config, ContentPart, MessageContent, PiiGuardrailConfig, PiiModeConfig, PiiStrategyConfig,
    Role,
};

/// The org/team PII section governing this request, if any: the most specific
/// scope with an explicit `guardrails.pii` wins wholesale — including
/// `enabled: false` to opt out of a global policy. Interpretation of the
/// section (request mode vs response mode) stays with each caller.
fn scoped_pii_section<'a>(cfg: &'a Config, ctx: &PluginContext) -> Option<&'a PiiGuardrailConfig> {
    cfg.scopes(ctx.org_id(), ctx.team_id())
        .iter()
        .rev()
        .find_map(|scope| scope.guardrails.pii.as_ref())
}
use himadri_observability::Metrics;
use tokio::sync::RwLock;
use himadri_plugin::context::PluginContext;
use himadri_plugin::traits::{Plugin, PluginError, PluginType, RejectKind, Stage};

use crate::pii_engine::{PiiEngine, RedactOptions, RedactStrategy};

/// What happens when PII is found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PiiMode {
    /// Rewrite the offending spans and forward the request.
    Redact,
    /// Reject the request with 400 (entity types named, values never).
    Block,
    /// Forward unchanged; record detections in metrics/metadata only.
    Observe,
}

impl PiiMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "redact" => Some(Self::Redact),
            "block" => Some(Self::Block),
            "observe" => Some(Self::Observe),
            _ => None,
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::Redact => "redact",
            Self::Block => "block",
            Self::Observe => "observe",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PiiGuardrailSettings {
    pub mode: PiiMode,
    pub options: RedactOptions,
    /// Message roles scanned. Assistant history is excluded by default: it
    /// already round-tripped through a provider once (SPEC §13 Q1).
    pub apply_to: Vec<Role>,
    pub scan_tool_arguments: bool,
    /// Engine errors: `true` forwards unscanned (availability-first),
    /// `false` fails the request (default).
    pub fail_open: bool,
}

impl Default for PiiGuardrailSettings {
    fn default() -> Self {
        Self {
            mode: PiiMode::Redact,
            options: RedactOptions::default(),
            apply_to: vec![Role::User, Role::System, Role::Tool],
            scan_tool_arguments: false,
            fail_open: false,
        }
    }
}

fn parse_roles(names: &[String]) -> Vec<Role> {
    names
        .iter()
        .filter_map(|name| match name.to_ascii_lowercase().as_str() {
            "user" => Some(Role::User),
            "system" => Some(Role::System),
            "assistant" => Some(Role::Assistant),
            "tool" => Some(Role::Tool),
            other => {
                warn!("guardrails.pii.apply_to: unknown role '{other}' ignored");
                None
            }
        })
        .collect()
}

impl PiiGuardrailSettings {
    /// Runtime settings from a validated config section. The section's
    /// `enabled` flag is the *caller's* concern (resolution semantics live
    /// in [`PiiGuardrailPlugin::effective_settings`]).
    pub fn from_config(cfg: &PiiGuardrailConfig) -> Self {
        Self {
            mode: match cfg.mode {
                PiiModeConfig::Redact => PiiMode::Redact,
                PiiModeConfig::Block => PiiMode::Block,
                PiiModeConfig::Observe => PiiMode::Observe,
            },
            options: RedactOptions {
                strategy: match cfg.strategy {
                    PiiStrategyConfig::Replace => RedactStrategy::Replace,
                    PiiStrategyConfig::Mask => RedactStrategy::Mask,
                    PiiStrategyConfig::Hash => RedactStrategy::Hash,
                    PiiStrategyConfig::Encrypt => RedactStrategy::Encrypt,
                    PiiStrategyConfig::Remove => RedactStrategy::Remove,
                },
                entities: cfg.entities.as_ref().map(|list| {
                    list.iter()
                        .map(|e| e.trim().to_ascii_uppercase())
                        .filter(|e| !e.is_empty())
                        .collect()
                }),
                min_confidence: cfg.min_confidence,
            },
            apply_to: parse_roles(&cfg.apply_to),
            scan_tool_arguments: cfg.scan_tool_arguments,
            fail_open: cfg.fail_open,
        }
    }

    /// Env-driven settings; `None` unless `GUARDRAILS_PII_MODE` is set to a
    /// valid mode. These act as the global default when the config file's
    /// `guardrails.pii` section is absent/disabled.
    pub fn from_env() -> Option<Self> {
        let mode = PiiMode::parse(&std::env::var("GUARDRAILS_PII_MODE").ok()?)?;
        let mut settings = Self {
            mode,
            options: options_from_env(),
            fail_open: fail_open_from_env(),
            ..Default::default()
        };
        if let Ok(v) = std::env::var("GUARDRAILS_PII_SCAN_TOOL_ARGS") {
            settings.scan_tool_arguments = v.eq_ignore_ascii_case("true");
        }
        Some(settings)
    }
}

/// Scan options from the shared `GUARDRAILS_PII_*` env vars (used by both
/// the request plugin's and the response guardrail's env defaults).
fn options_from_env() -> RedactOptions {
    let mut options = RedactOptions::default();
    if let Ok(v) = std::env::var("GUARDRAILS_PII_STRATEGY") {
        match RedactStrategy::parse(&v) {
            Some(s) => options.strategy = s,
            None => warn!("GUARDRAILS_PII_STRATEGY '{}' not recognized; using replace", v),
        }
    }
    if let Ok(v) = std::env::var("GUARDRAILS_PII_ENTITIES") {
        let set: std::collections::HashSet<String> = himadri_core::env::split_csv(&v)
            .into_iter()
            .map(|e| e.to_ascii_uppercase())
            .collect();
        if !set.is_empty() {
            options.entities = Some(set);
        }
    }
    if let Some(v) = himadri_core::env::parse_var::<f32>("GUARDRAILS_PII_MIN_CONFIDENCE") {
        options.min_confidence = v;
    }
    options
}

fn fail_open_from_env() -> bool {
    himadri_core::env::flag_is_truthy("GUARDRAILS_PII_FAIL_OPEN")
}

/// Above this total scanned size the engine runs on the blocking pool.
/// Deployment-wide operational knob (env), not per-scope policy.
fn inline_limit_from_env() -> usize {
    himadri_core::env::parse_var("GUARDRAILS_INLINE_LIMIT_BYTES").unwrap_or(16 * 1024)
}

/// Where a scanned string lives in the request, for write-back.
#[derive(Debug, Clone, Copy)]
enum Slot {
    /// `messages[msg].content` — whole `Text`, or one `Parts` text part.
    Content { msg: usize, part: Option<usize> },
    /// `messages[msg].tool_calls[call].function.arguments`.
    ToolArg { msg: usize, call: usize },
}

pub struct PiiGuardrailPlugin {
    engine: Arc<dyn PiiEngine>,
    /// Global default settings (env-derived), used when the live config
    /// yields nothing for a request's scope.
    defaults: Option<PiiGuardrailSettings>,
    /// The gateway's live config (see `Gateway::config_handle`): org/team
    /// overrides and the config-file global section resolve against this,
    /// so admin reloads apply without re-wiring the plugin.
    config: Option<Arc<RwLock<Config>>>,
    metrics: Option<Arc<Metrics>>,
    inline_limit_bytes: usize,
}

impl PiiGuardrailPlugin {
    /// Fixed-settings construction (env-only wiring and tests). Requests
    /// are always scanned with `settings`; no config resolution.
    pub fn new(
        engine: Arc<dyn PiiEngine>,
        settings: PiiGuardrailSettings,
        metrics: Option<Arc<Metrics>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            engine,
            defaults: Some(settings),
            config: None,
            metrics,
            inline_limit_bytes: inline_limit_from_env(),
        })
    }

    /// Config-aware construction: per-request resolution is
    /// team > org > config-file global > `defaults` (env), where a present
    /// org/team `guardrails.pii` section replaces everything wholesale —
    /// including `enabled: false` to opt a scope out of a global policy.
    pub fn with_config(
        engine: Arc<dyn PiiEngine>,
        defaults: Option<PiiGuardrailSettings>,
        config: Arc<RwLock<Config>>,
        metrics: Option<Arc<Metrics>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            engine,
            defaults,
            config: Some(config),
            metrics,
            inline_limit_bytes: inline_limit_from_env(),
        })
    }

    /// Resolve the settings governing this request, or `None` when the
    /// guardrail is disabled for its scope.
    async fn effective_settings(&self, ctx: &PluginContext) -> Option<PiiGuardrailSettings> {
        if let Some(handle) = &self.config {
            let cfg = handle.read().await;
            if let Some(pii) = scoped_pii_section(&cfg, ctx) {
                // Wholesale override: an explicit section decides
                // entirely, including disabling a global policy.
                return pii.enabled.then(|| PiiGuardrailSettings::from_config(pii));
            }
            if cfg.guardrails.pii.enabled {
                return Some(PiiGuardrailSettings::from_config(&cfg.guardrails.pii));
            }
        }
        self.defaults.clone()
    }

    /// Collect the strings to scan and where to write them back.
    fn collect_slots(
        settings: &PiiGuardrailSettings,
        ctx: &PluginContext,
    ) -> (Vec<Slot>, Vec<String>) {
        let mut slots = Vec::new();
        let mut texts = Vec::new();
        for (i, message) in ctx.request.messages.iter().enumerate() {
            if !settings.apply_to.contains(&message.role) {
                continue;
            }
            match &message.content {
                Some(MessageContent::Text(s)) if !s.is_empty() => {
                    slots.push(Slot::Content { msg: i, part: None });
                    texts.push(s.clone());
                }
                Some(MessageContent::Parts(parts)) => {
                    for (j, part) in parts.iter().enumerate() {
                        if let ContentPart::Text { text } = part {
                            if !text.is_empty() {
                                slots.push(Slot::Content {
                                    msg: i,
                                    part: Some(j),
                                });
                                texts.push(text.clone());
                            }
                        }
                    }
                }
                _ => {}
            }
            if settings.scan_tool_arguments {
                for (j, call) in message.tool_calls.iter().flatten().enumerate() {
                    if !call.function.arguments.is_empty() {
                        slots.push(Slot::ToolArg { msg: i, call: j });
                        texts.push(call.function.arguments.clone());
                    }
                }
            }
        }
        (slots, texts)
    }

    fn write_back(ctx: &mut PluginContext, slot: Slot, new_text: String) {
        match slot {
            Slot::Content { msg, part: None } => {
                ctx.request.messages[msg].content = Some(MessageContent::Text(new_text));
            }
            Slot::Content {
                msg,
                part: Some(part),
            } => {
                if let Some(MessageContent::Parts(parts)) = &mut ctx.request.messages[msg].content {
                    if let Some(ContentPart::Text { text }) = parts.get_mut(part) {
                        *text = new_text;
                    }
                }
            }
            Slot::ToolArg { msg, call } => {
                if let Some(calls) = &mut ctx.request.messages[msg].tool_calls {
                    if let Some(tc) = calls.get_mut(call) {
                        tc.function.arguments = new_text;
                    }
                }
            }
        }
    }

    fn record(&self, entities: &BTreeMap<String, u32>, action: &'static str) {
        record_detections(self.metrics.as_deref(), entities, "request", action);
    }

    fn engine_error(
        &self,
        fail_open: bool,
        e: crate::pii_engine::PiiError,
    ) -> Result<(), PluginError> {
        if let Some(metrics) = &self.metrics {
            metrics.guardrail_engine_errors_total.inc();
        }
        if fail_open {
            warn!("PII guardrail engine failed; forwarding unscanned (fail_open): {e}");
            Ok(())
        } else {
            Err(PluginError::Internal(format!("PII guardrail failed: {e}")))
        }
    }
}

fn record_detections(
    metrics: Option<&Metrics>,
    entities: &BTreeMap<String, u32>,
    direction: &str,
    action: &str,
) {
    if let Some(metrics) = metrics {
        for (entity_type, count) in entities {
            metrics
                .guardrail_pii_detections_total
                .with_label_values(&[entity_type, direction, action])
                .inc_by(u64::from(*count));
        }
    }
}

/// Rewritten texts (`None` = unchanged; redact mode only) plus aggregate
/// entity counts.
type EngineOutput = (Vec<Option<String>>, BTreeMap<String, u32>);

/// Per-segment engine pass. Runs on the caller's thread — the caller
/// decides whether that thread is the async worker or the blocking pool.
fn run_engine(
    engine: &dyn PiiEngine,
    texts: &[String],
    opts: &RedactOptions,
    mode: PiiMode,
) -> Result<EngineOutput, crate::pii_engine::PiiError> {
    let mut rewritten = Vec::with_capacity(texts.len());
    let mut counts: BTreeMap<String, u32> = BTreeMap::new();
    for text in texts {
        match mode {
            PiiMode::Redact => {
                let out = engine.redact(text, opts)?;
                for (t, n) in &out.replaced {
                    *counts.entry(t.clone()).or_insert(0) += n;
                }
                rewritten.push(if out.replaced.is_empty() {
                    None
                } else {
                    Some(out.text)
                });
            }
            PiiMode::Block | PiiMode::Observe => {
                for hit in engine.scan(text, opts)? {
                    *counts.entry(hit.entity_type).or_insert(0) += 1;
                }
                rewritten.push(None);
            }
        }
    }
    Ok((rewritten, counts))
}

#[async_trait]
impl Plugin for PiiGuardrailPlugin {
    fn name(&self) -> &str {
        "pii-guardrail"
    }

    fn plugin_type(&self) -> PluginType {
        PluginType::Guardrail
    }

    fn stage(&self) -> Stage {
        Stage::BeforeRequest
    }

    async fn execute(&self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        let Some(settings) = self.effective_settings(ctx).await else {
            return Ok(());
        };

        let (slots, texts) = Self::collect_slots(&settings, ctx);
        if texts.is_empty() {
            return Ok(());
        }

        let timer = self
            .metrics
            .as_ref()
            .map(|m| m.guardrail_scan_duration.with_label_values(&["request"]).start_timer());

        let total_bytes: usize = texts.iter().map(String::len).sum();
        let mode = settings.mode;
        let result = if total_bytes > self.inline_limit_bytes {
            // Large scan: regex over big prompts is CPU-bound; keep it off
            // the async workers.
            let engine = self.engine.clone();
            let opts = settings.options.clone();
            tokio::task::spawn_blocking(move || run_engine(engine.as_ref(), &texts, &opts, mode))
                .await
                .map_err(|e| PluginError::Internal(format!("PII guardrail task failed: {e}")))?
        } else {
            run_engine(self.engine.as_ref(), &texts, &settings.options, mode)
        };
        drop(timer);

        let (rewritten, counts) = match result {
            Ok(v) => v,
            Err(e) => return self.engine_error(settings.fail_open, e),
        };

        if counts.is_empty() {
            return Ok(());
        }

        self.record(&counts, mode.as_str());
        ctx.set_metadata(
            "guardrails.pii".to_string(),
            serde_json::json!({ "action": mode.as_str(), "entities": counts }),
        );

        match mode {
            PiiMode::Redact => {
                for (slot, new_text) in slots.into_iter().zip(rewritten) {
                    if let Some(new_text) = new_text {
                        Self::write_back(ctx, slot, new_text);
                    }
                }
                Ok(())
            }
            PiiMode::Block => {
                if let Some(metrics) = &self.metrics {
                    metrics
                        .guardrail_blocked_total
                        .with_label_values(&["request"])
                        .inc();
                }
                // Entity *types* only — a value must never appear in an
                // error the client (or a log) sees.
                let types: Vec<&str> = counts.keys().map(String::as_str).collect();
                Err(PluginError::Rejected {
                    name: self.name().to_string(),
                    reason: format!("PII detected: {}", types.join(", ")),
                    kind: RejectKind::BadRequest,
                })
            }
            PiiMode::Observe => Ok(()),
        }
    }
}

// ─────────────────────────── Response side ───────────────────────────

/// What happens when PII is found in a model *response*.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PiiResponseMode {
    Off,
    Observe,
    Redact,
    Block,
}

impl PiiResponseMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "observe" => Some(Self::Observe),
            "redact" => Some(Self::Redact),
            "block" => Some(Self::Block),
            _ => None,
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Observe => "observe",
            Self::Redact => "redact",
            Self::Block => "block",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PiiResponseSettings {
    pub mode: PiiResponseMode,
    pub options: RedactOptions,
    pub fail_open: bool,
}

impl PiiResponseSettings {
    /// Response-side view of a config section: the `response_mode` field
    /// with the section's shared scan options. `enabled`/`Off` gating is
    /// the resolver's concern.
    pub fn from_config(cfg: &PiiGuardrailConfig) -> Self {
        let request_side = PiiGuardrailSettings::from_config(cfg);
        Self {
            mode: match cfg.response_mode {
                himadri_core::PiiResponseModeConfig::Off => PiiResponseMode::Off,
                himadri_core::PiiResponseModeConfig::Observe => PiiResponseMode::Observe,
                himadri_core::PiiResponseModeConfig::Redact => PiiResponseMode::Redact,
                himadri_core::PiiResponseModeConfig::Block => PiiResponseMode::Block,
            },
            options: request_side.options,
            fail_open: request_side.fail_open,
        }
    }

    /// Env-driven default; `None` unless `GUARDRAILS_PII_RESPONSE_MODE` is
    /// set to a valid, non-`off` mode. Scan options come from the shared
    /// `GUARDRAILS_PII_*` vars.
    pub fn from_env() -> Option<Self> {
        let mode = PiiResponseMode::parse(&std::env::var("GUARDRAILS_PII_RESPONSE_MODE").ok()?)?;
        if mode == PiiResponseMode::Off {
            return None;
        }
        Some(Self {
            mode,
            options: options_from_env(),
            fail_open: fail_open_from_env(),
        })
    }
}

/// Scans model output through the gateway's `ResponseGuardrail` hook.
///
/// Enforcement notes:
/// - Non-streaming: `redact` rewrites the response, `block` turns it into
///   a 400 — both before the client sees anything.
/// - Streaming: the gateway runs response guardrails on the buffered text
///   at stream end, *after* chunks were delivered; actions there are
///   post-hoc (logged/metered only). See `gateway/stream.rs`.
/// - Fail-closed is expressed as `Reject`, not `Err`: the gateway treats a
///   guardrail `Err` as log-and-allow, which would silently pass unscanned
///   content.
pub struct PiiResponseGuardrail {
    engine: Arc<dyn PiiEngine>,
    defaults: Option<PiiResponseSettings>,
    config: Option<Arc<RwLock<Config>>>,
    metrics: Option<Arc<Metrics>>,
    inline_limit_bytes: usize,
}

impl PiiResponseGuardrail {
    /// Fixed-settings construction (tests / env-only wiring).
    pub fn new(
        engine: Arc<dyn PiiEngine>,
        settings: PiiResponseSettings,
        metrics: Option<Arc<Metrics>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            engine,
            defaults: Some(settings),
            config: None,
            metrics,
            inline_limit_bytes: inline_limit_from_env(),
        })
    }

    /// Config-aware construction; resolution mirrors the request plugin
    /// (team > org > config-file global > env defaults, wholesale
    /// override semantics).
    pub fn with_config(
        engine: Arc<dyn PiiEngine>,
        defaults: Option<PiiResponseSettings>,
        config: Arc<RwLock<Config>>,
        metrics: Option<Arc<Metrics>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            engine,
            defaults,
            config: Some(config),
            metrics,
            inline_limit_bytes: inline_limit_from_env(),
        })
    }

    async fn effective_settings(&self, ctx: &PluginContext) -> Option<PiiResponseSettings> {
        fn active(cfg: &PiiGuardrailConfig) -> Option<PiiResponseSettings> {
            (cfg.enabled
                && cfg.response_mode != himadri_core::PiiResponseModeConfig::Off)
                .then(|| PiiResponseSettings::from_config(cfg))
        }

        if let Some(handle) = &self.config {
            let cfg = handle.read().await;
            if let Some(pii) = scoped_pii_section(&cfg, ctx) {
                // Wholesale override: a present section decides
                // entirely, including turning response scanning off.
                return active(pii);
            }
            if let Some(settings) = active(&cfg.guardrails.pii) {
                return Some(settings);
            }
        }
        self.defaults.clone()
    }
}

#[async_trait]
impl himadri_plugin::traits::ResponseGuardrail for PiiResponseGuardrail {
    fn name(&self) -> &str {
        "pii-response-guardrail"
    }

    async fn check_response(
        &self,
        ctx: &PluginContext,
        response: &str,
    ) -> Result<himadri_plugin::traits::ResponseAction, PluginError> {
        use himadri_plugin::traits::ResponseAction;

        let Some(settings) = self.effective_settings(ctx).await else {
            return Ok(ResponseAction::Allow);
        };
        if response.is_empty() || settings.mode == PiiResponseMode::Off {
            return Ok(ResponseAction::Allow);
        }

        let timer = self.metrics.as_ref().map(|m| {
            m.guardrail_scan_duration
                .with_label_values(&["response"])
                .start_timer()
        });

        let mode = settings.mode;
        let result = if response.len() > self.inline_limit_bytes {
            let engine = self.engine.clone();
            let opts = settings.options.clone();
            let text = response.to_string();
            let redact = mode == PiiResponseMode::Redact;
            tokio::task::spawn_blocking(move || {
                if redact {
                    engine.redact(&text, &opts).map(ScanOrRedact::Redacted)
                } else {
                    engine.scan(&text, &opts).map(ScanOrRedact::Hits)
                }
            })
            .await
            .map_err(|e| PluginError::Internal(format!("PII response guardrail task failed: {e}")))?
        } else if mode == PiiResponseMode::Redact {
            self.engine
                .redact(response, &settings.options)
                .map(ScanOrRedact::Redacted)
        } else {
            self.engine
                .scan(response, &settings.options)
                .map(ScanOrRedact::Hits)
        };
        drop(timer);

        let outcome = match result {
            Ok(v) => v,
            Err(e) => {
                if let Some(metrics) = &self.metrics {
                    metrics.guardrail_engine_errors_total.inc();
                }
                return if settings.fail_open {
                    warn!("PII response guardrail engine failed; allowing (fail_open): {e}");
                    Ok(ResponseAction::Allow)
                } else {
                    // Reject, not Err: the gateway logs-and-allows on Err.
                    Ok(ResponseAction::Reject(
                        "PII response guardrail failed; response withheld".to_string(),
                    ))
                };
            }
        };

        let counts: BTreeMap<String, u32> = match &outcome {
            ScanOrRedact::Redacted(out) => out.replaced.iter().cloned().collect(),
            ScanOrRedact::Hits(hits) => {
                let mut counts = BTreeMap::new();
                for hit in hits {
                    *counts.entry(hit.entity_type.clone()).or_insert(0) += 1;
                }
                counts
            }
        };
        if counts.is_empty() {
            return Ok(ResponseAction::Allow);
        }
        record_detections(self.metrics.as_deref(), &counts, "response", mode.as_str());

        match outcome {
            ScanOrRedact::Redacted(out) => Ok(ResponseAction::Redact(out.text)),
            ScanOrRedact::Hits(_) if mode == PiiResponseMode::Block => {
                if let Some(metrics) = &self.metrics {
                    metrics
                        .guardrail_blocked_total
                        .with_label_values(&["response"])
                        .inc();
                }
                // Entity *types* only — values never surface.
                let types: Vec<&str> = counts.keys().map(String::as_str).collect();
                Ok(ResponseAction::Reject(format!(
                    "PII detected in model output: {}",
                    types.join(", ")
                )))
            }
            ScanOrRedact::Hits(_) => Ok(ResponseAction::Allow), // observe
        }
    }
}

enum ScanOrRedact {
    Hits(Vec<crate::pii_engine::PiiEntityHit>),
    Redacted(crate::pii_engine::RedactOutcome),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pii_engine::{PiiEntityHit, PiiError, RedactOutcome};
    use himadri_core::{ChatCompletionRequest, Message};

    /// Deterministic engine: every occurrence of `SECRET` is one
    /// `TEST_ENTITY` hit; redaction rewrites it to `[X]`.
    struct FakeEngine {
        fail: bool,
    }

    impl PiiEngine for FakeEngine {
        fn scan(&self, text: &str, _opts: &RedactOptions) -> Result<Vec<PiiEntityHit>, PiiError> {
            if self.fail {
                return Err(PiiError::Engine("boom".into()));
            }
            Ok(text
                .match_indices("SECRET")
                .map(|(start, m)| PiiEntityHit {
                    entity_type: "TEST_ENTITY".to_string(),
                    start,
                    end: start + m.len(),
                    confidence: 0.9,
                })
                .collect())
        }

        fn redact(&self, text: &str, opts: &RedactOptions) -> Result<RedactOutcome, PiiError> {
            let hits = self.scan(text, opts)?;
            Ok(RedactOutcome {
                text: text.replace("SECRET", "[X]"),
                replaced: if hits.is_empty() {
                    vec![]
                } else {
                    vec![("TEST_ENTITY".to_string(), hits.len() as u32)]
                },
            })
        }
    }

    fn plugin(mode: PiiMode, fail: bool) -> Arc<PiiGuardrailPlugin> {
        PiiGuardrailPlugin::new(
            Arc::new(FakeEngine { fail }),
            PiiGuardrailSettings {
                mode,
                ..Default::default()
            },
            None,
        )
    }

    fn ctx_with_user_text(text: &str) -> PluginContext {
        let request = ChatCompletionRequest {
            model: "test-model".to_string(),
            messages: vec![Message::user(text)],
            ..Default::default()
        };
        PluginContext::from_request(&request, None)
    }

    #[tokio::test]
    async fn redact_mode_rewrites_content() {
        let mut ctx = ctx_with_user_text("the SECRET plan");
        plugin(PiiMode::Redact, false).execute(&mut ctx).await.unwrap();
        assert_eq!(
            ctx.request.messages[0].content.as_ref().unwrap().flat_text(),
            "the [X] plan"
        );
        let meta = ctx.get_metadata("guardrails.pii").unwrap();
        assert_eq!(meta["action"], "redact");
        assert_eq!(meta["entities"]["TEST_ENTITY"], 1);
    }

    #[tokio::test]
    async fn block_mode_rejects_with_types_only() {
        let mut ctx = ctx_with_user_text("the SECRET plan");
        let err = plugin(PiiMode::Block, false)
            .execute(&mut ctx)
            .await
            .unwrap_err();
        match err {
            PluginError::Rejected { reason, kind, .. } => {
                assert_eq!(kind, RejectKind::BadRequest);
                assert!(reason.contains("TEST_ENTITY"));
                assert!(!reason.contains("SECRET"));
            }
            other => panic!("expected rejection, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn observe_mode_leaves_content_untouched() {
        let mut ctx = ctx_with_user_text("the SECRET plan");
        plugin(PiiMode::Observe, false).execute(&mut ctx).await.unwrap();
        assert_eq!(
            ctx.request.messages[0].content.as_ref().unwrap().flat_text(),
            "the SECRET plan"
        );
        assert!(ctx.get_metadata("guardrails.pii").is_some());
    }

    #[tokio::test]
    async fn clean_request_records_nothing() {
        let mut ctx = ctx_with_user_text("nothing to see");
        plugin(PiiMode::Redact, false).execute(&mut ctx).await.unwrap();
        assert!(ctx.get_metadata("guardrails.pii").is_none());
    }

    #[tokio::test]
    async fn assistant_messages_are_skipped_by_default() {
        let request = ChatCompletionRequest {
            model: "test-model".to_string(),
            messages: vec![Message {
                role: Role::Assistant,
                content: Some(MessageContent::text("the SECRET plan")),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut ctx = PluginContext::from_request(&request, None);
        plugin(PiiMode::Redact, false).execute(&mut ctx).await.unwrap();
        assert_eq!(
            ctx.request.messages[0].content.as_ref().unwrap().flat_text(),
            "the SECRET plan"
        );
    }

    #[tokio::test]
    async fn parts_content_is_redacted_per_text_part() {
        let request = ChatCompletionRequest {
            model: "test-model".to_string(),
            messages: vec![Message {
                role: Role::User,
                content: Some(MessageContent::Parts(vec![
                    ContentPart::Text {
                        text: "keep this".to_string(),
                    },
                    ContentPart::Text {
                        text: "a SECRET here".to_string(),
                    },
                ])),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut ctx = PluginContext::from_request(&request, None);
        plugin(PiiMode::Redact, false).execute(&mut ctx).await.unwrap();
        assert_eq!(
            ctx.request.messages[0].content.as_ref().unwrap().flat_text(),
            "keep thisa [X] here"
        );
    }

    #[tokio::test]
    async fn engine_failure_fails_closed_by_default() {
        let mut ctx = ctx_with_user_text("anything");
        let err = plugin(PiiMode::Redact, true)
            .execute(&mut ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, PluginError::Internal(_)));
    }

    #[tokio::test]
    async fn engine_failure_forwards_when_fail_open() {
        let mut ctx = ctx_with_user_text("anything");
        let plugin = PiiGuardrailPlugin::new(
            Arc::new(FakeEngine { fail: true }),
            PiiGuardrailSettings {
                fail_open: true,
                ..Default::default()
            },
            None,
        );
        plugin.execute(&mut ctx).await.unwrap();
    }

    // ── Config-driven resolution (team > org > global > env defaults) ──

    use himadri_core::{
        AuthContext, AuthScope, OrgConfig, PiiGuardrailConfig, PiiModeConfig, TeamConfig,
    };

    fn pii_section(enabled: bool, mode: PiiModeConfig) -> PiiGuardrailConfig {
        PiiGuardrailConfig {
            enabled,
            mode,
            ..Default::default()
        }
    }

    fn org_with_pii(pii: Option<PiiGuardrailConfig>, team_pii: Option<PiiGuardrailConfig>) -> OrgConfig {
        let mut org = OrgConfig::default();
        org.guardrails.pii = pii;
        if let Some(team_pii) = team_pii {
            let mut team = TeamConfig::default();
            team.guardrails.pii = Some(team_pii);
            org.teams.insert("team-a".to_string(), team);
        }
        org
    }

    fn config_plugin(
        config: Config,
        env_defaults: Option<PiiGuardrailSettings>,
    ) -> Arc<PiiGuardrailPlugin> {
        PiiGuardrailPlugin::with_config(
            Arc::new(FakeEngine { fail: false }),
            env_defaults,
            Arc::new(RwLock::new(config)),
            None,
        )
    }

    fn ctx_for_scope(org: Option<&str>, team: Option<&str>, text: &str) -> PluginContext {
        let request = ChatCompletionRequest {
            model: "test-model".to_string(),
            messages: vec![Message::user(text)],
            ..Default::default()
        };
        let auth = AuthContext {
            org_id: org.map(String::from),
            team_id: team.map(String::from),
            scope: AuthScope::ApiKey,
            ..Default::default()
        };
        PluginContext::from_request(&request, Some(&auth))
    }

    fn text_of(ctx: &PluginContext) -> String {
        ctx.request.messages[0]
            .content
            .as_ref()
            .unwrap()
            .flat_text()
            .into_owned()
    }

    #[tokio::test]
    async fn config_global_section_applies_when_enabled() {
        let mut config = Config::default();
        config.guardrails.pii = pii_section(true, PiiModeConfig::Redact);
        let plugin = config_plugin(config, None);

        let mut ctx = ctx_for_scope(None, None, "the SECRET plan");
        plugin.execute(&mut ctx).await.unwrap();
        assert_eq!(text_of(&ctx), "the [X] plan");
    }

    #[tokio::test]
    async fn disabled_everywhere_means_no_scan() {
        let plugin = config_plugin(Config::default(), None);
        let mut ctx = ctx_for_scope(None, None, "the SECRET plan");
        plugin.execute(&mut ctx).await.unwrap();
        assert_eq!(text_of(&ctx), "the SECRET plan");
    }

    #[tokio::test]
    async fn env_defaults_apply_when_config_global_disabled() {
        let plugin = config_plugin(Config::default(), Some(PiiGuardrailSettings::default()));
        let mut ctx = ctx_for_scope(None, None, "the SECRET plan");
        plugin.execute(&mut ctx).await.unwrap();
        assert_eq!(text_of(&ctx), "the [X] plan");
    }

    #[tokio::test]
    async fn org_override_beats_global() {
        let mut config = Config::default();
        config.guardrails.pii = pii_section(true, PiiModeConfig::Redact);
        config.orgs.insert(
            "acme".to_string(),
            org_with_pii(Some(pii_section(true, PiiModeConfig::Block)), None),
        );
        let plugin = config_plugin(config, None);

        // The org's block mode wins over the global redact mode.
        let mut ctx = ctx_for_scope(Some("acme"), None, "the SECRET plan");
        let err = plugin.execute(&mut ctx).await.unwrap_err();
        assert!(matches!(err, PluginError::Rejected { .. }));

        // Other orgs still get the global redact policy.
        let mut ctx = ctx_for_scope(Some("other"), None, "the SECRET plan");
        plugin.execute(&mut ctx).await.unwrap();
        assert_eq!(text_of(&ctx), "the [X] plan");
    }

    #[tokio::test]
    async fn disabled_org_override_opts_out_of_global_policy() {
        let mut config = Config::default();
        config.guardrails.pii = pii_section(true, PiiModeConfig::Redact);
        config.orgs.insert(
            "acme".to_string(),
            org_with_pii(Some(pii_section(false, PiiModeConfig::Redact)), None),
        );
        let plugin = config_plugin(config, Some(PiiGuardrailSettings::default()));

        // Wholesale replace: enabled=false disables scanning for the org
        // even though global config *and* env defaults are active.
        let mut ctx = ctx_for_scope(Some("acme"), None, "the SECRET plan");
        plugin.execute(&mut ctx).await.unwrap();
        assert_eq!(text_of(&ctx), "the SECRET plan");
    }

    #[tokio::test]
    async fn team_override_beats_org_override() {
        let mut config = Config::default();
        config.orgs.insert(
            "acme".to_string(),
            org_with_pii(
                Some(pii_section(true, PiiModeConfig::Block)),
                Some(pii_section(true, PiiModeConfig::Redact)),
            ),
        );
        let plugin = config_plugin(config, None);

        // team-a gets redact; the org's block applies to other teams.
        let mut ctx = ctx_for_scope(Some("acme"), Some("team-a"), "the SECRET plan");
        plugin.execute(&mut ctx).await.unwrap();
        assert_eq!(text_of(&ctx), "the [X] plan");

        let mut ctx = ctx_for_scope(Some("acme"), Some("team-b"), "the SECRET plan");
        let err = plugin.execute(&mut ctx).await.unwrap_err();
        assert!(matches!(err, PluginError::Rejected { .. }));
    }

    #[tokio::test]
    async fn live_config_change_applies_without_rewiring() {
        let handle = Arc::new(RwLock::new(Config::default()));
        let plugin = PiiGuardrailPlugin::with_config(
            Arc::new(FakeEngine { fail: false }),
            None,
            handle.clone(),
            None,
        );

        let mut ctx = ctx_for_scope(None, None, "the SECRET plan");
        plugin.execute(&mut ctx).await.unwrap();
        assert_eq!(text_of(&ctx), "the SECRET plan");

        // Simulate an admin reload enabling the global policy.
        handle.write().await.guardrails.pii = pii_section(true, PiiModeConfig::Redact);

        let mut ctx = ctx_for_scope(None, None, "the SECRET plan");
        plugin.execute(&mut ctx).await.unwrap();
        assert_eq!(text_of(&ctx), "the [X] plan");
    }

    #[test]
    fn from_config_maps_fields() {
        let cfg = PiiGuardrailConfig {
            enabled: true,
            mode: PiiModeConfig::Observe,
            strategy: himadri_core::PiiStrategyConfig::Mask,
            entities: Some(vec!["email_address".to_string(), " us_ssn ".to_string()]),
            min_confidence: 0.8,
            apply_to: vec!["user".to_string(), "bogus".to_string()],
            scan_tool_arguments: true,
            fail_open: true,
            ..Default::default()
        };
        let settings = PiiGuardrailSettings::from_config(&cfg);
        assert_eq!(settings.mode, PiiMode::Observe);
        assert_eq!(settings.options.strategy, RedactStrategy::Mask);
        let entities = settings.options.entities.unwrap();
        assert!(entities.contains("EMAIL_ADDRESS") && entities.contains("US_SSN"));
        assert_eq!(settings.options.min_confidence, 0.8);
        assert_eq!(settings.apply_to, vec![Role::User]); // unknown role dropped
        assert!(settings.scan_tool_arguments);
        assert!(settings.fail_open);
    }

    // ── Response guardrail ──

    use himadri_core::PiiResponseModeConfig;
    use himadri_plugin::traits::{ResponseAction, ResponseGuardrail};

    fn response_guardrail(mode: PiiResponseMode, fail: bool) -> Arc<PiiResponseGuardrail> {
        PiiResponseGuardrail::new(
            Arc::new(FakeEngine { fail }),
            PiiResponseSettings {
                mode,
                options: RedactOptions::default(),
                fail_open: false,
            },
            None,
        )
    }

    #[tokio::test]
    async fn response_redact_mode_rewrites_output() {
        let ctx = ctx_with_user_text("hi");
        let action = response_guardrail(PiiResponseMode::Redact, false)
            .check_response(&ctx, "the SECRET plan")
            .await
            .unwrap();
        assert_eq!(action, ResponseAction::Redact("the [X] plan".to_string()));
    }

    #[tokio::test]
    async fn response_block_mode_rejects_with_types_only() {
        let ctx = ctx_with_user_text("hi");
        let action = response_guardrail(PiiResponseMode::Block, false)
            .check_response(&ctx, "the SECRET plan")
            .await
            .unwrap();
        match action {
            ResponseAction::Reject(reason) => {
                assert!(reason.contains("TEST_ENTITY"), "{reason}");
                assert!(!reason.contains("SECRET"), "reason leaks value: {reason}");
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn response_observe_and_clean_output_allow() {
        let ctx = ctx_with_user_text("hi");
        let guardrail = response_guardrail(PiiResponseMode::Observe, false);
        assert_eq!(
            guardrail.check_response(&ctx, "the SECRET plan").await.unwrap(),
            ResponseAction::Allow
        );
        let guardrail = response_guardrail(PiiResponseMode::Redact, false);
        assert_eq!(
            guardrail.check_response(&ctx, "all clean here").await.unwrap(),
            ResponseAction::Allow
        );
    }

    #[tokio::test]
    async fn response_engine_failure_fails_closed_as_reject() {
        let ctx = ctx_with_user_text("hi");
        let action = response_guardrail(PiiResponseMode::Redact, true)
            .check_response(&ctx, "anything")
            .await
            .unwrap();
        assert!(matches!(action, ResponseAction::Reject(_)), "{action:?}");
    }

    #[tokio::test]
    async fn response_engine_failure_allows_when_fail_open() {
        let ctx = ctx_with_user_text("hi");
        let guardrail = PiiResponseGuardrail::new(
            Arc::new(FakeEngine { fail: true }),
            PiiResponseSettings {
                mode: PiiResponseMode::Redact,
                options: RedactOptions::default(),
                fail_open: true,
            },
            None,
        );
        assert_eq!(
            guardrail.check_response(&ctx, "anything").await.unwrap(),
            ResponseAction::Allow
        );
    }

    #[tokio::test]
    async fn response_mode_resolves_from_config_per_org() {
        let mut config = Config::default();
        // Global: response scanning off (request-side redact only).
        config.guardrails.pii = pii_section(true, PiiModeConfig::Redact);
        // Org override: response redaction on.
        let mut org_pii = pii_section(true, PiiModeConfig::Redact);
        org_pii.response_mode = PiiResponseModeConfig::Redact;
        config
            .orgs
            .insert("acme".to_string(), org_with_pii(Some(org_pii), None));

        let guardrail = PiiResponseGuardrail::with_config(
            Arc::new(FakeEngine { fail: false }),
            None,
            Arc::new(RwLock::new(config)),
            None,
        );

        // acme's responses are redacted.
        let ctx = ctx_for_scope(Some("acme"), None, "hi");
        assert_eq!(
            guardrail.check_response(&ctx, "the SECRET plan").await.unwrap(),
            ResponseAction::Redact("the [X] plan".to_string())
        );

        // Other orgs: global has response_mode off — allowed through.
        let ctx = ctx_for_scope(Some("globex"), None, "hi");
        assert_eq!(
            guardrail.check_response(&ctx, "the SECRET plan").await.unwrap(),
            ResponseAction::Allow
        );
    }

    #[tokio::test]
    async fn large_input_takes_spawn_blocking_path() {
        let big = format!("{} SECRET", "x".repeat(32 * 1024));
        let mut ctx = ctx_with_user_text(&big);
        plugin(PiiMode::Redact, false).execute(&mut ctx).await.unwrap();
        assert!(ctx
            .request
            .messages[0]
            .content
            .as_ref()
            .unwrap()
            .flat_text()
            .ends_with("[X]"));
    }
}
