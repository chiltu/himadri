//! Rebuilding routing targets from DB models and their endpoints — the
//! single bridge between the admin tables and live routing state.

use tracing::warn;

use himadri_core::Target;

use super::Gateway;

/// What a rebuild should do when it computes zero routing targets.
///
/// The right answer depends on who is asking. After an admin mutation the DB is
/// the authority — the operator just edited it, and disabling the last endpoint
/// legitimately empties routing (`Apply`). At startup and after a config apply
/// the DB only takes over when it actually produces targets; an empty result
/// must not replace the env/file-configured targets (`KeepPrevious`), or a DB
/// holding only disabled or unbuildable rows silently causes a full outage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnEmpty {
    /// Apply the empty result: routing genuinely has no DB targets.
    Apply,
    /// Leave all live state untouched and report `applied: false`.
    KeepPrevious,
}

/// One endpoint a rebuild could not turn into a routing target.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SkippedEndpoint {
    pub endpoint_id: String,
    pub provider_type: String,
    /// The provider registry's error, rendered for logs and admin responses.
    pub reason: String,
}

/// What a rebuild actually did — the fact callers decide from, replacing the
/// old `db_has_active_targets` prediction (which re-derived "will the DB
/// produce targets" from enabled flags alone and could disagree with the
/// rebuild's registry check, wiping live routing when it did).
#[derive(Debug, Clone, serde::Serialize)]
pub struct RebuildOutcome {
    /// Routing targets computed (and applied, unless `applied` is false).
    pub targets_built: usize,
    /// Enabled endpoints the provider registry could not build a client for.
    pub skipped: Vec<SkippedEndpoint>,
    /// Whether live routing state was replaced; false only for an empty
    /// result under [`OnEmpty::KeepPrevious`].
    pub applied: bool,
}

