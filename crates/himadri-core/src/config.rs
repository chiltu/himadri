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
    pub plugins: Vec<PluginConfig>,

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

    #[serde(default)]
    pub audit: AuditConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ContentFilterConfig {
    #[serde(default)]
    pub enabled: bool,

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
pub struct PluginConfig {
    pub name: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub config: serde_json::Value,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AdminConfig {
    pub enabled: bool,

    /// Master admin key. Never serialized: `Config` is returned verbatim by
    /// `GET /admin/config` and `/admin/config/history`, and the master key
    /// must not be readable by admin-scoped principals (e.g. JWT-role
    /// admins) or flow into config exports (CWE-522).
    #[serde(skip_serializing)]
    pub master_key: Option<String>,
}

impl Default for AdminConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            master_key: None,
        }
    }
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

        let config = match ext {
            "json" => serde_json::from_str::<Config>(&content)
                .map_err(|e| ConfigError::Parse(e.to_string()))?,
            _ => {
                return Err(ConfigError::InvalidValue {
                    field: "config_file".to_string(),
                    reason: format!("unsupported extension: {}", ext),
                })
            }
        };

        Ok(config)
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
                provider: "openai".to_string(),
                weight: 1.0,
                models: None,
                api_key_env: Some("OPENAI_API_KEY".to_string()),
                base_url: None,
            }],
            plugins: vec![],
            observability: ObservabilityConfig::default(),
            rate_limit: RateLimitConfig::default(),
            admin: AdminConfig::default(),
            orgs: HashMap::new(),
            cors: CorsConfig::default(),
            rbac: RbacConfig::default(),
        }
    }
}
