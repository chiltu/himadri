//! Development / break-glass admin login.
//!
//! When `DEV_ADMIN_PASSWORD` is set, the gateway exposes a username+password
//! login (`POST /auth/admin/login`, wired in the binary) that issues a
//! short-lived admin JWT. Two intended uses:
//!
//! 1. **Development** — configure the whole gateway from the dashboard
//!    without standing up an OIDC provider.
//! 2. **Break-glass** — regain administrator access when the OIDC provider
//!    is down or misconfigured and normal login is impossible.
//!
//! Tokens are signed HS256 with a **random per-boot secret**: there is no
//! signing key to configure or leak, every token dies on restart (restart the
//! gateway to revoke all break-glass sessions), and a token can never be
//! confused with one from a real issuer. Validation happens in the combined
//! auth middleware alongside OIDC tokens and API keys.
//!
//! Environment:
//! - `DEV_ADMIN_PASSWORD` — enables the mechanism (non-empty).
//! - `DEV_ADMIN_USERNAME` — login name, default `admin`.
//! - `DEV_ADMIN_TOKEN_TTL_SECS` — token lifetime, default 43200 (12h).

use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};

use crate::error::AuthError;
use crate::jwt::JwtClaims;

/// `iss`/`aud` for locally issued admin tokens. Namespaced so they can never
/// collide with a real OIDC issuer, and checked on validation so an OIDC
/// token can never take this code path.
pub const DEV_ADMIN_ISSUER: &str = "himadri:dev-admin-login";
pub const DEV_ADMIN_AUDIENCE: &str = "himadri:dashboard";

const DEFAULT_TTL_SECS: u64 = 12 * 60 * 60;

/// A successfully issued login token, shaped like an OAuth token response.
#[derive(Debug, serde::Serialize)]
pub struct IssuedAdminToken {
    pub access_token: String,
    pub token_type: &'static str,
    pub expires_in: u64,
}

pub struct AdminLogin {
    username: String,
    password: String,
    ttl_secs: u64,
    encoding: EncodingKey,
    decoding: DecodingKey,
}

impl AdminLogin {
    /// Enabled iff `DEV_ADMIN_PASSWORD` is set and non-empty.
    pub fn from_env() -> Option<Self> {
        let password = std::env::var("DEV_ADMIN_PASSWORD")
            .ok()
            .filter(|p| !p.trim().is_empty())?;
        let username = std::env::var("DEV_ADMIN_USERNAME").unwrap_or_else(|_| "admin".to_string());
        let ttl_secs = std::env::var("DEV_ADMIN_TOKEN_TTL_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|&t| t > 0)
            .unwrap_or(DEFAULT_TTL_SECS);
        Some(Self::new(username, password, ttl_secs))
    }

    pub fn new(username: String, password: String, ttl_secs: u64) -> Self {
        // Fresh random signing secret per instance/boot; see the module docs.
        let secret: [u8; 32] = rand::random();
        Self {
            username,
            password,
            ttl_secs,
            encoding: EncodingKey::from_secret(&secret),
            decoding: DecodingKey::from_secret(&secret),
        }
    }

    /// The login name, for startup logging (never log the password).
    pub fn username(&self) -> &str {
        &self.username
    }

    /// Constant-time credential check.
    pub fn verify(&self, username: &str, password: &str) -> bool {
        // Evaluate both comparisons unconditionally so a valid username can't
        // be probed via response timing.
        let user_ok = ct_eq(username.as_bytes(), self.username.as_bytes());
        let pass_ok = ct_eq(password.as_bytes(), self.password.as_bytes());
        user_ok & pass_ok
    }

    /// Issue an admin token. Callers must have checked [`Self::verify`] first.
    pub fn issue(&self) -> Result<IssuedAdminToken, AuthError> {
        let now = chrono::Utc::now().timestamp() as u64;
        let claims = JwtClaims {
            sub: self.username.clone(),
            iss: DEV_ADMIN_ISSUER.to_string(),
            aud: DEV_ADMIN_AUDIENCE.to_string(),
            exp: now + self.ttl_secs,
            iat: now,
            nbf: None,
            jti: None,
            scope: None,
            org_id: None,
            team_id: None,
            email: None,
            email_verified: None,
            // `into_auth_context` derives `AuthScope::Admin` from this role.
            roles: Some(vec!["admin".to_string()]),
            rate_limit_rpm: None,
            budget_limit_usd: None,
            custom: Default::default(),
        };
        let access_token = encode(&Header::new(Algorithm::HS256), &claims, &self.encoding)
            .map_err(|e| AuthError::InvalidToken(e.to_string()))?;
        Ok(IssuedAdminToken {
            access_token,
            token_type: "Bearer",
            expires_in: self.ttl_secs,
        })
    }

    /// Validate a locally issued admin token (signature, expiry, issuer,
    /// audience). Tokens from any other issuer — including real OIDC tokens —
    /// fail here and fall through to the other auth paths.
    pub fn validate(&self, token: &str) -> Result<JwtClaims, AuthError> {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_issuer(&[DEV_ADMIN_ISSUER]);
        validation.set_audience(&[DEV_ADMIN_AUDIENCE]);
        validation.leeway = 30;
        decode::<JwtClaims>(token, &self.decoding, &validation)
            .map(|data| data.claims)
            .map_err(|e| AuthError::InvalidToken(e.to_string()))
    }
}

/// Constant-time byte comparison; the accumulator also folds in the length
/// difference so equal-prefix probing gains nothing.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = (a.len() ^ b.len()) as u8;
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn login() -> AdminLogin {
        AdminLogin::new("admin".to_string(), "hunter2".to_string(), 3600)
    }

    #[test]
    fn issued_token_round_trips_as_admin() {
        let l = login();
        assert!(l.verify("admin", "hunter2"));
        let issued = l.issue().unwrap();
        assert_eq!(issued.token_type, "Bearer");
        assert_eq!(issued.expires_in, 3600);

        let claims = l.validate(&issued.access_token).unwrap();
        assert_eq!(claims.sub, "admin");
        assert_eq!(claims.iss, DEV_ADMIN_ISSUER);

        let ctx = claims.into_auth_context();
        assert_eq!(ctx.scope, himadri_core::AuthScope::Admin);
        assert!(ctx.roles.contains(&"admin".to_string()));
    }

    #[test]
    fn rejects_wrong_credentials() {
        let l = login();
        assert!(!l.verify("admin", "wrong"));
        assert!(!l.verify("root", "hunter2"));
        assert!(!l.verify("admin", "hunter2X"));
        assert!(!l.verify("admin", "hunter"));
        assert!(!l.verify("", ""));
    }

    #[test]
    fn rejects_tokens_from_another_instance() {
        // A different instance means a different per-boot secret; its tokens
        // must not validate here (this is also what invalidates all tokens on
        // gateway restart).
        let issued = login().issue().unwrap();
        assert!(login().validate(&issued.access_token).is_err());
    }

    #[test]
    fn rejects_tampered_tokens() {
        let l = login();
        let mut token = l.issue().unwrap().access_token;
        token.pop();
        assert!(l.validate(&token).is_err());
        assert!(l.validate("not.a.jwt").is_err());
    }
}