impl Gateway {
    /// Rebuild routing targets from database models and their endpoints.
    /// Called when a model or endpoint is created, updated, deleted, or toggled.
    ///
    /// Each enabled endpoint of each enabled model becomes one routing target,
    /// keyed by the endpoint id so the same provider type can back several
    /// endpoints with distinct credentials/URLs. A model with no enabled
    /// endpoint contributes no targets and is therefore inactive/unroutable.
    ///
    /// `on_empty` decides whether an empty result replaces live routing state;
    /// see [`OnEmpty`]. The returned [`RebuildOutcome`] reports what was built,
    /// what was skipped and why, and whether it was applied.
    pub async fn rebuild_targets_from_db(
        &self,
        models: &[himadri_admin::Model],
        endpoints: &[himadri_admin::ModelEndpoint],
        on_empty: OnEmpty,
    ) -> RebuildOutcome {
        // Build the new target list and key set before taking any locks or
        // touching the live maps, so in-flight requests keep resolving
        // against the previous state until the swap below.
        let mut new_targets = Vec::new();
        let mut new_keys: Vec<(String, String)> = Vec::new();
        let mut skipped = Vec::new();

        for model in models {
            if !model.enabled {
                continue;
            }

            for endpoint in endpoints
                .iter()
                .filter(|e| e.model_id == model.id && e.enabled)
            {
                // Register a client under the endpoint id so the target resolves
                // at request time. Skip endpoints the registry can't build (an
                // unregistered provider type with no base_url has nowhere to go).
                // The admin API rejects these at creation, so reaching this arm
                // means the row predates that check or was written directly.
                let base_url = endpoint.base_url.as_deref();
                let client = match self.provider_registry.build(&endpoint.provider_type, base_url) {
                    Ok(client) => client,
                    Err(e) => {
                        warn!(
                            endpoint = %endpoint.id,
                            provider_type = %endpoint.provider_type,
                            error = %e,
                            "skipping endpoint: cannot build a provider client"
                        );
                        skipped.push(SkippedEndpoint {
                            endpoint_id: endpoint.id.clone(),
                            provider_type: endpoint.provider_type.clone(),
                            reason: e.to_string(),
                        });
                        continue;
                    }
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

        // Keep-previous exit — this must sit before ANY live-state mutation
        // below. In particular the provider_keys retain: running it with an
        // empty key set would wipe every live credential while the previous
        // targets keep routing, sending empty Bearer tokens upstream. At this
        // point nothing has been touched: client registration above only
        // happens for endpoints that build, and an empty result built none.
        if new_targets.is_empty() && on_empty == OnEmpty::KeepPrevious {
            return RebuildOutcome {
                targets_built: 0,
                skipped,
                applied: false,
            };
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

        let targets_built = new_targets.len();
        let active_ids: std::collections::HashSet<String> =
            new_targets.iter().filter_map(|t| t.id.clone()).collect();

        // Lock order: config before targets (see the field docs on `Gateway`).
        let mut config = self.config.write().await;
        let mut targets = self.targets.write().await;
        // The endpoint ids that were routing until now, so their clients can be
        // pruned below.
        let previous_ids: Vec<String> = targets.iter().filter_map(|t| t.id.clone()).collect();
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

        // Drop the clients of endpoints that no longer route, or they
        // accumulate for the process's life (one per rotated endpoint) and a
        // deleted endpoint id stays resolvable. Pruned by *previous* target id
        // rather than retained on `active_ids`: `providers` also holds the
        // env-registered clients, keyed by provider name, and a retain would
        // wipe the very fallback Auto mode depends on.
        for stale in previous_ids.iter().filter(|id| !active_ids.contains(*id)) {
            self.providers.remove(stale);
        }

        RebuildOutcome {
            targets_built,
            skipped,
            applied: true,
        }
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

    /// Accepts exactly one provider type and rejects everything else, so a test
    /// can tell an injected registry apart from the default one.
    struct StubRegistry;

    impl himadri_provider::ProviderRegistry for StubRegistry {
        fn build(
            &self,
            provider_type: &str,
            _base_url: Option<&str>,
        ) -> Result<Arc<dyn himadri_provider::Provider>, himadri_provider::ProviderError> {
            if provider_type == "stub-ok" {
                Ok(Arc::new(himadri_provider::OpenAiCompatibleProvider::bearer(
                    "stub-ok",
                    "https://stub.example/v1",
                )))
            } else {
                Err(himadri_provider::ProviderError::UnknownType(
                    provider_type.to_string(),
                ))
            }
        }
    }

    /// Rebuild must go through the injected registry, not a hardcoded factory —
    /// this is what lets it be tested without real vendor presets. `openai` is a
    /// type the default registry builds happily but the stub rejects, so it
    /// fails if injection is ignored.
    #[tokio::test]
    async fn rebuild_builds_clients_through_the_injected_registry() {
        let mut gw = Gateway::new(Config::default(), Arc::new(Metrics::new()));
        gw.set_provider_registry(Arc::new(StubRegistry));

        let m = model("m1", "x");
        let rejected = endpoint("ep-openai", "m1", "openai", None, 1.0);
        let accepted = endpoint("ep-stub", "m1", "stub-ok", None, 1.0);
        gw.rebuild_targets_from_db(&[m], &[rejected, accepted], OnEmpty::Apply)
            .await;

        assert!(
            gw.get_provider("ep-stub").is_some(),
            "the injected registry accepted stub-ok, so its client must be registered"
        );
        assert!(
            gw.get_provider("ep-openai").is_none(),
            "openai was built despite the stub rejecting it: rebuild ignored the injected registry"
        );
    }

    /// Clients of endpoints that stop routing must be dropped, or a
    /// long-running gateway accumulates one per rotated endpoint for the life
    /// of the process and a deleted endpoint id stays resolvable. The
    /// env-registered providers — keyed by provider *name*, not endpoint id —
    /// must survive: they are the fallback Auto mode routes to when the DB
    /// stops producing targets.
    #[tokio::test]
    async fn rebuild_drops_clients_of_endpoints_that_no_longer_route() {
        let gw = Gateway::new(Config::default(), Arc::new(Metrics::new()));
        gw.register_provider(Arc::new(
            himadri_provider::OpenAiCompatibleProvider::bearer("env-openai", "https://api.example/v1"),
        ));

        let m = model("m1", "x");
        let ep1 = endpoint("ep1", "m1", "openai", None, 1.0);
        let ep2 = endpoint("ep2", "m1", "groq", None, 1.0);
        gw.rebuild_targets_from_db(
            std::slice::from_ref(&m),
            &[ep1.clone(), ep2],
            OnEmpty::Apply,
        )
        .await;
        assert!(gw.get_provider("ep1").is_some());
        assert!(gw.get_provider("ep2").is_some());

        // ep2 is deleted; ep1 survives.
        gw.rebuild_targets_from_db(&[m], &[ep1], OnEmpty::Apply).await;

        assert!(
            gw.get_provider("ep1").is_some(),
            "a surviving endpoint must keep its client"
        );
        assert!(
            gw.get_provider("ep2").is_none(),
            "a removed endpoint's client must be dropped, not leaked"
        );
        assert!(
            gw.get_provider("env-openai").is_some(),
            "env-registered providers must survive a DB rebuild — they are Auto mode's fallback"
        );
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
        gw.rebuild_targets_from_db(&[m], &[ep], OnEmpty::Apply).await;

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
        gw.rebuild_targets_from_db(&[m_glm, m_or], &[e_glm, e_or], OnEmpty::Apply)
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
            OnEmpty::Apply,
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
        gw.rebuild_targets_from_db(&[m], &[disabled], OnEmpty::Apply).await;

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
        gw.rebuild_targets_from_db(&[m], &[with_url, no_url], OnEmpty::Apply)
            .await;

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
                    gw.rebuild_targets_from_db(&models, &endpoints, OnEmpty::Apply).await;
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

    /// Rebuild must never leave a window where a surviving endpoint's API key
    /// is missing (an in-flight request would send an empty Bearer token), and
    /// must drop keys and breaker state only for endpoints that went away.
    #[tokio::test]
    async fn rebuild_keeps_surviving_state_and_drops_stale() {
        let gw = Gateway::new(Config::default(), Arc::new(Metrics::new()));

        let m = model("m1", "gpt-4o");
        let ep1 = endpoint("ep1", "m1", "openai", None, 1.0);
        let ep2 = endpoint("ep2", "m1", "openai", None, 1.0);
        gw.rebuild_targets_from_db(std::slice::from_ref(&m), &[ep1.clone(), ep2], OnEmpty::Apply)
            .await;

        // Simulate accumulated breaker state for both endpoints.
        for id in ["ep1", "ep2"] {
            gw.circuit_breakers
                .entry(id.to_string())
                .or_insert_with(|| Arc::new(CircuitBreaker::new(CircuitBreakerConfig::default())));
        }

        // ep2 is deleted; rebuild with only ep1.
        gw.rebuild_targets_from_db(&[m], &[ep1], OnEmpty::Apply).await;

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

    /// The outage regression `db_has_active_targets` could not catch: enabled
    /// rows exist, so the old guard would approve a rebuild — but every
    /// endpoint is unbuildable, so the rebuild computes zero targets and, if
    /// applied, wipes live routing. With `KeepPrevious` the empty result must
    /// leave targets, provider keys, and clients untouched, and the outcome
    /// must say what happened. The key assertion also pins the early-return
    /// placement: if the keep-previous exit ever moves below the
    /// `provider_keys` retain, the seeded key is wiped and this fails.
    #[tokio::test]
    async fn empty_rebuild_with_keep_previous_preserves_live_state() {
        let mut gw = Gateway::new(Config::default(), Arc::new(Metrics::new()));
        gw.set_provider_registry(Arc::new(StubRegistry));

        // Seed live state through a rebuild that produces a target; using
        // KeepPrevious here also pins that a nonzero result always applies.
        let m = model("m1", "x");
        let good = endpoint("ep-good", "m1", "stub-ok", None, 1.0);
        let outcome = gw
            .rebuild_targets_from_db(
                std::slice::from_ref(&m),
                &[good],
                OnEmpty::KeepPrevious,
            )
            .await;
        assert!(outcome.applied, "a nonzero rebuild must apply under KeepPrevious");
        assert_eq!(outcome.targets_built, 1);

        let live_target = Target {
            id: Some("ep-good".to_string()),
            provider: "stub-ok".to_string(),
            weight: 1.0,
            models: None,
            api_key_env: None,
            base_url: None,
        };
        assert_eq!(gw.get_api_key(&live_target).unwrap(), "sk-test");

        // The DB now holds only rows the registry can't build (a legacy row,
        // or one written directly to the DB, past the admin-API validation).
        let bad = endpoint("ep-bad", "m1", "openai", None, 1.0); // stub rejects "openai"
        let outcome = gw
            .rebuild_targets_from_db(&[m], &[bad], OnEmpty::KeepPrevious)
            .await;

        assert!(!outcome.applied);
        assert_eq!(outcome.targets_built, 0);
        assert_eq!(outcome.skipped.len(), 1);
        assert_eq!(outcome.skipped[0].endpoint_id, "ep-bad");
        assert_eq!(outcome.skipped[0].provider_type, "openai");
        assert!(
            outcome.skipped[0].reason.contains("openai"),
            "reason should carry the registry error, got: {}",
            outcome.skipped[0].reason
        );

        // Live state fully intact: target still routes, its key survived
        // (the retain hazard), and its client is still registered.
        let targets = gw.targets.read().await.clone();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].id.as_deref(), Some("ep-good"));
        assert_eq!(gw.get_api_key(&live_target).unwrap(), "sk-test");
        assert!(gw.get_provider("ep-good").is_some());
    }

    /// Successor to the deleted `db_has_active_targets` test, asserted through
    /// the real computation: a DB with disabled rows (or none) produces no
    /// targets, and under `KeepPrevious` the config/env targets must stand.
    #[tokio::test]
    async fn disabled_rows_with_keep_previous_do_not_wipe_targets() {
        let gw = Gateway::new(Config::default(), Arc::new(Metrics::new()));
        let config_targets = gw.targets.read().await.len();
        assert!(config_targets > 0, "default config must supply a target");

        let m = model("m1", "gpt-4o");
        let ep = endpoint("e1", "m1", "openai", None, 1.0);

        let mut disabled_ep = ep.clone();
        disabled_ep.enabled = false;
        let mut disabled_model = m.clone();
        disabled_model.enabled = false;

        for (models, endpoints) in [
            (vec![m.clone()], vec![disabled_ep]),         // endpoint disabled
            (vec![disabled_model], vec![ep]),             // model disabled
            (vec![m], vec![]),                            // no endpoint rows
            (vec![], vec![]),                             // empty DB
        ] {
            let outcome = gw
                .rebuild_targets_from_db(&models, &endpoints, OnEmpty::KeepPrevious)
                .await;
            assert!(!outcome.applied);
            assert!(outcome.skipped.is_empty(), "disabled rows are filtered, not skipped");
            assert_eq!(
                gw.targets.read().await.len(),
                config_targets,
                "config targets must survive an empty rebuild"
            );
        }
    }
}
