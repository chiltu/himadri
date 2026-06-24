use serde::{Deserialize, Serialize};

use crate::jwt::JwtClaims;
use himadri_core::AuthContext;

/// Zitadel-specific JWT claims
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZitadelClaims {
    /// Standard OIDC claims
    #[serde(flatten)]
    pub standard: JwtClaims,

    /// Zitadel organization ID
    #[serde(default, deserialize_with = "deserialize_zitadel_string")]
    pub zitadel_org_id: Option<String>,

    /// Zitadel project ID
    #[serde(default, deserialize_with = "deserialize_zitadel_string")]
    pub zitadel_project_id: Option<String>,

    /// Zitadel user ID
    #[serde(default, deserialize_with = "deserialize_zitadel_string")]
    pub zitadel_user_id: Option<String>,

    /// Zitadel username
    #[serde(default, deserialize_with = "deserialize_zitadel_string")]
    pub zitadel_username: Option<String>,

    /// Zitadel email
    #[serde(default, deserialize_with = "deserialize_zitadel_string")]
    pub zitadel_email: Option<String>,

    /// Zitadel email verified
    #[serde(default)]
    pub zitadel_verified: Option<bool>,

    /// Rate limit RPM from Zitadel metadata
    #[serde(default, deserialize_with = "deserialize_optional_u64")]
    pub rate_limit_rpm: Option<u64>,

    /// Budget limit USD from Zitadel metadata
    #[serde(default, deserialize_with = "deserialize_optional_f64")]
    pub budget_limit_usd: Option<f64>,
}

fn deserialize_zitadel_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let val = serde_json::Value::deserialize(deserializer)?;
    match val {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::String(s) => Ok(Some(s)),
        _ => Ok(None),
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

impl ZitadelClaims {
    /// Convert to AuthContext with Zitadel-specific fields
    pub fn into_auth_context(self) -> AuthContext {
        let mut auth_ctx = self.standard.into_auth_context();

        // Override with Zitadel-specific fields if present
        if let Some(org_id) = self.zitadel_org_id {
            auth_ctx.org_id = Some(org_id);
        }
        if let Some(team_id) = self.zitadel_project_id {
            auth_ctx.team_id = Some(team_id);
        }
        if let Some(user_id) = self.zitadel_user_id {
            auth_ctx.user_id = Some(user_id);
        }

        // Apply Zitadel-specific rate limit override
        if let Some(rpm) = self.rate_limit_rpm {
            auth_ctx.rate_limit_override = Some(himadri_core::RateLimitOverride {
                requests_per_second: Some(rpm / 60),
                burst_size: Some(rpm),
            });
        }

        auth_ctx
    }

    /// Parse from raw JWT claims
    pub fn from_jwt_claims(claims: &JwtClaims) -> Self {
        Self {
            standard: claims.clone(),
            zitadel_org_id: claims
                .custom
                .get("zitadel_org_id")
                .and_then(|v| v.as_str().map(|s| s.to_string())),
            zitadel_project_id: claims
                .custom
                .get("zitadel_project_id")
                .and_then(|v| v.as_str().map(|s| s.to_string())),
            zitadel_user_id: claims
                .custom
                .get("zitadel_user_id")
                .and_then(|v| v.as_str().map(|s| s.to_string())),
            zitadel_username: claims
                .custom
                .get("zitadel_username")
                .and_then(|v| v.as_str().map(|s| s.to_string())),
            zitadel_email: claims
                .custom
                .get("zitadel_email")
                .and_then(|v| v.as_str().map(|s| s.to_string())),
            zitadel_verified: claims
                .custom
                .get("zitadel_verified")
                .and_then(|v| v.as_bool()),
            rate_limit_rpm: claims.rate_limit_rpm,
            budget_limit_usd: claims.budget_limit_usd,
        }
    }
}
