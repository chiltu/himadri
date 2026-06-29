use dashmap::DashMap;
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Duration;
use tracing::debug;

use crate::error::AuthError;
use crate::jwt::JwtClaims;

/// OIDC discovery and JWKS management
pub struct OidcDiscovery {
    issuer: String,
    audience: String,
    jwks_uri: String,
    jwks: RwLock<Vec<jsonwebtoken::jwk::Jwk>>,
    last_refresh: parking_lot::Mutex<std::time::Instant>,
    client: reqwest::Client,
}

#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)]
struct OidcConfig {
    issuer: String,
    jwks_uri: String,
    #[allow(dead_code)]
    token_endpoint: Option<String>,
    #[allow(dead_code)]
    authorization_endpoint: Option<String>,
}

impl OidcDiscovery {
    /// Create a new OIDC discovery instance.
    ///
    /// `audience` is the expected `aud` claim (typically the OAuth client id).
    pub async fn new(
        issuer: &str,
        audience: &str,
        jwks_uri: Option<&str>,
    ) -> Result<Arc<Self>, AuthError> {
        let client = reqwest::Client::new();

        let jwks_uri = if let Some(uri) = jwks_uri {
            uri.to_string()
        } else {
            // Discover OIDC endpoints
            let discovery_url = format!("{}/.well-known/openid-configuration", issuer);
            debug!("Fetching OIDC discovery from {}", discovery_url);

            let config: OidcConfig = client
                .get(&discovery_url)
                .send()
                .await?
                .json()
                .await
                .map_err(|e| AuthError::OidcDiscoveryFailed(e.to_string()))?;

            config.jwks_uri
        };

        debug!("Fetching JWKS from {}", jwks_uri);

        // Fetch initial JWKS
        let jwks: jsonwebtoken::jwk::JwkSet = client
            .get(&jwks_uri)
            .send()
            .await
            .map_err(|e| AuthError::JwksFetchFailed(e.to_string()))?
            .json()
            .await
            .map_err(|e| AuthError::JwksFetchFailed(e.to_string()))?;

        let discovery = Arc::new(Self {
            issuer: issuer.to_string(),
            audience: audience.to_string(),
            jwks_uri,
            jwks: RwLock::new(jwks.keys),
            last_refresh: parking_lot::Mutex::new(std::time::Instant::now()),
            client,
        });

        debug!(
            "OIDC discovery initialized with {} keys",
            discovery.jwks.read().len()
        );

        Ok(discovery)
    }

    /// Validate a JWT token and extract claims
    pub fn validate_token(&self, token: &str) -> Result<JwtClaims, AuthError> {
        // Decode header to get kid
        let header = jsonwebtoken::decode_header(token)
            .map_err(|e| AuthError::InvalidToken(e.to_string()))?;

        let kid = header
            .kid
            .ok_or_else(|| AuthError::InvalidToken("missing kid in header".to_string()))?;

        // Find the matching key
        let jwks = self.jwks.read();
        let jwk = jwks
            .iter()
            .find(|k| k.common.key_id.as_deref() == Some(&kid))
            .ok_or_else(|| AuthError::InvalidToken(format!("key not found: {}", kid)))?;

        // Decode and validate
        let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
        validation.set_audience(&[&self.audience]);
        validation.set_issuer(&[&self.issuer]);

        let token_data = jsonwebtoken::decode::<JwtClaims>(
            token,
            &jsonwebtoken::DecodingKey::from_jwk(jwk)
                .map_err(|e| AuthError::InvalidToken(e.to_string()))?,
            &validation,
        )
        .map_err(|e| match e.kind() {
            jsonwebtoken::errors::ErrorKind::ExpiredSignature => AuthError::ExpiredToken,
            jsonwebtoken::errors::ErrorKind::InvalidSignature => AuthError::InvalidSignature,
            jsonwebtoken::errors::ErrorKind::InvalidAudience => AuthError::InvalidAudience,
            jsonwebtoken::errors::ErrorKind::InvalidIssuer => AuthError::InvalidIssuer,
            _ => AuthError::InvalidToken(e.to_string()),
        })?;

        Ok(token_data.claims)
    }

    /// Refresh JWKS if needed
    pub async fn refresh_if_needed(&self, interval: Duration) -> Result<(), AuthError> {
        let should_refresh = {
            let last = self.last_refresh.lock();
            last.elapsed() > interval
        };

        if should_refresh {
            self.refresh_jwks().await?;
        }

        Ok(())
    }

    /// Force refresh JWKS
    pub async fn refresh_jwks(&self) -> Result<(), AuthError> {
        debug!("Refreshing JWKS from {}", self.jwks_uri);

        let jwks: jsonwebtoken::jwk::JwkSet = self
            .client
            .get(&self.jwks_uri)
            .send()
            .await
            .map_err(|e| AuthError::JwksFetchFailed(e.to_string()))?
            .json()
            .await
            .map_err(|e| AuthError::JwksFetchFailed(e.to_string()))?;

        let mut current_jwks = self.jwks.write();
        *current_jwks = jwks.keys;

        let mut last_refresh = self.last_refresh.lock();
        *last_refresh = std::time::Instant::now();

        debug!("JWKS refreshed with {} keys", current_jwks.len());

        Ok(())
    }
}

/// In-memory cache of OIDC discovery instances
static DISCOVERY_CACHE: once_cell::sync::Lazy<DashMap<String, Arc<OidcDiscovery>>> =
    once_cell::sync::Lazy::new(DashMap::new);

/// Get or create an OIDC discovery instance
pub async fn get_or_create_discovery(
    issuer: &str,
    audience: &str,
    jwks_uri: Option<&str>,
) -> Result<Arc<OidcDiscovery>, AuthError> {
    let cache_key = format!("{}|{}", issuer, audience);
    if let Some(discovery) = DISCOVERY_CACHE.get(&cache_key) {
        return Ok(discovery.clone());
    }

    let discovery = OidcDiscovery::new(issuer, audience, jwks_uri).await?;
    DISCOVERY_CACHE.insert(cache_key, discovery.clone());

    Ok(discovery)
}
