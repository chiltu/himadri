//! Standard role taxonomy and capabilities.
//!
//! Defines a portable, vendor-neutral role set that enables consistent RBAC
//! configuration across organizations and identity providers (Zitadel, Auth0,
//! Keycloak, Okta, Entra, etc.).

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Standard role names in the role catalog.
/// These map to [`RoleCapabilities`] and can be extended with org-specific roles.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StandardRole {
    /// Basic authenticated user. Can submit requests to assigned models/providers.
    User,
    /// Extended execution access. Can use all models/providers without RBAC restriction.
    PowerUser,
    /// Observation and audit access. Can view usage logs and audit trails.
    Analyst,
    /// Full gateway control. Admin access with all capabilities.
    Admin,
    /// Custom org-defined role (e.g., "ml-engineer", "team-lead").
    Custom(String),
}

impl StandardRole {
    /// Parse a role string to StandardRole. Custom roles become `Custom(role_name)`.
    pub fn from_string(s: &str) -> Self {
        match s {
            "user" => Self::User,
            "power-user" | "poweruser" => Self::PowerUser,
            "analyst" => Self::Analyst,
            "admin" => Self::Admin,
            other => Self::Custom(other.to_string()),
        }
    }

    /// Serialize to string (canonical form).
    pub fn as_str(&self) -> String {
        match self {
            Self::User => "user".to_string(),
            Self::PowerUser => "power-user".to_string(),
            Self::Analyst => "analyst".to_string(),
            Self::Admin => "admin".to_string(),
            Self::Custom(name) => name.clone(),
        }
    }
}

/// Capabilities a role grants.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoleCapabilities {
    /// If true, this role inherits all capabilities from other roles.
    pub can_execute: bool,
    /// If true, can submit requests to assigned models.
    pub can_submit_requests: bool,
    /// If true, can list/read available models.
    pub can_read_models: bool,
    /// If true, no RBAC model restrictions apply (all models allowed).
    pub can_use_any_model: bool,
    /// If true, no RBAC provider restrictions apply (all providers allowed).
    pub can_use_any_provider: bool,
    /// If true, can view usage/billing information.
    pub can_read_usage: bool,
    /// If true, can view audit logs.
    pub can_read_logs: bool,
    /// If true, can manage API keys.
    pub can_manage_keys: bool,
    /// If true, can modify gateway configuration.
    pub can_manage_config: bool,
    /// If true, can view and manage team members.
    pub can_manage_team: bool,
    /// If true, can view and modify budget settings.
    pub can_manage_budget: bool,
    /// Additional custom capabilities (vendor-specific or org-specific).
    pub custom_capabilities: HashSet<String>,
}

impl RoleCapabilities {
    /// Capabilities for the "user" role: basic execution.
    pub fn user() -> Self {
        Self {
            can_execute: true,
            can_submit_requests: true,
            can_read_models: true,
            can_use_any_model: false,
            can_use_any_provider: false,
            can_read_usage: false,
            can_read_logs: false,
            can_manage_keys: false,
            can_manage_config: false,
            can_manage_team: false,
            can_manage_budget: false,
            custom_capabilities: HashSet::new(),
        }
    }

    /// Capabilities for the "power-user" role: extended execution.
    pub fn power_user() -> Self {
        Self {
            can_execute: true,
            can_submit_requests: true,
            can_read_models: true,
            can_use_any_model: true,
            can_use_any_provider: true,
            can_read_usage: false,
            can_read_logs: false,
            can_manage_keys: false,
            can_manage_config: false,
            can_manage_team: false,
            can_manage_budget: false,
            custom_capabilities: HashSet::new(),
        }
    }

