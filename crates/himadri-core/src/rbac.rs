//! Role-based access control evaluation.
//!
//! Turns an [`RbacConfig`] plus a principal's roles into allow/deny decisions
//! for model and provider access. Used by the gateway to enforce tiered access
//! on the `/v1` endpoints.

use crate::config::{RbacConfig, RolePolicy};

/// Reason an RBAC check denied a request. Rendered into a `403 Forbidden`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RbacDenial {
    /// The principal has no role matching the policy and no default applies.
    NoMatchingRole,
    /// The requested model is not permitted for the principal's roles.
    ModelForbidden(String),
    /// The requested provider is not permitted for the principal's roles.
    ProviderForbidden(String),
}

impl std::fmt::Display for RbacDenial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RbacDenial::NoMatchingRole => {
                write!(f, "no role grants access to this gateway")
            }
            RbacDenial::ModelForbidden(m) => {
                write!(f, "model '{}' is not permitted for your role", m)
            }
            RbacDenial::ProviderForbidden(p) => {
                write!(f, "provider '{}' is not permitted for your role", p)
            }
        }
    }
}

/// Match a value against a pattern that may contain `*` wildcards.
///
/// `*` matches any (possibly empty) run of characters. `"*"` matches anything.
/// Matching is case-sensitive and anchored to the whole string.
fn pattern_matches(pattern: &str, value: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return pattern == value;
    }

    // Split on '*' and greedily match each literal segment in order. Leading and
    // trailing empty segments encode "must start/end with".
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut pos = 0usize;

    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            // Anchored prefix.
            if !value[pos..].starts_with(part) {
                return false;
            }
            pos += part.len();
        } else if i == parts.len() - 1 {
            // Anchored suffix.
            return value[pos..].ends_with(part);
        } else if let Some(idx) = value[pos..].find(part) {
            pos += idx + part.len();
        } else {
            return false;
        }
    }
    true
}

fn any_match(patterns: &[String], value: &str) -> bool {
    patterns.iter().any(|p| pattern_matches(p, value))
}

/// The merged, effective access policy for a principal across all of its roles.
struct EffectivePolicy {
    /// Allowed model patterns. `None` means unrestricted (any model).
    models: Option<Vec<String>>,
    /// Allowed provider patterns. `None` means unrestricted (any provider).
    providers: Option<Vec<String>>,
}

impl RbacConfig {
    /// Resolve the effective policy for the given roles, or `None` if no role
    /// matched and no `default_role` applies.
    fn effective_policy(&self, roles: &[String]) -> Option<EffectivePolicy> {
        let matched: Vec<&RolePolicy> = roles.iter().filter_map(|r| self.roles.get(r)).collect();

        let policies: Vec<&RolePolicy> = if matched.is_empty() {
            match &self.default_role {
                Some(default) => self.roles.get(default).into_iter().collect(),
                None => return None,
            }
        } else {
            matched
        };

        if policies.is_empty() {
            // default_role names a role that doesn't exist — treat as no match.
            return None;
        }

        // Union across roles: a role with `None` (unrestricted) makes the whole
        // dimension unrestricted; otherwise concatenate the allow-lists.
        let mut models: Option<Vec<String>> = Some(Vec::new());
        let mut providers: Option<Vec<String>> = Some(Vec::new());

        for p in policies {
            merge_dimension(&mut models, &p.models);
            merge_dimension(&mut providers, &p.providers);
        }

        Some(EffectivePolicy { models, providers })
    }

    /// Check `value` against one policy dimension (models or providers),
    /// selected by `select`, producing `deny(value)` when it isn't permitted.
    /// Shared by [`check_model`](Self::check_model) and
    /// [`check_provider`](Self::check_provider).
    fn check_dimension(
        &self,
        roles: &[String],
        is_admin: bool,
        value: &str,
        select: impl Fn(&EffectivePolicy) -> &Option<Vec<String>>,
        deny: impl Fn(String) -> RbacDenial,
    ) -> Result<(), RbacDenial> {
        if !self.enabled || is_admin {
            return Ok(());
        }
        let policy = self
            .effective_policy(roles)
            .ok_or(RbacDenial::NoMatchingRole)?;
        match select(&policy) {
            None => Ok(()),
            Some(allowed) if any_match(allowed, value) => Ok(()),
            Some(_) => Err(deny(value.to_string())),
        }
    }

    /// Check whether the principal (identified by `roles`) may use `model`.
    /// `is_admin` short-circuits to allow (e.g. master key / admin scope).
    pub fn check_model(
        &self,
        roles: &[String],
        is_admin: bool,
        model: &str,
    ) -> Result<(), RbacDenial> {
        self.check_dimension(
            roles,
            is_admin,
            model,
            |p| &p.models,
            RbacDenial::ModelForbidden,
        )
    }

