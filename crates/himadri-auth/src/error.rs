use thiserror::Error;

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("invalid token: {0}")]
    InvalidToken(String),

    #[error("expired token")]
    ExpiredToken,

    #[error("invalid signature")]
    InvalidSignature,

    #[error("invalid audience")]
    InvalidAudience,

    #[error("invalid issuer")]
    InvalidIssuer,

    #[error("OIDC discovery failed: {0}")]
    OidcDiscoveryFailed(String),

    #[error("JWKS fetch failed: {0}")]
    JwksFetchFailed(String),

    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("configuration error: {0}")]
    Config(String),
}
