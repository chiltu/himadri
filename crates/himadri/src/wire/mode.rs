//! The provider-routing source decision: where routing targets come from.
//!
//! `DATABASE_URL` is a *storage* decision (API keys, usage, request logs) and
//! never by itself decides routing. The routing source is:
//!
//! - **Auto** (default): env/config targets route until the database actually
//!   produces targets, at which point the DB owns routing and the env-provider
//!   keys become the fallback. The startup `RebuildOutcome` — not a prediction
//!   — is what says which side is active.
//! - **Db** (`HIMADRI_PROVIDER_SOURCE=db`): a strict assertion for deployments
//!   that must never route with env-configured providers. Env provider
//!   registration is skipped wholesale, and boot fails fast without a
//!   `DATABASE_URL` — or on an unrecognized value, so a typo cannot silently
//!   mean "auto".

use super::providers::{api_key_env_var, non_preset_env};

/// Where provider routing comes from. Parsed from `HIMADRI_PROVIDER_SOURCE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderSource {
    /// Env/config routing with automatic DB takeover (the default).
    Auto,
    /// Database only: env provider registration is skipped entirely.
    Db,
}

pub const PROVIDER_SOURCE_VAR: &str = "HIMADRI_PROVIDER_SOURCE";

impl ProviderSource {
    /// Parse a `HIMADRI_PROVIDER_SOURCE` value. Unrecognized values are an
    /// error, not a fallback — the whole point of the strict knob is that a
    /// misspelling must not silently downgrade to Auto.
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => Ok(Self::Auto),
            "db" | "database" => Ok(Self::Db),
            other => Err(format!(
                "{PROVIDER_SOURCE_VAR} has unrecognized value '{other}' (expected 'auto' or 'db')"
            )),
        }
    }

    pub fn from_env() -> Result<Self, String> {
        match std::env::var(PROVIDER_SOURCE_VAR) {
            Ok(raw) => Self::parse(&raw),
            Err(_) => Ok(Self::Auto),
        }
    }
}

/// The provider env vars that are set but not feeding routing while the
/// database provides targets (or under [`ProviderSource::Db`], where env
/// registration is skipped outright).
///
/// Both halves of what `register_providers_from_env` reads: the derived
/// `{TYPE}_API_KEY` of every preset vendor, and the non-preset vars from
/// [`non_preset_env::ALL`] (the OpenAI base-URL override, the secondary
/// endpoint, and the Azure set).
///
/// `is_set` is injected so tests don't race the process environment.
pub fn inert_provider_env_vars(is_set: impl Fn(&str) -> bool) -> Vec<String> {
    himadri_provider::compatible::presets()
        .map(|(provider_type, _)| api_key_env_var(provider_type))
        .chain(non_preset_env::ALL.iter().map(|v| v.to_string()))
        .filter(|var| is_set(var))
        .collect()
}

pub fn inert_provider_env_vars_from_env() -> Vec<String> {
    inert_provider_env_vars(|var| std::env::var(var).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_auto_db_and_empty() {
        assert_eq!(ProviderSource::parse(""), Ok(ProviderSource::Auto));
        assert_eq!(ProviderSource::parse("auto"), Ok(ProviderSource::Auto));
        assert_eq!(ProviderSource::parse("db"), Ok(ProviderSource::Db));
        assert_eq!(ProviderSource::parse(" DB "), Ok(ProviderSource::Db));
        assert_eq!(ProviderSource::parse("database"), Ok(ProviderSource::Db));
    }

    #[test]
    fn parse_rejects_typos_instead_of_defaulting() {
        for bad in ["bd", "env", "sqlite", "yes"] {
            let err = ProviderSource::parse(bad).expect_err("must not default on typo");
            assert!(err.contains(bad), "error should echo the value, got: {err}");
        }
    }

    #[test]
    fn inert_vars_lists_exactly_the_set_ones() {
        let set = ["GROQ_API_KEY", "AZURE_OPENAI_API_KEY"];
        let vars = inert_provider_env_vars(|v| set.contains(&v));
        assert_eq!(vars, vec!["GROQ_API_KEY", "AZURE_OPENAI_API_KEY"]);

        assert!(inert_provider_env_vars(|_| false).is_empty());
    }

    #[test]
    fn inert_vars_covers_every_preset_vendor() {
        let all = inert_provider_env_vars(|_| true);
        for (provider_type, _) in himadri_provider::compatible::presets() {
            assert!(
                all.contains(&api_key_env_var(provider_type)),
                "{provider_type}'s key var missing from the inert-var candidates"
            );
        }
        assert!(all.contains(&"OPENAI_SECONDARY_BASE_URL".to_string()));
    }
}
