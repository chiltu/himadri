use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JwtClaims {
    /// Subject (user ID)
    pub sub: String,

    /// Issuer (e.g., Zitadel URL)
    pub iss: String,

    /// Audience (client ID). Per RFC 7519 this may be a single string or an
    /// array of strings; multiple values are joined with commas.
    #[serde(deserialize_with = "deserialize_aud")]
    pub aud: String,

    /// Expiration time (Unix timestamp)
    pub exp: u64,

    /// Issued at (Unix timestamp)
    pub iat: u64,

    /// Not before (Unix timestamp)
    #[serde(default)]
    pub nbf: Option<u64>,

    /// Token ID
    #[serde(default)]
    pub jti: Option<String>,

    /// Space-separated scopes
    #[serde(default)]
    pub scope: Option<String>,

    /// Organization ID (Zitadel-specific)
    #[serde(default)]
    pub org_id: Option<String>,

    /// Team ID
    #[serde(default)]
    pub team_id: Option<String>,

    /// Email
    #[serde(default)]
    pub email: Option<String>,

    /// Email verified
    #[serde(default)]
    pub email_verified: Option<bool>,

    /// Roles
    #[serde(default)]
    pub roles: Option<Vec<String>>,

    /// Rate limit RPM from claims
    #[serde(default, deserialize_with = "deserialize_optional_u64")]
    pub rate_limit_rpm: Option<u64>,

    /// Budget limit USD from claims
    #[serde(default, deserialize_with = "deserialize_optional_f64")]
    pub budget_limit_usd: Option<f64>,

    /// Custom claims
    #[serde(flatten)]
    pub custom: std::collections::HashMap<String, serde_json::Value>,
}

/// Deserialize the `aud` claim from either a string or an array of strings.
fn deserialize_aud<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let val = serde_json::Value::deserialize(deserializer)?;
    match val {
        serde_json::Value::String(s) => Ok(s),
        serde_json::Value::Array(arr) => Ok(arr
            .into_iter()
            .map(|v| match v {
                serde_json::Value::String(s) => s,
                other => other.to_string(),
            })
            .collect::<Vec<_>>()
            .join(",")),
        serde_json::Value::Null => Ok(String::new()),
        other => Ok(other.to_string()),
    }
}

fn deserialize_optional_u64<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let val = serde_json::Value::deserialize(deserializer)?;
    match val {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::Number(n) => Ok(n.as_u64()),
        serde_json::Value::String(s) => Ok(s.parse::<u64>().ok()),
        _ => Ok(None),
    }
}

fn deserialize_optional_f64<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let val = serde_json::Value::deserialize(deserializer)?;
    match val {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::Number(n) => Ok(n.as_f64()),
        serde_json::Value::String(s) => Ok(s.parse::<f64>().ok()),
        _ => Ok(None),
    }
}

impl JwtClaims {
    /// Check if token is expired
    pub fn is_expired(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        self.exp < now
    }

    /// Check if token is not yet valid
    pub fn is_not_yet_valid(&self) -> bool {
        if let Some(nbf) = self.nbf {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            nbf > now
        } else {
            false
        }
    }

    /// Extract scopes as a vector
    pub fn scopes(&self) -> Vec<String> {
        self.scope
            .as_ref()
            .map(|s| s.split_whitespace().map(|s| s.to_string()).collect())
            .unwrap_or_default()
    }

    /// Collect all roles granted to the principal.
    ///
    /// Combines two sources:
    /// 1. The flat `roles` claim (a JSON array of strings), if present.
    /// 2. Zitadel project-role claims. Zitadel emits granted project roles under
    ///    a URN-namespaced key — either `urn:zitadel:iam:org:project:roles` or
    ///    the project-scoped `urn:zitadel:iam:org:project:{project_id}:roles` —
    ///    whose value is an object whose *keys* are the role names, e.g.
    ///    `{ "admin": { "<org_id>": "<domain>" }, "user": { ... } }`.
    pub fn roles(&self) -> Vec<String> {
        let mut roles: Vec<String> = self.roles.clone().unwrap_or_default();

        for (key, value) in &self.custom {
            if key.starts_with("urn:zitadel:iam:org:project:") && key.ends_with(":roles") {
                if let Some(map) = value.as_object() {
                    roles.extend(map.keys().cloned());
                }
            }
        }

        roles.sort();
        roles.dedup();
        roles
    }

    /// Convert to AuthContext with roles and rate limit overrides from claims.
    ///
    /// Scope is derived from roles first (so Zitadel RBAC drives the gateway's
    /// Admin/ReadOnly distinction) and falls back to the OAuth `scope` string.
    pub fn into_auth_context(self) -> himadri_core::AuthContext {
        let roles = self.roles();
        let scopes = self.scopes();

        let is_admin = roles.iter().any(|r| r == "admin" || r == "gateway-admin")
            || scopes.contains(&"admin".to_string());
        let is_readonly = roles
            .iter()
            .any(|r| r == "read-only" || r == "readonly" || r == "read")
            || scopes.contains(&"read".to_string());

        let scope = if is_admin {
            himadri_core::AuthScope::Admin
        } else if is_readonly {
            himadri_core::AuthScope::ReadOnly
        } else {
            himadri_core::AuthScope::ApiKey
        };

        // Parse rate limit override from claims
        let rate_limit_override = self.rate_limit_rpm.map(|rpm| {
            himadri_core::RateLimitOverride {
                requests_per_second: Some(rpm / 60), // Convert RPM to RPS
                burst_size: Some(rpm),               // Burst = 1 minute worth
            }
        });

        himadri_core::AuthContext {
            api_key: format!("jwt:{}", self.sub),
            key_id: Some(self.sub.clone()),
            scope,
            org_id: self.org_id.clone(),
            team_id: self.team_id.clone(),
            user_id: Some(self.sub),
            rate_limit_override,
            roles,
            budget_limit_usd: self.budget_limit_usd,
        }
    }
}
