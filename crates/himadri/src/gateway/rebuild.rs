//! Rebuilding routing targets from DB models and their endpoints — the
//! single bridge between the admin tables and live routing state.

use tracing::warn;

use himadri_core::Target;

use super::providers::build_provider_client;
use super::Gateway;

impl Gateway {
    /// Rebuild routing targets from database models and their endpoints.
    /// Called when a model or endpoint is created, updated, deleted, or toggled.
    ///
    /// Each enabled endpoint of each enabled model becomes one routing target,
    /// keyed by the endpoint id so the same provider type can back several
    /// endpoints with distinct credentials/URLs. A model with no enabled
    /// endpoint contributes no targets and is therefore inactive/unroutable.
    pub async fn rebuild_targets_from_db(
        &self,
        models: &[himadri_admin::Model],
        endpoints: &[himadri_admin::ModelEndpoint],
    ) {
        // Build the new target list and key set before taking any locks or
        // touching the live maps, so in-flight requests keep resolving
        // against the previous state until the swap below.
        let mut new_targets = Vec::new();
        let mut new_keys: Vec<(String, String)> = Vec::new();

        for model in models {
            if !model.enabled {
                continue;
            }

            for endpoint in endpoints
                .iter()
                .filter(|e| e.model_id == model.id && e.enabled)
            {
                // Register a client under the endpoint id so the target resolves
                // at request time. Skip endpoints whose provider type is unknown
                // and carries no base_url (a generic client has nowhere to go).
                let base_url = endpoint.base_url.as_deref();
                let Some(client) = build_provider_client(&endpoint.provider_type, base_url) else {
                    warn!(
                        endpoint = %endpoint.id,
                        provider_type = %endpoint.provider_type,
                        "skipping endpoint: unknown provider type with no base_url configured"
                    );
                    continue;
                };
                self.register_provider_as(&endpoint.id, client);

                // Stash the (already decrypted) key, keyed by endpoint id, so
                // get_api_key can use it; it must not travel on the target.
                if let Some(key) = endpoint.api_key.as_deref().filter(|k| !k.is_empty()) {
                    new_keys.push((endpoint.id.clone(), key.to_string()));
                }

                new_targets.push(Target {
                    id: Some(endpoint.id.clone()),
                    provider: endpoint.provider_type.clone(),
                    weight: endpoint.weight,
                    models: Some(vec![model.name.clone()]),
                    api_key_env: None, // API key is stashed in provider_keys
                    base_url: endpoint.base_url.clone(),
                });
            }
        }

        // Repopulate provider_keys without an empty window: insert the new
        // entries first, then drop the ones that no longer exist. An in-flight
        // request holding an old target sees either the old or the new key for
        // its endpoint — never a missing one (which would go upstream as an
        // empty Bearer token and trip the circuit breaker on a 401).
        let live_keys: std::collections::HashSet<String> =
            new_keys.iter().map(|(id, _)| id.clone()).collect();
        for (id, key) in new_keys {
            self.provider_keys.insert(id, key);
        }
        self.provider_keys.retain(|id, _| live_keys.contains(id));

        let active_ids: std::collections::HashSet<String> =
            new_targets.iter().filter_map(|t| t.id.clone()).collect();

        // Lock order: config before targets (see the field docs on `Gateway`).
        let mut config = self.config.write().await;
        let mut targets = self.targets.write().await;
        config.targets = new_targets.clone();
        *targets = new_targets;
        drop(targets);
        drop(config);

        // Drop circuit breakers only for endpoints that no longer route;
        // surviving endpoints keep their breaker state so toggling one
        // endpoint doesn't reset health tracking for the whole fleet. Rate
        // limiter buckets are keyed by org/key — not by endpoint — so an
        // endpoint mutation must not touch them at all.
        self.circuit_breakers
            .retain(|id, _| active_ids.contains(id));
    }

    /// True when the model/endpoint tables would produce at least one routing
    /// target — i.e. some enabled model has at least one enabled endpoint.
    /// Callers gate [`Self::rebuild_targets_from_db`] on this at startup and
    /// after a config apply, so a DB holding only disabled rows doesn't
    /// replace the config/env targets with an empty list (a full outage).
    pub fn db_has_active_targets(
        models: &[himadri_admin::Model],
        endpoints: &[himadri_admin::ModelEndpoint],
    ) -> bool {
        models
            .iter()
            .any(|m| m.enabled && endpoints.iter().any(|e| e.enabled && e.model_id == m.id))
    }
}

