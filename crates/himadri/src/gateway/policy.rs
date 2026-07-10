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
        let Some(org_id) = auth_ctx.org_id.as_deref() else {
            return Ok(());
        };
        let Some(org_config) = config.orgs.get(org_id) else {
            return Ok(());
        };

        // Org- then team-level allow/block lists for the requested model.
        let team_config = auth_ctx
            .team_id
            .as_deref()
            .and_then(|team_id| org_config.teams.get(team_id).map(|c| (team_id, c)));
        let mut model_rules = vec![(
            "org",
            org_id,
            org_config.allowed_models.as_ref(),
            org_config.blocked_models.as_ref(),
        )];
        if let Some((team_id, team)) = &team_config {
            model_rules.push((
                "team",
                team_id,
                team.allowed_models.as_ref(),
                team.blocked_models.as_ref(),
            ));
        }
        for (scope, scope_id, allowed, blocked) in model_rules {
            if allowed.is_some_and(|list| !list.contains(&request.model)) {
                return Err(GatewayError::Forbidden(format!(
                    "Model '{}' not allowed for {} '{}'",
                    request.model, scope, scope_id
                )));
            }
            if blocked.is_some_and(|list| list.contains(&request.model)) {
                return Err(GatewayError::Forbidden(format!(
                    "Model '{}' blocked for {} '{}'",
                    request.model, scope, scope_id
                )));
            }
        }

        if org_config.guardrails.enabled {
            // Blocked-word scan: lowercase the configured words once, not
            // once per message.
            if !org_config.guardrails.blocked_words.is_empty() {
                let blocked_lower: Vec<String> = org_config
                    .guardrails
                    .blocked_words
                    .iter()
                    .map(|w| w.to_lowercase())
                    .collect();
                for message in &request.messages {
                    let Some(content) = &message.content else {
                        continue;
                    };
                    let lower_text = content.flat_text().to_lowercase();
                    if let Some(word) = blocked_lower.iter().find(|w| lower_text.contains(*w)) {
                        return Err(GatewayError::Forbidden(format!(
                            "Blocked word '{}' detected in request",
                            word
                        )));
                    }
                }
            }

            if let (Some(max), Some(requested)) = (
                org_config.guardrails.max_tokens_per_request,
                request.max_tokens,
            ) {
                if requested > max {
                    return Err(GatewayError::Forbidden(format!(
                        "max_tokens {} exceeds org guardrail limit of {}",
                        requested, max
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
        let Some(org_config) = auth_ctx
            .org_id
            .as_deref()
            .and_then(|org_id| config.orgs.get(org_id))
        else {
            return Ok(());
        };

        // Per-request token caps at org then team scope.
        let team_budget = auth_ctx
            .team_id
            .as_deref()
            .and_then(|team_id| org_config.teams.get(team_id))
            .and_then(|team| team.token_budget.as_ref());
        let caps = [
            ("org", org_config.token_budget.as_ref()),
            ("team", team_budget),
        ];
        for (scope, budget) in caps {
            let Some(max) = budget.and_then(|b| b.max_tokens_per_request) else {
                continue;
            };
            if requested > max {
                return Err(GatewayError::Forbidden(format!(
                    "max_tokens {} exceeds {} limit of {}",
                    requested, scope, max
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
