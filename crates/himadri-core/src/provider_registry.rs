//! Single source of truth for the provider types the gateway knows how to
//! build a client for without an explicit `base_url`.
//!
//! Three places previously hardcoded this list independently (the gateway's
//! client factory, env-var provider registration, and the web UI's provider
//! picker), so adding a vendor meant editing all three and forgetting one
//! produced models that list in `/v1/models` but 404 on completion. The
//! gateway's client factory must keep constructing a client for every entry
//! here (enforced by a test in `himadri::gateway`), the admin API serves the
//! list on `GET /admin/known-providers`, and the web UI fetches it from there.

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
/// any type with an explicit non-empty `base_url`. This is the same rule the
/// gateway's target rebuild applies when it skips endpoints, and `/v1/models`
/// must apply it too — otherwise a model advertises in the catalog and then
/// 404s on every completion.
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
