use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JwtClaims {
    /// Subject (user ID)
    pub sub: String,

    /// Issuer (e.g., Zitadel URL)
    pub iss: String,

    /// Audience (client ID)
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

    /// Convert to AuthContext with rate limit overrides from claims
    pub fn into_auth_context(self) -> himadri_core::AuthContext {
        let scope = if self.scopes().contains(&"admin".to_string()) {
            himadri_core::AuthScope::Admin
        } else if self.scopes().contains(&"read".to_string()) {
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
        }
    }
}