    /// Capabilities for the "analyst" role: observation and audit.
    pub fn analyst() -> Self {
        Self {
            can_execute: false,
            can_submit_requests: false,
            can_read_models: true,
            can_use_any_model: false,
            can_use_any_provider: false,
            can_read_usage: true,
            can_read_logs: true,
            can_manage_keys: false,
            can_manage_config: false,
            can_manage_team: false,
            can_manage_budget: false,
            custom_capabilities: HashSet::new(),
        }
    }

    /// Capabilities for the "admin" role: full control.
    pub fn admin() -> Self {
        Self {
            can_execute: true,
            can_submit_requests: true,
            can_read_models: true,
            can_use_any_model: true,
            can_use_any_provider: true,
            can_read_usage: true,
            can_read_logs: true,
            can_manage_keys: true,
            can_manage_config: true,
            can_manage_team: true,
            can_manage_budget: true,
            custom_capabilities: HashSet::new(),
        }
    }

    /// Merge capabilities: union across all (most-permissive wins).
    pub fn merge(&mut self, other: &Self) {
        self.can_execute |= other.can_execute;
        self.can_submit_requests |= other.can_submit_requests;
        self.can_read_models |= other.can_read_models;
        self.can_use_any_model |= other.can_use_any_model;
        self.can_use_any_provider |= other.can_use_any_provider;
        self.can_read_usage |= other.can_read_usage;
        self.can_read_logs |= other.can_read_logs;
        self.can_manage_keys |= other.can_manage_keys;
        self.can_manage_config |= other.can_manage_config;
        self.can_manage_team |= other.can_manage_team;
        self.can_manage_budget |= other.can_manage_budget;
        self.custom_capabilities
            .extend(other.custom_capabilities.clone());
    }
}

/// Standard billing tiers that can be embedded in JWT `custom:billing_tier` claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BillingTier {
    Free,
    Pro,
    Enterprise,
    Custom(String),
}

impl BillingTier {
    pub fn from_string(s: &str) -> Self {
        match s {
            "free" => Self::Free,
            "pro" => Self::Pro,
            "enterprise" => Self::Enterprise,
            other => Self::Custom(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Free => "free",
            Self::Pro => "pro",
            Self::Enterprise => "enterprise",
            Self::Custom(s) => s,
        }
    }
}

/// Mapping from vendor-specific role names to standard roles.
/// Used during JWT claims processing to normalize roles from different OIDC providers.
pub struct VendorRoleMapping {
    /// Map vendor role name → standard role name
    pub mappings: HashMap<String, String>,
}

impl VendorRoleMapping {
    /// Create an empty mapping.
    pub fn new() -> Self {
        Self {
            mappings: HashMap::new(),
        }
    }

    /// Zitadel project role mapping.
    pub fn zitadel() -> Self {
        let mut mappings = HashMap::new();
        mappings.insert("admin".to_string(), "admin".to_string());
        mappings.insert("editor".to_string(), "power-user".to_string());
        mappings.insert("viewer".to_string(), "analyst".to_string());
        mappings.insert("member".to_string(), "user".to_string());

        Self { mappings }
    }

    /// Auth0 role mapping.
    pub fn auth0() -> Self {
        let mut mappings = HashMap::new();
        mappings.insert("Admin".to_string(), "admin".to_string());
        mappings.insert("Editor".to_string(), "power-user".to_string());
        mappings.insert("Viewer".to_string(), "analyst".to_string());
        mappings.insert("User".to_string(), "user".to_string());

        Self { mappings }
    }

    /// Keycloak realm role mapping.
    pub fn keycloak() -> Self {
        let mut mappings = HashMap::new();
        mappings.insert("realm-admin".to_string(), "admin".to_string());
        mappings.insert("realm-editor".to_string(), "power-user".to_string());
        mappings.insert("realm-viewer".to_string(), "analyst".to_string());
        mappings.insert("realm-user".to_string(), "user".to_string());

        Self { mappings }
    }