    /// Check whether the principal may route to `provider`.
    pub fn check_provider(
        &self,
        roles: &[String],
        is_admin: bool,
        provider: &str,
    ) -> Result<(), RbacDenial> {
        self.check_dimension(
            roles,
            is_admin,
            provider,
            |p| &p.providers,
            RbacDenial::ProviderForbidden,
        )
    }
}

/// Union one dimension's allow-list into `acc`: `None` (unrestricted) wins and
/// makes the dimension unrestricted; otherwise the patterns are concatenated.
fn merge_dimension(acc: &mut Option<Vec<String>>, item: &Option<Vec<String>>) {
    match item {
        None => *acc = None,
        Some(list) => {
            if let Some(a) = acc.as_mut() {
                a.extend(list.iter().cloned());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn policy(models: Option<Vec<&str>>, providers: Option<Vec<&str>>) -> RolePolicy {
        RolePolicy {
            models: models.map(|v| v.into_iter().map(String::from).collect()),
            providers: providers.map(|v| v.into_iter().map(String::from).collect()),
        }
    }

    fn config() -> RbacConfig {
        let mut roles = HashMap::new();
        roles.insert(
            "analyst".to_string(),
            policy(Some(vec!["gpt-4o-mini"]), None),
        );
        roles.insert(
            "engineer".to_string(),
            policy(Some(vec!["gpt-4o", "o1", "claude-*"]), None),
        );
        roles.insert(
            "ml-platform".to_string(),
            policy(None, Some(vec!["openai", "bedrock"])),
        );
        RbacConfig {
            enabled: true,
            roles,
            default_role: None,
        }
    }

    #[test]
    fn test_pattern_matches() {
        assert!(pattern_matches("*", "anything"));
        assert!(pattern_matches("gpt-4o", "gpt-4o"));
        assert!(!pattern_matches("gpt-4o", "gpt-4o-mini"));
        assert!(pattern_matches("claude-*", "claude-3-5-sonnet"));
        assert!(!pattern_matches("claude-*", "gpt-4o"));
        assert!(pattern_matches("*-mini", "gpt-4o-mini"));
        assert!(pattern_matches("a*b*c", "axxbyyc"));
        assert!(!pattern_matches("a*b*c", "axxb"));
    }

    #[test]
    fn test_disabled_allows_everything() {
        let mut cfg = config();
        cfg.enabled = false;
        assert!(cfg.check_model(&[], false, "anything").is_ok());
    }

    #[test]
    fn test_admin_bypasses() {
        let cfg = config();
        assert!(cfg.check_model(&[], true, "gpt-4o").is_ok());
        assert!(cfg.check_provider(&[], true, "bedrock").is_ok());
    }

    #[test]
    fn test_model_allow_and_deny() {
        let cfg = config();
        let analyst = vec!["analyst".to_string()];
        assert!(cfg.check_model(&analyst, false, "gpt-4o-mini").is_ok());
        assert_eq!(
            cfg.check_model(&analyst, false, "gpt-4o"),
            Err(RbacDenial::ModelForbidden("gpt-4o".to_string()))
        );

        let engineer = vec!["engineer".to_string()];
        assert!(cfg
            .check_model(&engineer, false, "claude-3-5-sonnet")
            .is_ok());
        assert!(cfg.check_model(&engineer, false, "o1").is_ok());
        assert!(cfg.check_model(&engineer, false, "gpt-4o-mini").is_err());
    }

    #[test]
    fn test_union_across_roles_is_most_permissive() {
        let cfg = config();
        let both = vec!["analyst".to_string(), "engineer".to_string()];
        // Union of analyst + engineer model lists.
        assert!(cfg.check_model(&both, false, "gpt-4o-mini").is_ok());
        assert!(cfg.check_model(&both, false, "gpt-4o").is_ok());
    }

    #[test]
    fn test_unrestricted_model_role() {
        let cfg = config();
        let ml = vec!["ml-platform".to_string()];
        // ml-platform has models=None → any model allowed.
        assert!(cfg.check_model(&ml, false, "some-exotic-model").is_ok());
        // but provider is restricted.
        assert!(cfg.check_provider(&ml, false, "openai").is_ok());
        assert_eq!(
            cfg.check_provider(&ml, false, "gemini"),
            Err(RbacDenial::ProviderForbidden("gemini".to_string()))
        );
    }

    #[test]
    fn test_no_matching_role_denied() {
        let cfg = config();
        let unknown = vec!["random".to_string()];
        assert_eq!(
            cfg.check_model(&unknown, false, "gpt-4o"),
            Err(RbacDenial::NoMatchingRole)
        );
    }

    #[test]
    fn test_default_role_applies() {
        let mut cfg = config();
        cfg.default_role = Some("analyst".to_string());
        let unknown = vec!["random".to_string()];
        // Falls back to analyst's policy.
        assert!(cfg.check_model(&unknown, false, "gpt-4o-mini").is_ok());
        assert!(cfg.check_model(&unknown, false, "gpt-4o").is_err());
    }
}
