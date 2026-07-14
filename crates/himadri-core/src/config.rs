use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

use crate::error::ConfigError;
use crate::types::Target;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub strategy: StrategyConfig,

    #[serde(default)]
    pub targets: Vec<Target>,

    #[serde(default)]
    pub observability: ObservabilityConfig,

    #[serde(default)]
    pub rate_limit: RateLimitConfig,

    #[serde(default)]
    pub admin: AdminConfig,

    #[serde(default)]
    pub orgs: HashMap<String, OrgConfig>,

    #[serde(default)]
    pub cors: CorsConfig,

    #[serde(default)]
    pub rbac: RbacConfig,

    /// Global guardrail defaults; org/team `guardrails.pii` sections
    /// override them wholesale (docs/SPEC_GUARDRAILS.md §6.6).
    #[serde(default)]
    pub guardrails: GuardrailsConfig,
}

/// Content-safety guardrails applied on the `/v1` request path.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GuardrailsConfig {
    #[serde(default)]
    pub pii: PiiGuardrailConfig,
}

/// What happens when PII is detected.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum PiiModeConfig {
    /// Rewrite the offending spans and forward the request (default).
    #[default]
    Redact,
    /// Reject the request with 400.
    Block,
    /// Forward unchanged; record detections only.
    Observe,
}

/// How detected spans are rewritten in `redact` mode.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum PiiStrategyConfig {
    /// `[EMAIL_ADDRESS]`-style placeholder (default).
    #[default]
    Replace,
    /// Partial masking, e.g. `jo**@****le.com`.
    Mask,
    /// Salted irreversible hash suffix.
    Hash,
    /// Reversible token (requires `GUARDRAILS_ENCRYPTION_KEY`).
    Encrypt,
    /// Remove the matched text entirely.
    Remove,
}

/// Response-side scanning mode. Non-streaming responses are redacted or
/// blocked before the client sees them; for streams the check runs on the
/// buffered text at stream end only (post-hoc — chunks already sent).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum PiiResponseModeConfig {
    #[default]
    Off,
    Observe,
    Redact,
    Block,
}

/// PII detection/redaction settings. Lives at the top level of `Config`
/// (global default) and, optionally, on org/team `guardrails.pii` where a
/// present section replaces the global settings wholesale — field-level
/// merging is deliberately avoided to keep resolution predictable.
///
/// Secrets (hash salt, encryption key) are env-only
/// (`GUARDRAILS_HASH_SALT`, `GUARDRAILS_ENCRYPTION_KEY`) and must never be
/// added here: `Config` is served verbatim by `GET /admin/config`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PiiGuardrailConfig {
    pub enabled: bool,

    pub mode: PiiModeConfig,

    pub strategy: PiiStrategyConfig,

    /// Entity types to act on (e.g. `EMAIL_ADDRESS`, `US_SSN`).
    /// `None` = every type the engine detects.
    pub entities: Option<Vec<String>>,

    /// Detections below this confidence are ignored.
    pub min_confidence: f32,

    /// Message roles scanned. Assistant history is excluded by default:
    /// it already round-tripped through a provider once.
    pub apply_to: Vec<String>,

    pub scan_tool_arguments: bool,

    /// Engine errors: `true` forwards unscanned (availability-first),
    /// `false` fails the request (default).
    pub fail_open: bool,

    pub response_mode: PiiResponseModeConfig,
}

impl Default for PiiGuardrailConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: PiiModeConfig::default(),
            strategy: PiiStrategyConfig::default(),
            entities: None,
            min_confidence: 0.6,
            apply_to: vec!["user".into(), "system".into(), "tool".into()],
            scan_tool_arguments: false,
            fail_open: false,
            response_mode: PiiResponseModeConfig::default(),
        }
    }
}

/// Role-based access control policy. Maps roles (e.g. Zitadel project roles
/// carried in the JWT) to the models and providers a principal may use, enabling
/// tiered/differentiated access on the `/v1` endpoints.
///
/// Semantics:
/// - When `enabled` is false, RBAC is a no-op (all access allowed).
/// - A principal with `AuthScope::Admin` bypasses RBAC entirely.
/// - The effective policy is the **union** across all of a principal's roles
///   that appear in `roles` (most-permissive wins).
/// - If none of a principal's roles match and `default_role` is set, that role's
///   policy applies; if `default_role` is unset and no role matches, access is
///   **denied**.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RbacConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default)]
    pub roles: HashMap<String, RolePolicy>,

    /// Fallback role applied to authenticated principals whose roles don't match
    /// any entry in `roles` (e.g. API-key principals, or users without a tier).
    #[serde(default)]
    pub default_role: Option<String>,
}

