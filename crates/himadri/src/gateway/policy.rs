//! Pre-flight request policy: rate limits, token budgets, org guardrails,
//! and RBAC. Shared by the streaming and non-streaming paths via
//! `prepare_request`.

use himadri_core::{AuthContext, AuthScope, ChatCompletionRequest, Config, GatewayError, Target};

use super::Gateway;

impl Gateway {
    /// Enforce role-based model access. Admin-scope principals bypass RBAC.
    /// A principal whose roles grant no access is rejected with `403`.
    pub(super) fn check_rbac_model(
        &self,
        auth: Option<&AuthContext>,
        config: &Config,
        model: &str,
    ) -> Result<(), GatewayError> {
        if !config.rbac.enabled {
            return Ok(());
        }
        let (roles, is_admin): (&[String], bool) = match auth {
            Some(ctx) => (&ctx.roles, ctx.scope == AuthScope::Admin),
            None => (&[], false),
        };
        config
            .rbac
            .check_model(roles, is_admin, model)
            .map_err(|d| GatewayError::Forbidden(d.to_string()))
    }

    /// Retain only the targets whose provider the principal's roles permit,
    /// preserving priority order. Errors with `403` if RBAC leaves no target.
    pub(super) async fn filter_targets_by_rbac(
        &self,
        auth: Option<&AuthContext>,
        ordered: Vec<Target>,
    ) -> Result<Vec<Target>, GatewayError> {
        let config = self.config.read().await;
        if !config.rbac.enabled {
            return Ok(ordered);
        }
        let (roles, is_admin): (&[String], bool) = match auth {
            Some(ctx) => (&ctx.roles, ctx.scope == AuthScope::Admin),
            None => (&[], false),
        };
        if is_admin {
            return Ok(ordered);
        }

        let mut last_denial: Option<himadri_core::RbacDenial> = None;
        let allowed: Vec<Target> = ordered
            .into_iter()
            .filter(
                |t| match config.rbac.check_provider(roles, is_admin, &t.provider) {
                    Ok(()) => true,
                    Err(d) => {
                        last_denial = Some(d);
                        false
                    }
                },
            )
            .collect();

        if allowed.is_empty() {
            let reason = last_denial
                .map(|d| d.to_string())
                .unwrap_or_else(|| "no permitted provider for your role".to_string());
            return Err(GatewayError::Forbidden(reason));
        }
        Ok(allowed)
    }

    pub(super) fn check_org_guardrails(
        &self,
        auth: Option<&AuthContext>,
        config: &Config,
        request: &ChatCompletionRequest,
    ) -> Result<(), GatewayError> {
        let Some(auth_ctx) = auth else {
            return Ok(());
        };
        // One walk for every scoped check (see himadri_core::scope). All rules
        // here are cumulative: each scope that states one enforces it — "team
        // config narrows org config".
        let scopes = config.scopes(auth_ctx.org_id.as_deref(), auth_ctx.team_id.as_deref());

        // Allow/block lists for the requested model.
        for scope in &scopes {
            if scope
                .allowed_models
                .is_some_and(|list| !list.contains(&request.model))
            {
                return Err(GatewayError::Forbidden(format!(
                    "Model '{}' not allowed for {} '{}'",
                    request.model,
                    scope.kind.as_str(),
                    scope.id
                )));
            }
            if scope
                .blocked_models
                .is_some_and(|list| list.contains(&request.model))
            {
                return Err(GatewayError::Forbidden(format!(
                    "Model '{}' blocked for {} '{}'",
                    request.model,
                    scope.kind.as_str(),
                    scope.id
                )));
            }
        }

        // Message bodies lowercased once for the whole request, not once per
        // scope — flattening and lowercasing every prompt is the expensive part
        // here, and it does not vary by scope. Built lazily: most requests
        // configure no blocked words at all and never pay for it.
        let mut lowered_messages: Option<Vec<String>> = None;

        // Word/token guardrails. Each scope's `guardrails.enabled` gates only
        // that scope's own contribution: an org with guardrails off does not
        // switch off a team's, and vice versa (same self-contained-section
        // rule the per-scope PII override uses).
        for scope in &scopes {
            if !scope.guardrails.enabled {
                continue;
            }

            // Blocked-word scan: lowercase the configured words once, not
            // once per message.
            if !scope.guardrails.blocked_words.is_empty() {
                let messages = lowered_messages.get_or_insert_with(|| {
                    request
                        .messages
                        .iter()
                        .filter_map(|m| m.content.as_ref())
                        .map(|content| content.flat_text().to_lowercase())
                        .collect()
                });
                let blocked_lower: Vec<String> = scope
                    .guardrails
                    .blocked_words
                    .iter()
                    .map(|w| w.to_lowercase())
                    .collect();
                for lower_text in messages.iter() {
                    if let Some(word) = blocked_lower.iter().find(|w| lower_text.contains(*w)) {
                        return Err(GatewayError::Forbidden(format!(
                            "Blocked word '{}' detected in request",
                            word
                        )));
                    }
                }
            }

            if let (Some(max), Some(requested)) =
                (scope.guardrails.max_tokens_per_request, request.max_tokens)
            {
                if requested > max {
                    return Err(GatewayError::Forbidden(format!(
                        "max_tokens {} exceeds {} guardrail limit of {}",
                        requested,
                        scope.kind.as_str(),
                        max
                    )));
                }
            }
        }

        Ok(())
    }

