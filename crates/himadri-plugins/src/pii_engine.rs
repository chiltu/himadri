//! PII detection/redaction engine abstraction and its `redact-core` adapter.
//!
//! The gateway codes against [`PiiEngine`] so the underlying crate is
//! swappable (redact-core is pre-1.0 and pinned exactly; see
//! docs/SPEC_GUARDRAILS.md §4). Detection results deliberately expose entity
//! *types and spans only* — never the matched values — so nothing upstream
//! of the engine can leak a detected secret into logs or error messages.

use std::collections::HashSet;
use std::sync::Arc;

use redact_core::recognizers::pattern::PatternRecognizer;
use redact_core::{
    AnalyzerEngine, AnonymizationStrategy, AnonymizerConfig, EntityType, RecognizerRegistry,
};

/// How detected entities are rewritten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedactStrategy {
    /// `[EMAIL_ADDRESS]`-style placeholder (default).
    Replace,
    /// Partial masking, e.g. `jo**@****le.com`.
    Mask,
    /// Salted irreversible hash suffix, e.g. `[EMAIL_ADDRESS_a1b2c3d4]`.
    Hash,
    /// Reversible `<TOKEN_uuid>` (requires an encryption key).
    Encrypt,
    /// Remove the matched text entirely.
    Remove,
}

impl RedactStrategy {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "replace" => Some(Self::Replace),
            "mask" => Some(Self::Mask),
            "hash" => Some(Self::Hash),
            "encrypt" => Some(Self::Encrypt),
            "remove" => Some(Self::Remove),
            _ => None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PiiError {
    #[error("pii engine error: {0}")]
    Engine(String),
    #[error("pii engine misconfigured: {0}")]
    Config(String),
}

/// A detected entity: type, byte span, confidence. No value.
#[derive(Debug, Clone)]
pub struct PiiEntityHit {
    pub entity_type: String,
    pub start: usize,
    pub end: usize,
    pub confidence: f32,
}

#[derive(Debug, Clone)]
pub struct RedactOptions {
    pub strategy: RedactStrategy,
    /// Entity types (string form, e.g. `EMAIL_ADDRESS`) to act on.
    /// `None` = every type the engine detects.
    pub entities: Option<HashSet<String>>,
    /// Detections below this confidence are ignored.
    pub min_confidence: f32,
}

impl Default for RedactOptions {
    fn default() -> Self {
        Self {
            strategy: RedactStrategy::Replace,
            entities: None,
            min_confidence: 0.6,
        }
    }
}

/// Result of a redaction pass: the rewritten text plus what was replaced
/// (entity types and counts only — original values are structurally absent).
#[derive(Debug, Clone)]
pub struct RedactOutcome {
    pub text: String,
    /// `(entity_type, count)` pairs, one per distinct type, sorted by type.
    pub replaced: Vec<(String, u32)>,
}

/// What the guardrail plugins need from a PII engine. Synchronous and
/// CPU-bound by contract; callers decide whether to `spawn_blocking`.
pub trait PiiEngine: Send + Sync {
    /// Detect entities honoring `opts.entities` / `opts.min_confidence`.
    fn scan(&self, text: &str, opts: &RedactOptions) -> Result<Vec<PiiEntityHit>, PiiError>;