/// Access policy for a single role.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RolePolicy {
    /// Allowed model patterns (supports `*` wildcards, e.g. `claude-*`, `*`).
    /// `None` means no model restriction for this role (all models allowed).
    #[serde(default)]
    pub models: Option<Vec<String>>,

    /// Allowed provider names (exact match, supports `*`). `None` means no
    /// provider restriction for this role (all providers allowed).
    #[serde(default)]
    pub providers: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CorsConfig {
    pub enabled: bool,
    pub allowed_origins: Vec<String>,
    pub allowed_methods: Vec<String>,
    pub allowed_headers: Vec<String>,
}

impl Default for CorsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            allowed_origins: Vec::new(),
            allowed_methods: default_cors_methods(),
            allowed_headers: default_cors_headers(),
        }
    }
}

fn default_cors_methods() -> Vec<String> {
    vec![
        "GET".into(),
        "POST".into(),
        "PUT".into(),
        "DELETE".into(),
        "OPTIONS".into(),
    ]
}

fn default_cors_headers() -> Vec<String> {
    vec!["Authorization".into(), "Content-Type".into()]
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OrgConfig {
    #[serde(default)]
    pub name: Option<String>,

    #[serde(default)]
    pub enabled: bool,

    #[serde(default)]
    pub allowed_models: Option<Vec<String>>,

    #[serde(default)]
    pub blocked_models: Option<Vec<String>>,

    #[serde(default)]
    pub rate_limit: Option<RateLimitConfig>,

    #[serde(default)]
    pub token_budget: Option<OrgTokenBudget>,

    #[serde(default)]
    pub guardrails: OrgGuardrailConfig,

    #[serde(default)]
    pub teams: HashMap<String, TeamConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TeamConfig {
    #[serde(default)]
    pub name: Option<String>,

    #[serde(default)]
    pub enabled: bool,

    #[serde(default)]
    pub allowed_models: Option<Vec<String>>,

    #[serde(default)]
    pub blocked_models: Option<Vec<String>>,

    #[serde(default)]
    pub rate_limit: Option<RateLimitConfig>,

    #[serde(default)]
    pub token_budget: Option<OrgTokenBudget>,

    #[serde(default)]
    pub guardrails: OrgGuardrailConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OrgTokenBudget {
    #[serde(default)]
    pub max_tokens_per_request: Option<u32>,

    #[serde(default)]
    pub max_tokens_per_day: Option<u64>,

    #[serde(default)]
    pub max_tokens_per_month: Option<u64>,

    #[serde(default)]
    pub cost_limit_per_day: Option<f64>,

    #[serde(default)]
    pub cost_limit_per_month: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OrgGuardrailConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default)]
    pub blocked_words: Vec<String>,

    #[serde(default)]
    pub max_tokens_per_request: Option<u32>,

    #[serde(default)]
    pub content_filter: Option<ContentFilterConfig>,

    /// Org/team-scope PII guardrail override. When present it replaces the
    /// global `Config.guardrails.pii` settings wholesale for this scope
    /// (including `enabled: false` to opt out of a global policy).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pii: Option<PiiGuardrailConfig>,

    #[serde(default)]
    pub audit: AuditConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ContentFilterConfig {
    #[serde(default)]
    pub enabled: bool,

    /// Deprecated: use `guardrails.pii` (with `mode: block`) instead.
    /// Config load maps this to an equivalent `pii` section when no
    /// explicit one is present; the field will be removed.
    #[serde(default)]
    pub block_pii: bool,

    #[serde(default)]
    pub block_toxicity: bool,

    #[serde(default)]
    pub custom_patterns: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AuditConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default)]
    pub log_requests: bool,

    #[serde(default)]
    pub log_responses: bool,

    #[serde(default)]
    pub redact_pii: bool,

    #[serde(default)]
    pub retention_days: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StrategyConfig {
    pub mode: StrategyMode,

    pub fallback_timeout_ms: u64,

    /// Rules for the `conditional` strategy (matched in order).
    pub conditional_rules: Vec<ConditionalRuleConfig>,

    /// Rules for the `content_based` strategy (matched in order).
    pub content_rules: Vec<ContentRuleConfig>,

    /// Variants for the `ab_test` strategy.
    pub ab_variants: Vec<ABVariantConfig>,

    /// Fallback target for `conditional` / `content_based` when no rule matches.
    pub strategy_fallback: Option<Target>,
}

impl Default for StrategyConfig {
    fn default() -> Self {
        Self {
            mode: default_strategy_mode(),
            fallback_timeout_ms: 30000,
            conditional_rules: Vec::new(),
            content_rules: Vec::new(),
            ab_variants: Vec::new(),
            strategy_fallback: None,
        }
    }
}

fn default_strategy_mode() -> StrategyMode {
    StrategyMode::Single
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StrategyMode {
    Single,
    Fallback,
    LoadBalance,
    LeastLatency,
    CostOptimized,
    Conditional,
    #[serde(rename = "content_based")]
    ContentBased,
    #[serde(rename = "ab_test")]
    ABTest,
}

/// Key a `conditional` rule matches against.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConditionKeyConfig {
    Model,
    ModelPrefix,
}

/// A single `conditional` strategy rule: if `key` matches `value`, route to
/// `target`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConditionalRuleConfig {
    pub key: ConditionKeyConfig,
    pub value: String,
    pub target: Target,
}

