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

        let keys = fetch_jwks(&client, &jwks_uri).await?;

        let discovery = Arc::new(Self {
            issuer: issuer.to_string(),
            audience: audience.to_string(),
            jwks_uri,
            jwks: RwLock::new(keys),
            last_refresh: parking_lot::Mutex::new(std::time::Instant::now()),
            client,
        });

        debug!(
            "OIDC discovery initialized with {} keys",
            discovery.jwks.read().len()
        );

        Ok(discovery)
    }

    /// Build a discovery instance from an already-known JWKS, without any
    /// network I/O. Used by tests to validate tokens against locally
    /// generated keys; also useful for air-gapped deployments that pin keys.
    pub fn from_parts(
        issuer: &str,
        audience: &str,
        keys: Vec<jsonwebtoken::jwk::Jwk>,
    ) -> Arc<Self> {
        Arc::new(Self {
            issuer: issuer.to_string(),
            audience: audience.to_string(),
            jwks_uri: String::new(),
            jwks: RwLock::new(keys),
            last_refresh: parking_lot::Mutex::new(std::time::Instant::now()),
            client: reqwest::Client::new(),
        })
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

        // Decode and validate. The algorithm comes from the matched JWK
        // (falling back to the key type's default) rather than being pinned
        // to RS256, so an IdP rotating to RS384/512, ES256/384 or EdDSA
        // keeps working.
        let algorithm = algorithm_for_jwk(jwk).ok_or_else(|| {
            AuthError::InvalidToken(format!("unsupported JWK algorithm for kid {}", kid))
        })?;
        let mut validation = jsonwebtoken::Validation::new(algorithm);
        // jsonwebtoken validates `exp` by default but NOT `nbf` — enable it
        // so pre-provisioned / clock-skewed tokens are rejected until valid.
        validation.validate_nbf = true;
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

        let keys = fetch_jwks(&self.client, &self.jwks_uri).await?;

        let mut current_jwks = self.jwks.write();
        *current_jwks = keys;

        let mut last_refresh = self.last_refresh.lock();
        *last_refresh = std::time::Instant::now();

        debug!("JWKS refreshed with {} keys", current_jwks.len());

        Ok(())
    }
}

/// Fetch and parse a JWKS document, mapping transport/parse failures to
/// [`AuthError::JwksFetchFailed`]. Shared by initial discovery and refresh.
async fn fetch_jwks(
    client: &reqwest::Client,
    uri: &str,
) -> Result<Vec<jsonwebtoken::jwk::Jwk>, AuthError> {
    let set: jsonwebtoken::jwk::JwkSet = client
        .get(uri)
        .send()
        .await
        .map_err(|e| AuthError::JwksFetchFailed(e.to_string()))?
        .json()
        .await
        .map_err(|e| AuthError::JwksFetchFailed(e.to_string()))?;
    Ok(set.keys)
}

/// Resolve the signature algorithm to validate with for a JWK: the key's
/// explicit `alg` when present, otherwise a default derived from the key
/// type/curve.
fn algorithm_for_jwk(jwk: &jsonwebtoken::jwk::Jwk) -> Option<jsonwebtoken::Algorithm> {
    use jsonwebtoken::jwk::{AlgorithmParameters, EllipticCurve, KeyAlgorithm};
    use jsonwebtoken::Algorithm;

    if let Some(alg) = jwk.common.key_algorithm {
        return match alg {
            KeyAlgorithm::RS256 => Some(Algorithm::RS256),
            KeyAlgorithm::RS384 => Some(Algorithm::RS384),
            KeyAlgorithm::RS512 => Some(Algorithm::RS512),
            KeyAlgorithm::PS256 => Some(Algorithm::PS256),
            KeyAlgorithm::PS384 => Some(Algorithm::PS384),
            KeyAlgorithm::PS512 => Some(Algorithm::PS512),
            KeyAlgorithm::ES256 => Some(Algorithm::ES256),
            KeyAlgorithm::ES384 => Some(Algorithm::ES384),
            KeyAlgorithm::EdDSA => Some(Algorithm::EdDSA),
            _ => None,
        };
    }
    match &jwk.algorithm {
        AlgorithmParameters::RSA(_) => Some(Algorithm::RS256),
        AlgorithmParameters::EllipticCurve(ec) => match ec.curve {
            EllipticCurve::P256 => Some(Algorithm::ES256),
            EllipticCurve::P384 => Some(Algorithm::ES384),
            _ => None,
        },
        AlgorithmParameters::OctetKeyPair(_) => Some(Algorithm::EdDSA),
        _ => None,
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
