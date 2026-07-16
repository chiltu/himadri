//! Scope resolution: the single walk from a request's `(org_id, team_id)` to
//! the ordered chain of config scopes that apply to it.
//!
//! Four checks used to walk `config.orgs → org.teams` independently (model
//! rules, token budgets, org guardrails, and the PII plugin's per-scope
//! override), which let their precedence semantics drift. The walk now lives
//! here once; each consumer keeps its own *combination* rule, applied over the
//! same chain:
//!
//! - **Cumulative** (model rules, token budgets, blocked words, max_tokens):
//!   iterate the chain in order; every scope that states a rule enforces it.
//! - **Wholesale override** (PII): the most specific scope with a section wins
//!   entirely — `scopes.iter().rev().find_map(...)` — including disabling a
//!   global policy.

use crate::config::{Config, OrgGuardrailConfig, OrgTokenBudget};

/// Which level of the org hierarchy a [`Scope`] came from. Renders lowercase
/// in error messages ("blocked for team 'research'").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    Org,
    Team,
}

impl ScopeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ScopeKind::Org => "org",
            ScopeKind::Team => "team",
        }
    }
}

/// A borrowed view of one config scope's request-policy fields — the subset
/// `OrgConfig` and `TeamConfig` have in common.
#[derive(Debug, Clone, Copy)]
pub struct Scope<'a> {
    pub kind: ScopeKind,
    pub id: &'a str,
    pub allowed_models: Option<&'a [String]>,
    pub blocked_models: Option<&'a [String]>,
    pub token_budget: Option<&'a OrgTokenBudget>,
    pub guardrails: &'a OrgGuardrailConfig,
}

impl Config {
    /// The chain of scopes that apply to a principal, least→most specific:
    /// `[org]` or `[org, team]`. Empty when the principal carries no org id or
    /// the org isn't configured. A team exists only under its org: a `team_id`
    /// with no configured org, or one not present in that org's `teams`,
    /// contributes nothing.
    pub fn scopes(&self, org_id: Option<&str>, team_id: Option<&str>) -> Vec<Scope<'_>> {
        let Some((org_id, org)) = org_id.and_then(|id| self.orgs.get_key_value(id)) else {
            return Vec::new();
        };

        let mut chain = vec![Scope {
            kind: ScopeKind::Org,
            id: org_id,
            allowed_models: org.allowed_models.as_deref(),
            blocked_models: org.blocked_models.as_deref(),
            token_budget: org.token_budget.as_ref(),
            guardrails: &org.guardrails,
        }];

        if let Some((team_id, team)) = team_id.and_then(|id| org.teams.get_key_value(id)) {
            chain.push(Scope {
                kind: ScopeKind::Team,
                id: team_id,
                allowed_models: team.allowed_models.as_deref(),
                blocked_models: team.blocked_models.as_deref(),
                token_budget: team.token_budget.as_ref(),
                guardrails: &team.guardrails,
            });
        }

        chain
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{OrgConfig, TeamConfig};

    fn config_with(org_id: &str, teams: &[&str]) -> Config {
        let mut org = OrgConfig::default();
        for t in teams {
            org.teams.insert(t.to_string(), TeamConfig::default());
        }
        let mut cfg = Config::default();
        cfg.orgs.insert(org_id.to_string(), org);
        cfg
    }

    #[test]
    fn no_org_id_or_unknown_org_yields_empty_chain() {
        let cfg = config_with("acme", &["research"]);
        assert!(cfg.scopes(None, None).is_empty());
        assert!(cfg.scopes(None, Some("research")).is_empty());
        assert!(cfg.scopes(Some("unknown"), Some("research")).is_empty());
    }

    #[test]
    fn org_only_chain() {
        let cfg = config_with("acme", &["research"]);
        let chain = cfg.scopes(Some("acme"), None);
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].kind, ScopeKind::Org);
        assert_eq!(chain[0].id, "acme");
    }

    #[test]
    fn org_then_team_least_to_most_specific() {
        let cfg = config_with("acme", &["research"]);
        let chain = cfg.scopes(Some("acme"), Some("research"));
        assert_eq!(chain.len(), 2);
        assert_eq!((chain[0].kind, chain[0].id), (ScopeKind::Org, "acme"));
        assert_eq!((chain[1].kind, chain[1].id), (ScopeKind::Team, "research"));
    }

    #[test]
    fn team_unknown_under_org_contributes_nothing() {
        let cfg = config_with("acme", &["research"]);
        let chain = cfg.scopes(Some("acme"), Some("not-a-team"));
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].kind, ScopeKind::Org);
    }
}
