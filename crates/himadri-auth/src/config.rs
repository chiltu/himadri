use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    /// Enable API key authentication
    #[serde(default = "default_true")]
    pub api_key_enabled: bool,

    /// Master key for admin access
    #[serde(default)]
    pub master_key: Option<String>,

    /// JWT/OIDC configuration
    #[serde(default)]
    pub jwt: Option<JwtConfig>,

    /// OAuth2 configuration
    #[serde(default)]
    pub oauth2: Option<OAuth2Config>,
}

fn default_true() -> bool {
    true
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            api_key_enabled: true,
            master_key: None,
            jwt: None,
            oauth2: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JwtConfig {
    /// Enable JWT authentication
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// OIDC issuer URL (e.g., "https://your-domain.zitadel.cloud")
    pub issuer: String,

    /// Expected audience (client ID)
    pub audience: String,

    /// JWKS refresh interval in seconds
    #[serde(default = "default_jwks_refresh")]
    pub jwks_refresh_interval: u64,

    /// Explicit JWKS URL (bypasses OIDC discovery)
    #[serde(default)]
    pub jwks_uri: Option<String>,
}

fn default_jwks_refresh() -> u64 {
    3600
}

impl Default for JwtConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            issuer: String::new(),
            audience: String::new(),
            jwks_refresh_interval: 3600,
            jwks_uri: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuth2Config {
    /// Enable OAuth2 authentication
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Client ID
    pub client_id: String,

    /// Client secret
    pub client_secret: String,

    /// Token endpoint URL
    pub token_endpoint: String,

    /// Introspection endpoint URL
    #[serde(default)]
    pub introspection_endpoint: Option<String>,
}