#[cfg(test)]
mod lock_order_tests {
    use super::*;
    use crate::gateway::target_serves_model;
    use himadri_circuitbreaker::{CircuitBreaker, CircuitBreakerConfig};
    use himadri_core::{ChatCompletionRequest, Config, GatewayError};
    use himadri_observability::Metrics;
    use std::sync::Arc;

    fn model(id: &str, name: &str) -> himadri_admin::Model {
        himadri_admin::Model {
            id: id.to_string(),
            name: name.to_string(),
            display_name: None,
            enabled: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn endpoint(
        id: &str,
        model_id: &str,
        provider_type: &str,
        base_url: Option<&str>,
        weight: f64,
    ) -> himadri_admin::ModelEndpoint {
        himadri_admin::ModelEndpoint {
            id: id.to_string(),
            model_id: model_id.to_string(),
            provider_type: provider_type.to_string(),
            base_url: base_url.map(str::to_string),
            api_key: Some("sk-test".to_string()),
            weight,
            enabled: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    /// Regression test: an endpoint must get a client registered in
    /// `self.providers` (keyed by endpoint id) so its routing target resolves.
    /// Without this, every request routed to a DB endpoint 503s with
    /// "Provider not found".
    #[tokio::test]
    async fn rebuild_registers_provider_clients_for_endpoints() {
        let gw = Gateway::new(Config::default(), Arc::new(Metrics::new()));
        assert!(gw.get_provider("ep1").is_none());

        let m = model("m1", "nvidia/nemotron:free");
        let ep = endpoint("ep1", "m1", "openrouter", None, 1.0);
        gw.rebuild_targets_from_db(&[m], &[ep]).await;

        // Client is registered under the endpoint id, and the decrypted key was
        // stashed (also keyed by endpoint id) for get_api_key.
        assert!(gw.get_provider("ep1").is_some());
        let target = Target {
            id: Some("ep1".to_string()),
            provider: "openrouter".to_string(),
            weight: 1.0,
            models: None,
            api_key_env: None,
            base_url: None,
        };
        assert_eq!(gw.get_api_key(&target).unwrap(), "sk-test");
    }

    /// Regression test: a request must only be routed to an endpoint that
    /// actually serves the requested model. With two models on two endpoints, a
    /// request for one must never be sent to the other's endpoint.
    #[tokio::test]
    async fn select_targets_filters_by_model() {
        let gw = Gateway::new(Config::default(), Arc::new(Metrics::new()));

        let m_glm = model("m-glm", "glm-5.2");
        let m_or = model("m-or", "openrouter/free");
        let e_glm = endpoint(
            "e-glm",
            "m-glm",
            "opencode",
            Some("https://opencode.ai/zen/go/v1"),
            1.0,
        );
        let e_or = endpoint(
            "e-or",
            "m-or",
            "openrouter",
            Some("https://openrouter.ai/api/v1"),
            1.0,
        );
        gw.rebuild_targets_from_db(&[m_glm, m_or], &[e_glm, e_or])
            .await;

        // `openrouter/free` resolves only to the openrouter endpoint.
        let req = ChatCompletionRequest {
            model: "openrouter/free".to_string(),
            ..Default::default()
        };
        let targets = gw.select_targets(&req, None).await.unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].provider, "openrouter");

        // An unknown model resolves to no target (clean 404 instead of routing
        // to an arbitrary endpoint that will reject it).
        let req = ChatCompletionRequest {
            model: "no/such-model".to_string(),
            ..Default::default()
        };
        let err = gw.select_targets(&req, None).await.unwrap_err();
        assert!(matches!(err, GatewayError::NotFound(_)));
    }

    /// Each enabled endpoint is its own routing target carrying the endpoint's
    /// weight. A model with two endpoints yields two eligible, distinctly-keyed
    /// targets; a second model with one endpoint yields one.
    #[tokio::test]
    async fn rebuild_emits_one_target_per_endpoint() {
        let gw = Gateway::new(Config::default(), Arc::new(Metrics::new()));

        let m_4o = model("m-4o", "gpt-4o");
        let m_mini = model("m-mini", "gpt-4o-mini");
        gw.rebuild_targets_from_db(
            &[m_4o, m_mini],
            &[
                endpoint(
                    "e1",
                    "m-4o",
                    "openai",
                    Some("https://api.openai.com/v1"),
                    5.0,
                ),
                // Same model served by a second provider endpoint.
                endpoint(
                    "e2",
                    "m-4o",
                    "openrouter",
                    Some("https://openrouter.ai/api/v1"),
                    2.0,
                ),
                endpoint(
                    "e3",
                    "m-mini",
                    "openai",
                    Some("https://api.openai.com/v1"),
                    1.0,
                ),
            ],
        )
        .await;

        // Three enabled endpoints → three targets, each single-model, weighted
        // and keyed from the endpoint. Asserted on the raw target list so the
        // check is independent of the active selection strategy.
        let targets = gw.targets.read().await.clone();
        assert_eq!(targets.len(), 3);
        let weight_of = |provider: &str, m: &str| {
            targets
                .iter()
                .find(|t| t.provider == provider && target_serves_model(t, m))
                .map(|t| t.weight)
        };
        assert_eq!(weight_of("openai", "gpt-4o"), Some(5.0));
        assert_eq!(weight_of("openrouter", "gpt-4o"), Some(2.0));
        assert_eq!(weight_of("openai", "gpt-4o-mini"), Some(1.0));

        // Each endpoint is keyed by its distinct id (two same-type openai
        // endpoints coexist without clobbering each other).
        let ids: Vec<&str> = targets.iter().filter_map(|t| t.id.as_deref()).collect();
        assert_eq!(ids.len(), 3);

        // "gpt-4o" is eligible on both endpoints; "gpt-4o-mini" on one.
        let eligible = |m: &str| targets.iter().filter(|t| target_serves_model(t, m)).count();
        assert_eq!(eligible("gpt-4o"), 2);
        assert_eq!(eligible("gpt-4o-mini"), 1);
    }

    /// A model with no enabled endpoint is inactive: it contributes no targets
    /// and requests for it 404.
    #[tokio::test]
    async fn model_without_enabled_endpoint_is_inactive() {
        let gw = Gateway::new(Config::default(), Arc::new(Metrics::new()));

        let m = model("m1", "lonely-model");
        let mut disabled = endpoint("e1", "m1", "openai", Some("https://api.openai.com/v1"), 1.0);
        disabled.enabled = false;
        gw.rebuild_targets_from_db(&[m], &[disabled]).await;

        assert!(gw.targets.read().await.is_empty());
        let req = ChatCompletionRequest {
            model: "lonely-model".to_string(),
            ..Default::default()
        };
        let err = gw.select_targets(&req, None).await.unwrap_err();
        assert!(matches!(err, GatewayError::NotFound(_)));
    }

    /// An unknown provider type needs an explicit `base_url`; with one it
    /// registers a generic Bearer client (keyed by endpoint id), without one it
    /// is skipped.
    #[tokio::test]
    async fn rebuild_handles_unknown_provider_types() {
        let gw = Gateway::new(Config::default(), Arc::new(Metrics::new()));

        let m = model("m1", "x");
        let with_url = endpoint(
            "ep-with",
            "m1",
            "custom-vendor",
            Some("https://api.custom.example/v1"),
            1.0,
        );
        let no_url = endpoint("ep-no", "m1", "mystery-vendor", None, 1.0);
        gw.rebuild_targets_from_db(&[m], &[with_url, no_url]).await;

        assert!(gw.get_provider("ep-with").is_some());
        assert!(gw.get_provider("ep-no").is_none());
    }

    /// Regression test for the ABBA deadlock between `reload_config`
    /// (strategy → config → targets) and `rebuild_targets_from_db` (which
    /// used to acquire targets → config). Hammer both concurrently; the
    /// timeout fails the test instead of hanging the suite if the
    /// deadlock ever comes back.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn config_reload_and_target_rebuild_do_not_deadlock() {
        let gw = Arc::new(Gateway::new(Config::default(), Arc::new(Metrics::new())));

        let reloader = {
            let gw = gw.clone();
            tokio::spawn(async move {
                for _ in 0..500 {
                    gw.reload_config(Config::default()).await.unwrap();
                }
            })
        };
        let rebuilder = {
            let gw = gw.clone();
            tokio::spawn(async move {
                let models = vec![model("m1", "gpt-4o"), model("m2", "claude")];
                let endpoints = vec![
                    endpoint("e1", "m1", "openai", None, 1.0),
                    endpoint("e2", "m2", "anthropic", None, 1.0),
                ];
                for _ in 0..500 {
                    gw.rebuild_targets_from_db(&models, &endpoints).await;
                }
            })
        };

        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            reloader.await.unwrap();
            rebuilder.await.unwrap();
        })
        .await
        .expect("deadlock: reload_config and rebuild_targets_from_db did not complete");
    }

    /// `build_provider_client` must construct a client for every entry in the
    /// shared registry without a `base_url` — the registry is what
    /// `/v1/models` and the admin UI trust, so a gap here means models list
    /// but 404 on completion.
    #[test]
    fn build_provider_client_agrees_with_known_provider_registry() {
        for t in himadri_core::KNOWN_PROVIDER_TYPES {
            assert!(
                build_provider_client(t, None).is_some(),
                "registry lists {t} but build_provider_client can't build it"
            );
        }
        assert!(build_provider_client("mystery-vendor", None).is_none());
        assert!(build_provider_client("mystery-vendor", Some("https://x.example/v1")).is_some());
    }

    /// Rebuild must never leave a window where a surviving endpoint's API key
    /// is missing (an in-flight request would send an empty Bearer token), and
    /// must drop keys and breaker state only for endpoints that went away.
    #[tokio::test]
    async fn rebuild_keeps_surviving_state_and_drops_stale() {
        let gw = Gateway::new(Config::default(), Arc::new(Metrics::new()));

        let m = model("m1", "gpt-4o");
        let ep1 = endpoint("ep1", "m1", "openai", None, 1.0);
        let ep2 = endpoint("ep2", "m1", "openai", None, 1.0);
        gw.rebuild_targets_from_db(std::slice::from_ref(&m), &[ep1.clone(), ep2])
            .await;

        // Simulate accumulated breaker state for both endpoints.
        for id in ["ep1", "ep2"] {
            gw.circuit_breakers
                .entry(id.to_string())
                .or_insert_with(|| Arc::new(CircuitBreaker::new(CircuitBreakerConfig::default())));
        }

        // ep2 is deleted; rebuild with only ep1.
        gw.rebuild_targets_from_db(&[m], &[ep1]).await;

        let target = Target {
            id: Some("ep1".to_string()),
            provider: "openai".to_string(),
            weight: 1.0,
            models: None,
            api_key_env: None,
            base_url: None,
        };
        assert_eq!(gw.get_api_key(&target).unwrap(), "sk-test");
        assert!(gw.provider_keys.get("ep2").is_none(), "stale key kept");
        assert!(
            gw.circuit_breakers.contains_key("ep1"),
            "healthy breaker reset"
        );
        assert!(
            !gw.circuit_breakers.contains_key("ep2"),
            "stale breaker kept"
        );
    }

    /// The startup / config-reassert guard: only an enabled model with an
    /// enabled endpoint counts as an active DB target. Anything less must not
    /// trigger a rebuild that would wipe config/env targets.
    #[test]
    fn db_has_active_targets_requires_enabled_model_and_endpoint() {
        use std::slice::from_ref;

        let m = model("m1", "gpt-4o");
        let ep = endpoint("e1", "m1", "openai", None, 1.0);
        assert!(Gateway::db_has_active_targets(from_ref(&m), from_ref(&ep)));

        let mut disabled_ep = ep.clone();
        disabled_ep.enabled = false;
        assert!(!Gateway::db_has_active_targets(
            from_ref(&m),
            &[disabled_ep]
        ));

        let mut disabled_model = m.clone();
        disabled_model.enabled = false;
        assert!(!Gateway::db_has_active_targets(
            &[disabled_model],
            from_ref(&ep)
        ));

        assert!(!Gateway::db_has_active_targets(&[m], &[]));
    }
}
