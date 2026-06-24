use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::debug;

use crate::error::AuthError;

/// OAuth2 token response from token endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default = "default_bearer")]
    pub token_type: String,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub scope: Option<String>,
}

fn default_bearer() -> String {
    "Bearer".to_string()
}

/// OAuth2 client_credentials token client
pub struct TokenClient {
    client_id: String,
    client_secret: String,
    token_endpoint: String,
    http_client: reqwest::Client,
    cache: DashMap<String, CachedToken>,
}

struct CachedToken {
    token: String,
    expires_at: std::time::Instant,
}

impl TokenClient {
    /// Create a new token client
    pub fn new(client_id: String, client_secret: String, token_endpoint: String) -> Self {
        Self {
            client_id,
            client_secret,
            token_endpoint,
            http_client: reqwest::Client::new(),
            cache: DashMap::new(),
        }
    }

    /// Request a token using client_credentials grant
    pub async fn request_token(&self, scope: Option<&str>) -> Result<TokenResponse, AuthError> {
        let mut form = vec![("grant_type", "client_credentials".to_string())];

        if let Some(scope) = scope {
            form.push(("scope", scope.to_string()));
        }

        debug!("Requesting OAuth2 token from {}", self.token_endpoint);

        let response = self
            .http_client
            .post(&self.token_endpoint)
            .basic_auth(&self.client_id, Some(&self.client_secret))
            .form(&form)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(AuthError::TokenRequestFailed(format!(
                "HTTP {}: {}",
                status, body
            )));
        }

        let token: TokenResponse = response
            .json()
            .await
            .map_err(|e| AuthError::TokenRequestFailed(e.to_string()))?;

        debug!("OAuth2 token received, expires_in: {:?}", token.expires_in);

        Ok(token)
    }

    /// Get a valid token, refreshing if needed
    pub async fn get_valid_token(&self, scope: Option<&str>) -> Result<String, AuthError> {
        let cache_key = scope.unwrap_or("default");

        // Check cache
        if let Some(cached) = self.cache.get(cache_key) {
            // Token is valid for at least 30 more seconds
            if cached.expires_at > std::time::Instant::now() + std::time::Duration::from_secs(30) {
                return Ok(cached.token.clone());
            }
        }

        // Request new token
        let token = self.request_token(scope).await?;

        // Cache token
        let expires_at = std::time::Instant::now()
            + std::time::Duration::from_secs(token.expires_in.unwrap_or(3600));

        self.cache.insert(
            cache_key.to_string(),
            CachedToken {
                token: token.access_token.clone(),
                expires_at,
            },
        );

        Ok(token.access_token)
    }
}

/// In-memory cache of token clients
static CLIENT_CACHE: once_cell::sync::Lazy<DashMap<String, Arc<TokenClient>>> =
    once_cell::sync::Lazy::new(DashMap::new);

/// Get or create a token client
pub fn get_or_create_client(
    client_id: &str,
    client_secret: &str,
    token_endpoint: &str,
) -> Arc<TokenClient> {
    let key = format!("{}:{}", client_id, token_endpoint);

    if let Some(client) = CLIENT_CACHE.get(&key) {
        return client.clone();
    }

    let client = Arc::new(TokenClient::new(
        client_id.to_string(),
        client_secret.to_string(),
        token_endpoint.to_string(),
    ));

    CLIENT_CACHE.insert(key, client.clone());
    client
}
