use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::debug;

use crate::error::AuthError;
use himadri_core::AuthContext;

/// OAuth2 token introspection result (RFC 7662)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntrospectionResult {
    /// Whether the token is active
    pub active: bool,

    /// The scope of the token
    #[serde(default)]
    pub scope: Option<String>,

    /// Client identifier
    #[serde(default)]
    pub client_id: Option<String>,

    /// Human-readable identifier
    #[serde(default)]
    pub username: Option<String>,

    /// Token type
    #[serde(default)]
    pub token_type: Option<String>,

    /// Expiration timestamp
    #[serde(default)]
    pub exp: Option<u64>,

    /// Issued at timestamp
    #[serde(default)]
    pub iat: Option<u64>,

    /// Subject identifier
    #[serde(default)]
    pub sub: Option<String>,

    /// Audience
    #[serde(default)]
    pub aud: Option<String>,

    /// Issuer
    #[serde(default)]
    pub iss: Option<String>,

    /// Token identifier
    #[serde(default)]
    pub jti: Option<String>,
}

impl IntrospectionResult {
    /// Convert to AuthContext
    pub fn into_auth_context(self) -> Result<AuthContext, AuthError> {
        if !self.active {
            return Err(AuthError::TokenInactive);
        }

        let sub = self.sub.unwrap_or_else(|| "unknown".to_string());

        let scope = self.scope.as_deref().unwrap_or("");
        let roles: Vec<String> = scope.split_whitespace().map(|s| s.to_string()).collect();
        let auth_scope = if scope.contains("admin") {
            himadri_core::AuthScope::Admin
        } else if scope.contains("read") {
            himadri_core::AuthScope::ReadOnly
        } else {
            himadri_core::AuthScope::ApiKey
        };

        Ok(AuthContext {
            api_key: format!("oauth2:{}", sub),
            key_id: self.client_id,
            scope: auth_scope,
            org_id: None,
            team_id: None,
            user_id: Some(sub),
            rate_limit_override: None,
            roles,
            budget_limit_usd: None,
        })
    }
}

/// OAuth2 token introspector
pub struct TokenIntrospector {
    introspection_endpoint: String,
    client_id: String,
    client_secret: String,
    http_client: reqwest::Client,
}

impl TokenIntrospector {
    /// Create a new token introspector
    pub fn new(introspection_endpoint: String, client_id: String, client_secret: String) -> Self {
        Self {
            introspection_endpoint,
            client_id,
            client_secret,
            http_client: reqwest::Client::new(),
        }
    }

    /// Introspect a token
    pub async fn introspect(&self, token: &str) -> Result<IntrospectionResult, AuthError> {
        debug!("Introspecting token at {}", self.introspection_endpoint);

        let response = self
            .http_client
            .post(&self.introspection_endpoint)
            .basic_auth(&self.client_id, Some(&self.client_secret))
            .form(&[("token", token)])
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(AuthError::IntrospectionFailed(format!(
                "HTTP {}: {}",
                status, body
            )));
        }

        let result: IntrospectionResult = response
            .json()
            .await
            .map_err(|e| AuthError::IntrospectionFailed(e.to_string()))?;

        debug!("Token introspection: active={}", result.active);

        Ok(result)
    }
}

/// In-memory cache of introspectors
static INTROSPECTOR_CACHE: once_cell::sync::Lazy<DashMap<String, Arc<TokenIntrospector>>> =
    once_cell::sync::Lazy::new(DashMap::new);

/// Get or create an introspector
pub fn get_or_create_introspector(
    introspection_endpoint: &str,
    client_id: &str,
    client_secret: &str,
) -> Arc<TokenIntrospector> {
    let key = introspection_endpoint.to_string();

    if let Some(introspector) = INTROSPECTOR_CACHE.get(&key) {
        return introspector.clone();
    }

    let introspector = Arc::new(TokenIntrospector::new(
        introspection_endpoint.to_string(),
        client_id.to_string(),
        client_secret.to_string(),
    ));

    INTROSPECTOR_CACHE.insert(key, introspector.clone());
    introspector
}
