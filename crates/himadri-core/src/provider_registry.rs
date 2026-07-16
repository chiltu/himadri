//! The provider types the gateway *advertises*: the admin API serves this list
//! on `GET /admin/known-providers`, the web UI's provider picker fetches it
//! from there, and `/v1/models` filters against it.
//!
//! The *executable* truth is `himadri_provider`'s `ProviderRegistry` — what the
//! target rebuild actually builds clients from. This list mirrors it statically
//! for consumers that cannot reach it (himadri-admin depends on himadri-core
//! only). The two are pinned to exactly the same set, in both directions, by
//! the tests in `himadri::wire::providers`: a type advertised here that the
//! registry cannot build would list in `/v1/models` and then 404 on every
//! completion, and a registered type missing here would route but never be
//! offered in the UI.

/// Provider types with a built-in preset (default base URL, auth scheme,
/// extra headers). Endpoints of any other `provider_type` are routable only
/// when they carry an explicit `base_url` for the generic Bearer
/// OpenAI-compatible client.
pub const KNOWN_PROVIDER_TYPES: &[&str] = &[
    "openai",
    "anthropic",
    "gemini",
    "openrouter",
    "together",
    "groq",
    "fireworks",
    "deepinfra",
    "cerebras",
    "novita",
];

pub fn is_known_provider_type(provider_type: &str) -> bool {
    KNOWN_PROVIDER_TYPES.contains(&provider_type)
}

/// Whether a model endpoint can actually be routed: a known preset type, or
/// any type with an explicit non-empty `base_url`.
///
/// The static mirror of the rule `ProviderRegistry::build` applies when the
/// target rebuild skips an endpoint. `/v1/models` must apply the same rule —
/// otherwise a model advertises in the catalog and then 404s on every
/// completion — but cannot reach the registry from himadri-admin, hence the
/// duplicate; the drift tests in `himadri::wire::providers` keep them in
/// agreement.
pub fn endpoint_is_routable(provider_type: &str, base_url: Option<&str>) -> bool {
    is_known_provider_type(provider_type) || base_url.map(str::trim).is_some_and(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_types_route_without_base_url() {
        for t in KNOWN_PROVIDER_TYPES {
            assert!(endpoint_is_routable(t, None), "{t} should be routable");
        }
    }

    #[test]
    fn unknown_type_requires_base_url() {
        assert!(!endpoint_is_routable("my-vendor", None));
        assert!(!endpoint_is_routable("my-vendor", Some("  ")));
        assert!(endpoint_is_routable(
            "my-vendor",
            Some("http://localhost:8080/v1")
        ));
    }
}
