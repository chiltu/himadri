//! Provider registry wire-up: the one place that decides which provider types
//! this gateway can route to.
//!
//! Each provider module owns how it is built; this module owns which ones are
//! registered. Adding a vendor is a `register` call here plus that vendor's own
//! registration function — no routing-path code changes.

use std::sync::Arc;

use himadri_provider::{MapProviderRegistry, ProviderRegistry};

/// Build the provider registry used by DB-driven routing.
///
/// ENV mode does not go through this: it registers concrete provider instances
/// directly from environment variables at startup.
pub fn build_provider_registry() -> Arc<dyn ProviderRegistry> {
    let mut registry = MapProviderRegistry::new();

    himadri_provider::anthropic::register(&mut registry);
    himadri_provider::gemini::register(&mut registry);
    himadri_provider::compatible::register(&mut registry);

    Arc::new(registry)
}

/// The env vars ENV-mode provider registration reads beyond the derived
/// `{TYPE}_API_KEY` presets — the ones gating or configuring the non-preset
/// registrations.
///
/// Named here rather than written as literals at the read sites so that
/// registration and the "these are inert under database routing" warning read
/// one list: a var that only one of them knows about is a var the operator is
/// never told is being ignored.
pub mod non_preset_env {
    /// Base-URL override for the always-registered OpenAI provider.
    pub const OPENAI_BASE_URL: &str = "OPENAI_BASE_URL";
    /// Presence registers a second OpenAI-compatible upstream.
    pub const OPENAI_SECONDARY_BASE_URL: &str = "OPENAI_SECONDARY_BASE_URL";
    pub const AZURE_OPENAI_API_KEY: &str = "AZURE_OPENAI_API_KEY";
    pub const AZURE_OPENAI_ENDPOINT: &str = "AZURE_OPENAI_ENDPOINT";
    pub const AZURE_OPENAI_DEPLOYMENT: &str = "AZURE_OPENAI_DEPLOYMENT";
    pub const AZURE_OPENAI_API_VERSION: &str = "AZURE_OPENAI_API_VERSION";

    /// Every var above. Add a row when a new non-preset registration reads one.
    pub const ALL: &[&str] = &[
        OPENAI_BASE_URL,
        OPENAI_SECONDARY_BASE_URL,
        AZURE_OPENAI_API_KEY,
        AZURE_OPENAI_ENDPOINT,
        AZURE_OPENAI_DEPLOYMENT,
        AZURE_OPENAI_API_VERSION,
    ];
}

/// The API-key env var that enables a preset vendor in ENV mode:
/// `openrouter` → `OPENROUTER_API_KEY`.
///
/// Deriving the name is what lets a vendor be added in one place. The cost is
/// that a `provider_type` which doesn't uppercase into a valid identifier would
/// derive an env var nobody sets, silently leaving the vendor unregistered —
/// `preset_api_key_env_vars_match_the_documented_names` guards that.
pub fn api_key_env_var(provider_type: &str) -> String {
    format!("{}_API_KEY", provider_type.to_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `KNOWN_PROVIDER_TYPES` is what the admin API advertises on
    /// `GET /admin/known-providers` and what the web UI offers in its provider
    /// picker. Anything advertised there must actually build, or an operator can
    /// create an endpoint that lists in `/v1/models` and then 404s on every
    /// completion.
    #[test]
    fn every_advertised_provider_type_builds() {
        let registry = build_provider_registry();
        for provider_type in himadri_core::KNOWN_PROVIDER_TYPES {
            assert!(
                registry.is_known(provider_type),
                "{provider_type} is advertised in KNOWN_PROVIDER_TYPES but the registry cannot build it"
            );
        }
    }

    /// The other direction: a type registered here but missing from
    /// `KNOWN_PROVIDER_TYPES` is routable yet invisible to the UI, so operators
    /// can never select it.
    #[test]
    fn every_registered_provider_type_is_advertised() {
        let mut registry = MapProviderRegistry::new();
        himadri_provider::anthropic::register(&mut registry);
        himadri_provider::gemini::register(&mut registry);
        himadri_provider::compatible::register(&mut registry);

        for provider_type in registry.registered_types() {
            assert!(
                himadri_core::is_known_provider_type(provider_type),
                "{provider_type} is registered but missing from KNOWN_PROVIDER_TYPES, so the UI cannot offer it"
            );
        }

        // Without this the loop above passes vacuously if the wire-up ever stops
        // registering anything. Together with `every_advertised_provider_type_builds`
        // it pins the two lists to exactly the same set.
        assert_eq!(
            registry.registered_types().count(),
            himadri_core::KNOWN_PROVIDER_TYPES.len(),
            "registered provider types and KNOWN_PROVIDER_TYPES have drifted apart"
        );
    }

    /// ENV mode derives each vendor's API-key env var from its `provider_type`,
    /// so a preset renamed in `himadri_provider::compatible` silently changes the
    /// var operators must set — the vendor would just never register, with no
    /// error. Pin the derived names to the ones documented in
    /// `docs/configuration.md`.
    #[test]
    fn preset_api_key_env_vars_match_the_documented_names() {
        let derived: Vec<String> = himadri_provider::compatible::presets()
            .filter(|(provider_type, _)| *provider_type != "openai")
            .map(|(provider_type, _)| api_key_env_var(provider_type))
            .collect();

        assert_eq!(
            derived,
            [
                "OPENROUTER_API_KEY",
                "TOGETHER_API_KEY",
                "GROQ_API_KEY",
                "FIREWORKS_API_KEY",
                "DEEPINFRA_API_KEY",
                "CEREBRAS_API_KEY",
                "NOVITA_API_KEY",
            ],
            "the API-key env vars ENV mode reads no longer match docs/configuration.md"
        );
    }

    /// Every preset vendor must also be a type DB mode can route, or a vendor
    /// would work in ENV mode and be unavailable to DB-configured endpoints.
    #[test]
    fn every_preset_vendor_is_routable_in_db_mode_too() {
        for (provider_type, _) in himadri_provider::compatible::presets() {
            assert!(
                himadri_core::is_known_provider_type(provider_type),
                "{provider_type} is an ENV-mode preset but not a known DB-mode provider type"
            );
        }
    }

    /// The display names ENV mode logs come off the preset config; they used to
    /// be a hand-kept third column in main.rs.
    #[test]
    fn preset_configs_carry_their_own_display_names() {
        for (provider_type, preset) in himadri_provider::compatible::presets() {
            let config = preset();
            assert_eq!(
                config.name, provider_type,
                "preset config name must equal its provider_type"
            );
            assert!(
                !config.display_name.is_empty(),
                "{provider_type} has no display_name to log"
            );
        }
    }

    #[test]
    fn unregistered_type_still_routes_with_an_explicit_base_url() {
        let registry = build_provider_registry();
        assert!(registry
            .validate("my-self-hosted-vllm", Some("http://localhost:8000/v1"))
            .is_ok());
        assert!(registry.validate("my-self-hosted-vllm", None).is_err());
    }
}