    /// Rewrite `text` per `opts`. Text without matches is returned unchanged
    /// (same contents, `replaced` empty).
    fn redact(&self, text: &str, opts: &RedactOptions) -> Result<RedactOutcome, PiiError>;
}

/// Gateway-specific secret patterns registered on top of redact-core's 36
/// default recognizers, mirroring `himadri-observability`'s audit `Redactor`
/// so credentials are covered inline, not just classic PII.
const SECRET_PATTERNS: &[(&str, &str, f32)] = &[
    (
        "GW_JWT",
        r"eyJ[a-zA-Z0-9_-]+\.eyJ[a-zA-Z0-9_-]+\.[a-zA-Z0-9_-]+",
        0.95,
    ),
    ("GW_AWS_KEY", r"AKIA[0-9A-Z]{16}", 0.95),
    // OpenAI-style secrets — including this gateway's own `sk-…` keys — are
    // the most likely credential to appear in prompts.
    ("GW_API_KEY", r"sk-[a-zA-Z0-9_-]{16,}", 0.9),
    ("GW_BEARER_TOKEN", r"(?i)bearer\s+[a-zA-Z0-9._~+/=-]{16,}", 0.9),
];

/// Secrets used by the hash/encrypt strategies. Sourced from env at wiring
/// time; deliberately not part of `Config` (which `GET /admin/config`
/// serializes verbatim — CWE-522, same reasoning as `AdminConfig.master_key`).
#[derive(Default, Clone)]
pub struct EngineSecrets {
    pub hash_salt: Option<String>,
    pub encryption_key: Option<String>,
}

impl EngineSecrets {
    pub fn from_env() -> Self {
        Self {
            hash_salt: std::env::var("GUARDRAILS_HASH_SALT").ok(),
            encryption_key: std::env::var("GUARDRAILS_ENCRYPTION_KEY").ok(),
        }
    }
}

/// `redact-core`-backed implementation of [`PiiEngine`].
pub struct RedactCoreEngine {
    engine: AnalyzerEngine,
    secrets: EngineSecrets,
}

impl RedactCoreEngine {
    /// Build the engine once at startup: redact-core's default pattern
    /// recognizers plus the gateway secret patterns. Entity subset,
    /// confidence threshold, and strategy are per-call, so live config
    /// reloads never rebuild the engine.
    pub fn new(secrets: EngineSecrets) -> Result<Arc<Self>, PiiError> {
        // One PatternRecognizer carrying defaults *and* custom patterns:
        // `AnalyzerEngine::new()` would install its own default recognizer,
        // and a second one would double-detect every default pattern.
        let mut pattern = PatternRecognizer::new();
        for &(name, regex, score) in SECRET_PATTERNS {
            pattern
                .add_pattern(EntityType::Custom(name.to_string()), regex, score)
                .map_err(|e| PiiError::Config(format!("invalid pattern {name}: {e}")))?;
        }

        let mut registry = RecognizerRegistry::new();
        registry.add_recognizer(Arc::new(pattern));

        let engine = AnalyzerEngine::builder()
            .with_recognizer_registry(registry)
            .build();

        Ok(Arc::new(Self { engine, secrets }))
    }

    /// Analyze and apply the entity/confidence filter. The registry has
    /// already resolved overlapping detections; filtering only removes
    /// entries, so the result stays non-overlapping.
    fn filtered_entities(
        &self,
        text: &str,
        opts: &RedactOptions,
    ) -> Result<Vec<redact_core::RecognizerResult>, PiiError> {
        let analysis = self
            .engine
            .analyze(text, None)
            .map_err(|e| PiiError::Engine(e.to_string()))?;

        Ok(analysis
            .detected_entities
            .into_iter()
            .filter(|e| e.score >= opts.min_confidence)
            .filter(|e| match &opts.entities {
                Some(set) => set.contains(e.entity_type.as_str()),
                None => true,
            })
            // Drop any value the recognizer captured: nothing past this
            // point may carry the matched text.
            .map(|mut e| {
                e.text = None;
                e.context = None;
                e
            })
            .collect())
    }

    fn anonymizer_config(&self, opts: &RedactOptions) -> Result<AnonymizerConfig, PiiError> {
        let strategy = match opts.strategy {
            RedactStrategy::Replace => AnonymizationStrategy::Replace,
            RedactStrategy::Mask => AnonymizationStrategy::Mask,
            RedactStrategy::Hash => AnonymizationStrategy::Hash,
            RedactStrategy::Encrypt => AnonymizationStrategy::Encrypt,
            RedactStrategy::Remove => AnonymizationStrategy::Redact,
        };
        if strategy == AnonymizationStrategy::Encrypt && self.secrets.encryption_key.is_none() {
            return Err(PiiError::Config(
                "encrypt strategy requires GUARDRAILS_ENCRYPTION_KEY".to_string(),
            ));
        }
        Ok(AnonymizerConfig {
            strategy,
            encryption_key: self.secrets.encryption_key.clone(),
            hash_salt: self.secrets.hash_salt.clone(),
            preserve_format: true,
            ..Default::default()
        })
    }
}

impl PiiEngine for RedactCoreEngine {
    fn scan(&self, text: &str, opts: &RedactOptions) -> Result<Vec<PiiEntityHit>, PiiError> {
        Ok(self
            .filtered_entities(text, opts)?
            .into_iter()
            .map(|e| PiiEntityHit {
                entity_type: e.entity_type.as_str().to_string(),
                start: e.start,
                end: e.end,
                confidence: e.score,
            })
            .collect())
    }