    /// Azure AD / Entra ID role mapping.
    pub fn entra() -> Self {
        let mut mappings = HashMap::new();
        mappings.insert("GlobalAdmin".to_string(), "admin".to_string());
        mappings.insert("Admin".to_string(), "admin".to_string());
        mappings.insert("Editor".to_string(), "power-user".to_string());
        mappings.insert("Reader".to_string(), "analyst".to_string());
        mappings.insert("User".to_string(), "user".to_string());

        Self { mappings }
    }

    /// Okta role mapping.
    pub fn okta() -> Self {
        let mut mappings = HashMap::new();
        mappings.insert("System Administrator".to_string(), "admin".to_string());
        mappings.insert(
            "Organization Administrator".to_string(),
            "admin".to_string(),
        );
        mappings.insert(
            "Application Administrator".to_string(),
            "power-user".to_string(),
        );
        mappings.insert("Group Administrator".to_string(), "power-user".to_string());
        mappings.insert("User Administrator".to_string(), "analyst".to_string());
        mappings.insert("Help Desk Administrator".to_string(), "analyst".to_string());
        mappings.insert("Okta User".to_string(), "user".to_string());

        Self { mappings }
    }

    /// Ping Identity role mapping.
    pub fn ping_identity() -> Self {
        let mut mappings = HashMap::new();
        mappings.insert("Administrator".to_string(), "admin".to_string());
        mappings.insert("Editor".to_string(), "power-user".to_string());
        mappings.insert("Auditor".to_string(), "analyst".to_string());
        mappings.insert("User".to_string(), "user".to_string());

        Self { mappings }
    }

    /// Map a vendor role name to a standard role name.
    /// Returns the mapped role, or the original if no mapping exists.
    pub fn map(&self, vendor_role: &str) -> String {
        self.mappings
            .get(vendor_role)
            .cloned()
            .unwrap_or_else(|| vendor_role.to_string())
    }

    /// Insert a custom mapping.
    pub fn insert(&mut self, vendor_role: impl Into<String>, standard_role: impl Into<String>) {
        self.mappings
            .insert(vendor_role.into(), standard_role.into());
    }
}

impl Default for VendorRoleMapping {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_standard_role_parsing() {
        assert_eq!(StandardRole::from_string("user"), StandardRole::User);
        assert_eq!(
            StandardRole::from_string("power-user"),
            StandardRole::PowerUser
        );
        assert_eq!(StandardRole::from_string("analyst"), StandardRole::Analyst);
        assert_eq!(StandardRole::from_string("admin"), StandardRole::Admin);
        assert_eq!(
            StandardRole::from_string("ml-engineer"),
            StandardRole::Custom("ml-engineer".to_string())
        );
    }

    #[test]
    fn test_role_capabilities_merge() {
        let mut user_cap = RoleCapabilities::user();
        let analyst_cap = RoleCapabilities::analyst();

        user_cap.merge(&analyst_cap);

        assert!(user_cap.can_submit_requests);
        assert!(user_cap.can_read_usage);
        assert!(user_cap.can_read_logs);
    }

    #[test]
    fn test_vendor_role_mapping_zitadel() {
        let mapping = VendorRoleMapping::zitadel();
        assert_eq!(mapping.map("admin"), "admin");
        assert_eq!(mapping.map("editor"), "power-user");
        assert_eq!(mapping.map("viewer"), "analyst");
        assert_eq!(mapping.map("unknown"), "unknown");
    }

    #[test]
    fn test_vendor_role_mapping_auth0() {
        let mapping = VendorRoleMapping::auth0();
        assert_eq!(mapping.map("Admin"), "admin");
        assert_eq!(mapping.map("Editor"), "power-user");
    }

    #[test]
    fn test_billing_tier_from_string() {
        assert_eq!(BillingTier::from_string("free"), BillingTier::Free);
        assert_eq!(BillingTier::from_string("pro"), BillingTier::Pro);
        assert_eq!(
            BillingTier::from_string("enterprise"),
            BillingTier::Enterprise
        );
    }
}