/// Match type for a `content_based` rule.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContentConditionTypeConfig {
    PromptContains,
    PromptNotContains,
    PromptRegex,
}

/// A single `content_based` strategy rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentRuleConfig {
    pub condition_type: ContentConditionTypeConfig,
    pub value: String,
    pub target: Target,
}

/// A single `ab_test` variant. A non-positive `weight` is treated as 1.0.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ABVariantConfig {
    pub target: Target,
    #[serde(default)]
    pub weight: f64,
    #[serde(default)]
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ObservabilityConfig {
    #[serde(default)]
    pub tracing: TracingConfig,

    #[serde(default)]
    pub metrics: MetricsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TracingConfig {
    pub enabled: bool,
    pub service_name: String,
    pub endpoint: Option<String>,
    pub sample_ratio: f64,
}

impl Default for TracingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            service_name: default_service_name(),
            endpoint: None,
            sample_ratio: default_sample_ratio(),
        }
    }
}

fn default_service_name() -> String {
    "himadri".to_string()
}

fn default_sample_ratio() -> f64 {
    1.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MetricsConfig {
    pub enabled: bool,
    pub path: String,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path: default_metrics_path(),
        }
    }
}

fn default_metrics_path() -> String {
    "/metrics".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RateLimitConfig {
    pub enabled: bool,
    pub requests_per_second: u64,
    pub burst_size: u64,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            requests_per_second: default_rate_limit_rps(),
            burst_size: default_rate_limit_burst(),
        }
    }
}

fn default_rate_limit_rps() -> u64 {
    100
}

fn default_rate_limit_burst() -> u64 {
    200
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AdminConfig {
    /// Master admin key. Never serialized: `Config` is returned verbatim by
    /// `GET /admin/config` and `/admin/config/history`, and the master key
    /// must not be readable by admin-scoped principals (e.g. JWT-role
    /// admins) or flow into config exports (CWE-522).
    #[serde(skip_serializing)]
    pub master_key: Option<String>,
}

impl Config {
    pub fn load_from_env() -> Result<Self, ConfigError> {
        if let Ok(path) = std::env::var("GATEWAY_CONFIG") {
            return Self::load_from_file(&path);
        }

        Ok(Self::default())
    }

    pub fn load_from_file(path: &str) -> Result<Self, ConfigError> {
        let content = std::fs::read_to_string(path)?;
        let ext = Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("json");

        let mut config = match ext {
            "json" => serde_json::from_str::<Config>(&content)
                .map_err(|e| ConfigError::Parse(e.to_string()))?,
            _ => {
                return Err(ConfigError::InvalidValue {
                    field: "config_file".to_string(),
                    reason: format!("unsupported extension: {}", ext),
                })
            }
        };
        config.apply_guardrail_deprecations();

        Ok(config)
    }

    /// Map the deprecated `content_filter.block_pii` flag to an equivalent
    /// `guardrails.pii` section (block mode) wherever no explicit `pii`
    /// section is present. Called on config load and on every admin
    /// reload/rollback so old configs keep their (intended) semantics.
    pub fn apply_guardrail_deprecations(&mut self) {
        fn shim(scope: &str, guardrails: &mut OrgGuardrailConfig) {
            let block_pii = guardrails.enabled
                && guardrails
                    .content_filter
                    .as_ref()
                    .is_some_and(|cf| cf.enabled && cf.block_pii);
            if block_pii && guardrails.pii.is_none() {
                tracing::warn!(
                    "content_filter.block_pii ({scope}) is deprecated; \
                     mapping to guardrails.pii {{ enabled: true, mode: block }} — \
                     migrate the config to the pii section"
                );
                guardrails.pii = Some(PiiGuardrailConfig {
                    enabled: true,
                    mode: PiiModeConfig::Block,
                    ..Default::default()
                });
            }
        }

        for (org_id, org) in &mut self.orgs {
            shim(&format!("org '{org_id}'"), &mut org.guardrails);
            for (team_id, team) in &mut org.teams {
                shim(
                    &format!("org '{org_id}' team '{team_id}'"),
                    &mut team.guardrails,
                );
            }
        }
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.targets.is_empty() {
            return Err(ConfigError::MissingField("targets".to_string()));
        }

        for target in &self.targets {
            if target.provider.is_empty() {
                return Err(ConfigError::MissingField("target.provider".to_string()));
            }
        }

        Ok(())
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            strategy: StrategyConfig::default(),
            targets: vec![Target {
                id: None,
                provider: "openai".to_string(),
                weight: 1.0,
                models: None,
                api_key_env: Some("OPENAI_API_KEY".to_string()),
                base_url: None,
            }],
            observability: ObservabilityConfig::default(),
            rate_limit: RateLimitConfig::default(),
            admin: AdminConfig::default(),
            orgs: HashMap::new(),
            cors: CorsConfig::default(),
            rbac: RbacConfig::default(),
            guardrails: GuardrailsConfig::default(),
        }
    }
}