    fn redact(&self, text: &str, opts: &RedactOptions) -> Result<RedactOutcome, PiiError> {
        let entities = self.filtered_entities(text, opts)?;
        if entities.is_empty() {
            return Ok(RedactOutcome {
                text: text.to_string(),
                replaced: Vec::new(),
            });
        }

        let mut counts: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
        for e in &entities {
            *counts.entry(e.entity_type.as_str().to_string()).or_insert(0) += 1;
        }

        let config = self.anonymizer_config(opts)?;
        // The engine's own `anonymize()` re-analyzes with no entity or
        // confidence filter, so drive the anonymizer registry directly with
        // the filtered set.
        let result = self
            .engine
            .anonymizer_registry()
            .anonymize(text, entities, &config)
            .map_err(|e| PiiError::Engine(e.to_string()))?;

        Ok(RedactOutcome {
            text: result.text,
            replaced: counts.into_iter().collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> Arc<RedactCoreEngine> {
        RedactCoreEngine::new(EngineSecrets::default()).unwrap()
    }

    #[test]
    fn detects_and_replaces_email_and_ssn() {
        let out = engine()
            .redact(
                "Mail john@example.com, SSN 123-45-6789.",
                &RedactOptions::default(),
            )
            .unwrap();
        assert!(out.text.contains("[EMAIL_ADDRESS]"), "{}", out.text);
        assert!(out.text.contains("[US_SSN]"), "{}", out.text);
        assert!(!out.text.contains("john@example.com"));
        assert!(!out.text.contains("123-45-6789"));
        assert!(out
            .replaced
            .iter()
            .any(|(t, n)| t == "EMAIL_ADDRESS" && *n == 1));
    }

    #[test]
    fn clean_text_passes_through_unchanged() {
        let text = "Summarize the quarterly report in three bullet points.";
        let out = engine().redact(text, &RedactOptions::default()).unwrap();
        assert_eq!(out.text, text);
        assert!(out.replaced.is_empty());
    }

    #[test]
    fn entity_subset_limits_what_is_redacted() {
        let opts = RedactOptions {
            entities: Some(["EMAIL_ADDRESS".to_string()].into_iter().collect()),
            ..Default::default()
        };
        let out = engine()
            .redact("Mail john@example.com, SSN 123-45-6789.", &opts)
            .unwrap();
        assert!(out.text.contains("[EMAIL_ADDRESS]"));
        assert!(out.text.contains("123-45-6789"), "{}", out.text);
    }

    #[test]
    fn gateway_secret_patterns_are_detected() {
        let out = engine()
            .redact(
                "Use key sk-abcdefghij0123456789 and AKIAIOSFODNN7EXAMPLE.",
                &RedactOptions::default(),
            )
            .unwrap();
        assert!(!out.text.contains("sk-abcdefghij0123456789"), "{}", out.text);
        assert!(!out.text.contains("AKIAIOSFODNN7EXAMPLE"), "{}", out.text);
        let types: Vec<&str> = out.replaced.iter().map(|(t, _)| t.as_str()).collect();
        assert!(types.contains(&"GW_API_KEY"), "{:?}", types);
        assert!(types.contains(&"GW_AWS_KEY"), "{:?}", types);
    }

    #[test]
    fn scan_reports_hits_without_values() {
        let hits = engine()
            .scan("Reach me at jane@corp.org", &RedactOptions::default())
            .unwrap();
        assert!(hits.iter().any(|h| h.entity_type == "EMAIL_ADDRESS"));
    }

    #[test]
    fn min_confidence_filters_low_score_hits() {
        let opts = RedactOptions {
            min_confidence: 0.99,
            ..Default::default()
        };
        let out = engine().redact("Mail john@example.com", &opts).unwrap();
        assert!(out.replaced.is_empty(), "{:?}", out.replaced);
        assert!(out.text.contains("john@example.com"));
    }

    #[test]
    fn encrypt_without_key_is_a_config_error() {
        let opts = RedactOptions {
            strategy: RedactStrategy::Encrypt,
            ..Default::default()
        };
        let err = engine().redact("Mail john@example.com", &opts).unwrap_err();
        assert!(matches!(err, PiiError::Config(_)));
    }
}