    pub(super) fn check_token_budgets(
        &self,
        auth: Option<&AuthContext>,
        config: &Config,
        request: &ChatCompletionRequest,
    ) -> Result<(), GatewayError> {
        let Some(requested) = request.max_tokens else {
            return Ok(());
        };
        let Some(auth_ctx) = auth else {
            return Ok(());
        };

        // Per-request token caps, cumulative across the scope chain.
        for scope in config.scopes(auth_ctx.org_id.as_deref(), auth_ctx.team_id.as_deref()) {
            let Some(max) = scope.token_budget.and_then(|b| b.max_tokens_per_request) else {
                continue;
            };
            if requested > max {
                return Err(GatewayError::Forbidden(format!(
                    "max_tokens {} exceeds {} limit of {}",
                    requested,
                    scope.kind.as_str(),
                    max
                )));
            }
        }
        Ok(())
    }

    pub(super) fn check_rate_limits(
        &self,
        auth: Option<&AuthContext>,
        config: &Config,
    ) -> Result<(), GatewayError> {
        if !config.rate_limit.enabled {
            return Ok(());
        }

        // Global rate limit
        if !self.rate_limiter.check_global() {
            return Err(GatewayError::RateLimited {
                retry_after_secs: 1,
            });
        }

        if let Some(auth_ctx) = auth {
            // Per-key rate limit (uses override from API key if set)
            if let Some(ref key_id) = auth_ctx.key_id {
                let (rate, burst) = match &auth_ctx.rate_limit_override {
                    Some(override_cfg) => {
                        (override_cfg.requests_per_second, override_cfg.burst_size)
                    }
                    None => (None, None),
                };
                if !self.rate_limiter.check_key(key_id, rate, burst) {
                    return Err(GatewayError::RateLimited {
                        retry_after_secs: 1,
                    });
                }
            }

            // Per-org rate limit
            if let Some(ref org_id) = auth_ctx.org_id {
                if let Some(org_config) = config.orgs.get(org_id) {
                    if let Some(ref org_rate_limit) = org_config.rate_limit {
                        if org_rate_limit.enabled {
                            let rate = org_rate_limit.requests_per_second;
                            let burst = org_rate_limit.burst_size;
                            if !self.rate_limiter.check_org(org_id, Some(rate), Some(burst)) {
                                return Err(GatewayError::RateLimited {
                                    retry_after_secs: 1,
                                });
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use himadri_core::{Message, OrgConfig, OrgGuardrailConfig, OrgTokenBudget, TeamConfig};
    use himadri_observability::Metrics;
    use std::sync::Arc;

    fn gateway() -> Gateway {
        Gateway::new(Config::default(), Arc::new(Metrics::new()))
    }

    fn auth(org: Option<&str>, team: Option<&str>) -> AuthContext {
        AuthContext {
            org_id: org.map(str::to_string),
            team_id: team.map(str::to_string),
            ..Default::default()
        }
    }

    fn request(model: &str, text: &str, max_tokens: Option<u32>) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: model.to_string(),
            messages: vec![Message::user(text)],
            max_tokens,
            ..Default::default()
        }
    }

    /// A config with one org ("acme") and one team ("research") under it.
    fn config(org: OrgConfig) -> Config {
        let mut cfg = Config::default();
        cfg.orgs.insert("acme".to_string(), org);
        cfg
    }

    fn org_with_team(mut org: OrgConfig, team: TeamConfig) -> OrgConfig {
        org.teams.insert("research".to_string(), team);
        org
    }

    // --- Characterization: behavior that must survive the scope-chain refactor ---

    #[tokio::test]
    async fn org_blocked_word_rejects_case_insensitively() {
        let cfg = config(OrgConfig {
            guardrails: OrgGuardrailConfig {
                enabled: true,
                blocked_words: vec!["Secret-Project-X".to_string()],
                ..Default::default()
            },
            ..Default::default()
        });
        let gw = gateway();

        let err = gw
            .check_org_guardrails(
                Some(&auth(Some("acme"), None)),
                &cfg,
                &request("gpt-4o", "tell me about sEcReT-pRoJeCt-x please", None),
            )
            .expect_err("blocked word must reject");
        assert!(err.to_string().to_lowercase().contains("secret-project-x"));

        assert!(gw
            .check_org_guardrails(
                Some(&auth(Some("acme"), None)),
                &cfg,
                &request("gpt-4o", "an innocuous request", None),
            )
            .is_ok());
    }

    #[tokio::test]
    async fn org_max_tokens_guardrail_enforces_cap() {
        let cfg = config(OrgConfig {
            guardrails: OrgGuardrailConfig {
                enabled: true,
                max_tokens_per_request: Some(1000),
                ..Default::default()
            },
            ..Default::default()
        });
        let gw = gateway();
        let a = auth(Some("acme"), None);

        assert!(gw
            .check_org_guardrails(Some(&a), &cfg, &request("m", "hi", Some(1001)))
            .is_err());
        assert!(gw
            .check_org_guardrails(Some(&a), &cfg, &request("m", "hi", Some(1000)))
            .is_ok());
        // No max_tokens on the request: nothing to compare.
        assert!(gw
            .check_org_guardrails(Some(&a), &cfg, &request("m", "hi", None))
            .is_ok());
    }

    #[tokio::test]
    async fn disabled_org_guardrails_enforce_nothing() {
        let cfg = config(OrgConfig {
            guardrails: OrgGuardrailConfig {
                enabled: false,
                blocked_words: vec!["forbidden".to_string()],
                max_tokens_per_request: Some(1),
                ..Default::default()
            },
            ..Default::default()
        });
        assert!(gateway()
            .check_org_guardrails(
                Some(&auth(Some("acme"), None)),
                &cfg,
                &request("m", "forbidden text", Some(9999)),
            )
            .is_ok());
    }

    #[tokio::test]
    async fn model_rules_enforce_at_both_scopes_and_name_the_scope() {
        let cfg = config(org_with_team(
            OrgConfig {
                allowed_models: Some(vec!["gpt-4o".to_string(), "gpt-4o-mini".to_string()]),
                ..Default::default()
            },
            TeamConfig {
                blocked_models: Some(vec!["gpt-4o".to_string()]),
                ..Default::default()
            },
        ));
        let gw = gateway();
        let a = auth(Some("acme"), Some("research"));

        // Not on the org allow-list -> org rejects, error names the org scope.
        let err = gw
            .check_org_guardrails(Some(&a), &cfg, &request("claude-3", "hi", None))
            .expect_err("org allow-list must reject");
        assert!(err.to_string().contains("org 'acme'"), "got: {err}");

        // On the org allow-list but team-blocked -> team rejects.
        let err = gw
            .check_org_guardrails(Some(&a), &cfg, &request("gpt-4o", "hi", None))
            .expect_err("team block-list must reject");
        assert!(err.to_string().contains("team 'research'"), "got: {err}");

        // Allowed by org, not blocked by team.
        assert!(gw
            .check_org_guardrails(Some(&a), &cfg, &request("gpt-4o-mini", "hi", None))
            .is_ok());
    }

    #[tokio::test]
    async fn missing_auth_org_or_config_checks_nothing() {
        let cfg = config(OrgConfig {
            guardrails: OrgGuardrailConfig {
                enabled: true,
                blocked_words: vec!["forbidden".to_string()],
                ..Default::default()
            },
            ..Default::default()
        });
        let gw = gateway();
        let req = request("m", "forbidden", None);

        assert!(gw.check_org_guardrails(None, &cfg, &req).is_ok());
        assert!(gw
            .check_org_guardrails(Some(&auth(None, None)), &cfg, &req)
            .is_ok());
        assert!(gw
            .check_org_guardrails(Some(&auth(Some("unknown-org"), None)), &cfg, &req)
            .is_ok());
    }

    // --- New behavior (2026-07-15): team guardrails enforce, per-scope gated.
    // The schema and docs ("team config narrows org config") always promised
    // this; the old walk read only the org section.

    #[tokio::test]
    async fn team_blocked_word_and_max_tokens_enforce() {
        let cfg = config(org_with_team(
            OrgConfig::default(), // org states no guardrails at all
            TeamConfig {
                guardrails: OrgGuardrailConfig {
                    enabled: true,
                    blocked_words: vec!["classified".to_string()],
                    max_tokens_per_request: Some(500),
                    ..Default::default()
                },
                ..Default::default()
            },
        ));
        let gw = gateway();
        let a = auth(Some("acme"), Some("research"));

        let err = gw
            .check_org_guardrails(Some(&a), &cfg, &request("m", "this is classified", None))
            .expect_err("team blocked word must reject");
        assert!(err.to_string().contains("classified"));

        let err = gw
            .check_org_guardrails(Some(&a), &cfg, &request("m", "hi", Some(501)))
            .expect_err("team max_tokens cap must reject");
        assert!(err.to_string().contains("team guardrail limit"), "got: {err}");

        // A principal on the org without the team is untouched by team rules.
        assert!(gw
            .check_org_guardrails(
                Some(&auth(Some("acme"), None)),
                &cfg,
                &request("m", "this is classified", Some(501)),
            )
            .is_ok());
    }

    #[tokio::test]
    async fn scope_enabled_gates_only_that_scopes_guardrails() {
        // Org guardrails disabled, team enabled: team's word still enforces,
        // org's does not — each scope's section is self-contained.
        let cfg = config(org_with_team(
            OrgConfig {
                guardrails: OrgGuardrailConfig {
                    enabled: false,
                    blocked_words: vec!["org-word".to_string()],
                    ..Default::default()
                },
                ..Default::default()
            },
            TeamConfig {
                guardrails: OrgGuardrailConfig {
                    enabled: true,
                    blocked_words: vec!["team-word".to_string()],
                    ..Default::default()
                },
                ..Default::default()
            },
        ));
        let gw = gateway();
        let a = auth(Some("acme"), Some("research"));

        assert!(gw
            .check_org_guardrails(Some(&a), &cfg, &request("m", "org-word here", None))
            .is_ok());
        assert!(gw
            .check_org_guardrails(Some(&a), &cfg, &request("m", "team-word here", None))
            .is_err());

        // And the mirror: team disabled leaves only the org's words active.
        let cfg = config(org_with_team(
            OrgConfig {
                guardrails: OrgGuardrailConfig {
                    enabled: true,
                    blocked_words: vec!["org-word".to_string()],
                    ..Default::default()
                },
                ..Default::default()
            },
            TeamConfig {
                guardrails: OrgGuardrailConfig {
                    enabled: false,
                    blocked_words: vec!["team-word".to_string()],
                    ..Default::default()
                },
                ..Default::default()
            },
        ));
        assert!(gw
            .check_org_guardrails(Some(&a), &cfg, &request("m", "team-word here", None))
            .is_ok());
        assert!(gw
            .check_org_guardrails(Some(&a), &cfg, &request("m", "org-word here", None))
            .is_err());
    }

    #[tokio::test]
    async fn token_budgets_enforce_org_and_team_caps_with_scope_in_error() {
        let cfg = config(org_with_team(
            OrgConfig {
                token_budget: Some(OrgTokenBudget {
                    max_tokens_per_request: Some(8192),
                    ..Default::default()
                }),
                ..Default::default()
            },
            TeamConfig {
                token_budget: Some(OrgTokenBudget {
                    max_tokens_per_request: Some(4096),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ));
        let gw = gateway();
        let a = auth(Some("acme"), Some("research"));

        let err = gw
            .check_token_budgets(Some(&a), &cfg, &request("m", "hi", Some(9000)))
            .expect_err("org cap must reject");
        assert!(err.to_string().contains("org"), "got: {err}");

        let err = gw
            .check_token_budgets(Some(&a), &cfg, &request("m", "hi", Some(5000)))
            .expect_err("team cap must reject");
        assert!(err.to_string().contains("team"), "got: {err}");

        assert!(gw
            .check_token_budgets(Some(&a), &cfg, &request("m", "hi", Some(4096)))
            .is_ok());
    }
}