#[cfg(test)]
mod guardrail_config_tests {
    use super::*;

    #[test]
    fn pii_config_serde_round_trip() {
        let json = r#"{
            "enabled": true,
            "mode": "block",
            "strategy": "mask",
            "entities": ["EMAIL_ADDRESS", "US_SSN"],
            "min_confidence": 0.8,
            "apply_to": ["user"],
            "scan_tool_arguments": true,
            "fail_open": true,
            "response_mode": "observe"
        }"#;
        let parsed: PiiGuardrailConfig = serde_json::from_str(json).unwrap();
        assert!(parsed.enabled);
        assert_eq!(parsed.mode, PiiModeConfig::Block);
        assert_eq!(parsed.strategy, PiiStrategyConfig::Mask);
        assert_eq!(parsed.min_confidence, 0.8);
        assert_eq!(parsed.apply_to, vec!["user"]);
        assert_eq!(parsed.response_mode, PiiResponseModeConfig::Observe);

        let round: PiiGuardrailConfig =
            serde_json::from_str(&serde_json::to_string(&parsed).unwrap()).unwrap();
        assert_eq!(round, parsed);
    }

    #[test]
    fn pii_config_defaults_are_disabled_redact_replace() {
        let parsed: PiiGuardrailConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed, PiiGuardrailConfig::default());
        assert!(!parsed.enabled);
        assert_eq!(parsed.mode, PiiModeConfig::Redact);
        assert_eq!(parsed.strategy, PiiStrategyConfig::Replace);
        assert_eq!(parsed.apply_to, vec!["user", "system", "tool"]);
    }

    #[test]
    fn config_without_guardrails_section_still_parses() {
        let config: Config = serde_json::from_str(r#"{"targets": []}"#).unwrap();
        assert!(!config.guardrails.pii.enabled);
    }

    #[test]
    fn block_pii_deprecation_maps_to_pii_block_mode() {
        let mut config = Config::default();
        let mut org = OrgConfig::default();
        org.guardrails.enabled = true;
        org.guardrails.content_filter = Some(ContentFilterConfig {
            enabled: true,
            block_pii: true,
            ..Default::default()
        });
        config.orgs.insert("acme".to_string(), org);

        config.apply_guardrail_deprecations();

        let pii = config.orgs["acme"].guardrails.pii.as_ref().unwrap();
        assert!(pii.enabled);
        assert_eq!(pii.mode, PiiModeConfig::Block);
    }

    #[test]
    fn block_pii_shim_does_not_override_explicit_pii_section() {
        let mut config = Config::default();
        let mut org = OrgConfig::default();
        org.guardrails.enabled = true;
        org.guardrails.content_filter = Some(ContentFilterConfig {
            enabled: true,
            block_pii: true,
            ..Default::default()
        });
        org.guardrails.pii = Some(PiiGuardrailConfig {
            enabled: true,
            mode: PiiModeConfig::Observe,
            ..Default::default()
        });
        config.orgs.insert("acme".to_string(), org);

        config.apply_guardrail_deprecations();

        assert_eq!(
            config.orgs["acme"].guardrails.pii.as_ref().unwrap().mode,
            PiiModeConfig::Observe
        );
    }

    #[test]
    fn block_pii_shim_requires_both_enabled_flags() {
        let mut config = Config::default();
        let mut org = OrgConfig::default();
        // guardrails.enabled is false — the dead flag never applied here.
        org.guardrails.content_filter = Some(ContentFilterConfig {
            enabled: true,
            block_pii: true,
            ..Default::default()
        });
        config.orgs.insert("acme".to_string(), org);

        config.apply_guardrail_deprecations();
        assert!(config.orgs["acme"].guardrails.pii.is_none());
    }
}
