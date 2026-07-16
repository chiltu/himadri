//! Provider registry: the single source of truth for turning a `provider_type`
//! name into a live provider client.
//!
//! Provider types are not enumerated here. Each provider module contributes its
//! own types via a `register` function (see [`anthropic::register`],
//! [`gemini::register`], [`compatible::register`]), and the gateway's wire-up
//! decides which ones to call. Adding a vendor therefore touches that vendor's
//! module and the wire-up list — never a match arm in the middle of the routing
//! path.
//!
//! [`anthropic::register`]: crate::anthropic::register
//! [`gemini::register`]: crate::gemini::register
//! [`compatible::register`]: crate::compatible::register

use std::collections::HashMap;
use std::sync::Arc;

use crate::compatible::OpenAiCompatibleProvider;
use crate::{error::ProviderError, traits::Provider};

/// Constructs one provider client, given an optional `base_url` override.
///
/// The override is already trimmed and non-empty when `Some`; a builder that
/// ignores it pins the provider to its preset URL.
pub type ProviderBuilder =
    Box<dyn Fn(Option<&str>) -> Result<Arc<dyn Provider>, ProviderError> + Send + Sync>;

/// Builds provider clients by `provider_type`.
///
/// Constructed once at startup and injected into the gateway, which uses it both
/// to build clients during a target rebuild and to validate endpoints at the
/// admin API boundary before they reach the database.
pub trait ProviderRegistry: Send + Sync {
    /// Build a client for `provider_type`, with `base_url` overriding the
    /// provider's preset when supplied.
    ///
    /// A registered type builds with or without a `base_url`. An unregistered
    /// type builds only with an explicit `base_url`, served by a generic Bearer
    /// OpenAI-compatible client; without one it fails with
    /// [`ProviderError::MissingBaseUrl`], since a generic client has nowhere to
    /// send the request.
    fn build(
        &self,
        provider_type: &str,
        base_url: Option<&str>,
    ) -> Result<Arc<dyn Provider>, ProviderError>;

    /// Whether this `provider_type`/`base_url` pair can be built at all.
    ///
    /// The admin API calls this before persisting an endpoint, so a
    /// misconfigured route is rejected at creation rather than silently skipped
    /// on the next rebuild.
    fn validate(&self, provider_type: &str, base_url: Option<&str>) -> Result<(), ProviderError> {
        self.build(provider_type, base_url).map(|_| ())
    }

    /// Whether `provider_type` is registered, and so routable without a
    /// `base_url` of its own.
    fn is_known(&self, provider_type: &str) -> bool {
        self.validate(provider_type, None).is_ok()
    }
}

/// A [`ProviderRegistry`] backed by a map of per-type builders.
///
/// Populated by `register` calls at startup, then frozen behind an `Arc` for the
/// life of the process.
#[derive(Default)]
pub struct MapProviderRegistry {
    builders: HashMap<String, ProviderBuilder>,
}

impl MapProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `builder` under `provider_type`, replacing any previous entry.
    ///
    /// Later registrations win, so a deployment can override a built-in preset
    /// by registering its own builder after the defaults.
    pub fn register<F>(&mut self, provider_type: impl Into<String>, builder: F)
    where
        F: Fn(Option<&str>) -> Result<Arc<dyn Provider>, ProviderError> + Send + Sync + 'static,
    {
        self.builders
            .insert(provider_type.into(), Box::new(builder));
    }

    /// The registered type names, unordered.
    pub fn registered_types(&self) -> impl Iterator<Item = &str> {
        self.builders.keys().map(String::as_str)
    }
}

impl ProviderRegistry for MapProviderRegistry {
    fn build(
        &self,
        provider_type: &str,
        base_url: Option<&str>,
    ) -> Result<Arc<dyn Provider>, ProviderError> {
        let base_url = base_url.map(str::trim).filter(|s| !s.is_empty());

        match self.builders.get(provider_type) {
            Some(builder) => builder(base_url),
            // Unregistered vendor: a generic Bearer client needs an explicit URL.
            None => {
                let base_url = base_url
                    .ok_or_else(|| ProviderError::MissingBaseUrl(provider_type.to_string()))?;
                Ok(Arc::new(OpenAiCompatibleProvider::bearer(
                    provider_type,
                    base_url,
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Result::unwrap_err` needs the `Ok` type to be `Debug`, which
    /// `Arc<dyn Provider>` cannot be. Drop the `Ok` side instead.
    fn expect_err(result: Result<Arc<dyn Provider>, ProviderError>) -> ProviderError {
        result.err().expect("expected build to fail")
    }

    fn stub_registry() -> MapProviderRegistry {
        let mut registry = MapProviderRegistry::new();
        registry.register("stub", |base_url| {
            Ok(Arc::new(OpenAiCompatibleProvider::bearer(
                "stub",
                base_url.unwrap_or("https://stub.example.com/v1"),
            )))
        });
        registry
    }

    #[test]
    fn registered_type_builds_without_base_url() {
        assert!(stub_registry().build("stub", None).is_ok());
    }

    #[test]
    fn builder_receives_the_base_url_override() {
        let mut registry = MapProviderRegistry::new();
        registry.register("echo", |base_url| {
            // Fail with the received override so the test can read it back.
            Err(ProviderError::InvalidConfiguration(
                base_url.unwrap_or("<none>").to_string(),
            ))
        });

        let err = expect_err(registry.build("echo", Some("https://custom/v1")));
        assert!(
            matches!(err, ProviderError::InvalidConfiguration(url) if url == "https://custom/v1"),
        );
    }

    #[test]
    fn blank_base_url_reaches_the_builder_as_none() {
        let mut registry = MapProviderRegistry::new();
        registry.register("echo", |base_url| {
            Err(ProviderError::InvalidConfiguration(
                base_url.unwrap_or("<none>").to_string(),
            ))
        });

        let err = expect_err(registry.build("echo", Some("   ")));
        assert!(matches!(err, ProviderError::InvalidConfiguration(url) if url == "<none>"));
    }

    #[test]
    fn unregistered_type_without_base_url_is_rejected() {
        let err = expect_err(stub_registry().build("mystery", None));
        assert!(
            matches!(&err, ProviderError::MissingBaseUrl(t) if t == "mystery"),
            "expected MissingBaseUrl(\"mystery\"), got {err:?}"
        );
    }

    #[test]
    fn unregistered_type_with_base_url_builds_a_generic_client() {
        assert!(stub_registry()
            .build("mystery", Some("https://api.example.com/v1"))
            .is_ok());
    }

    #[test]
    fn later_registration_overrides_earlier() {
        let mut registry = MapProviderRegistry::new();
        registry.register("dup", |_| Err(ProviderError::Internal("first".into())));
        registry.register("dup", |_| Err(ProviderError::Internal("second".into())));

        let err = expect_err(registry.build("dup", None));
        assert!(matches!(err, ProviderError::Internal(m) if m == "second"));
    }

    #[test]
    fn is_known_tracks_registration_not_routability() {
        let registry = stub_registry();
        assert!(registry.is_known("stub"));
        // Routable with a base_url, but still not a registered type.
        assert!(!registry.is_known("mystery"));
        assert!(registry
            .validate("mystery", Some("https://api.example.com/v1"))
            .is_ok());
    }
}
