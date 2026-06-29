//! Combined authentication middleware.
//!
//! Layers JWT/OIDC bearer-token validation on top of the existing API-key /
//! master-key authentication. A presented bearer token is first tried as a JWT
//! (when JWT auth is configured); if that fails it falls back to API-key
//! validation. This lets the gateway accept both OIDC access tokens and
//! gateway-issued API keys on the same endpoints.

use std::sync::Arc;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::Response,
};
use himadri_admin::AuthMiddleware;
use himadri_auth::OidcDiscovery;
use himadri_core::AuthContext;
use tracing::debug;

/// Shared state for the combined auth middleware.
pub struct CombinedAuth {
    api_key: Arc<AuthMiddleware>,
    /// Present when JWT/OIDC auth is enabled.
    jwt: Option<Arc<OidcDiscovery>>,
}

impl CombinedAuth {
    pub fn new(api_key: Arc<AuthMiddleware>, jwt: Option<Arc<OidcDiscovery>>) -> Self {
        Self { api_key, jwt }
    }

    pub async fn middleware(
        State(auth): State<Arc<Self>>,
        headers: HeaderMap,
        mut request: axum::extract::Request,
        next: Next,
    ) -> Result<Response, StatusCode> {
        // Dev-mode bypass mirrors AuthMiddleware: no master key => anonymous.
        if auth.jwt.is_none() && auth.api_key.is_bypass() {
            request
                .extensions_mut()
                .insert(Some(AuthContext::anonymous()));
            return Ok(next.run(request).await);
        }

        let token = headers
            .get("Authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(|t| t.to_string());

        let token = match token {
            Some(t) => t,
            None => {
                request.extensions_mut().insert(None::<AuthContext>);
                return Err(StatusCode::UNAUTHORIZED);
            }
        };

        // Try JWT validation first when configured and the token looks like a
        // JWT (three dot-separated segments). Non-JWT tokens fall through to the
        // API-key path without a wasted parse.
        if let Some(discovery) = &auth.jwt {
            if looks_like_jwt(&token) {
                match discovery.validate_token(&token) {
                    Ok(claims) => {
                        if claims.is_expired() || claims.is_not_yet_valid() {
                            return Err(StatusCode::UNAUTHORIZED);
                        }
                        request
                            .extensions_mut()
                            .insert(Some(claims.into_auth_context()));
                        return Ok(next.run(request).await);
                    }
                    Err(e) => {
                        debug!("JWT validation failed, trying API key: {}", e);
                    }
                }
            }
        }

        // Fall back to API-key / master-key validation.
        match auth.api_key.authenticate(&token).await {
            Ok(Some(ctx)) => {
                request.extensions_mut().insert(Some(ctx));
                Ok(next.run(request).await)
            }
            Ok(None) => {
                request.extensions_mut().insert(None::<AuthContext>);
                Err(StatusCode::UNAUTHORIZED)
            }
            Err(()) => {
                request.extensions_mut().insert(None::<AuthContext>);
                Err(StatusCode::INTERNAL_SERVER_ERROR)
            }
        }
    }
}

/// A JWT is three non-empty base64url segments separated by dots. Gateway API
/// keys never have this shape, so this cheaply routes a token to the JWT path
/// vs. the API-key path without an expensive parse.
fn looks_like_jwt(token: &str) -> bool {
    let mut parts = token.split('.');
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next()),
        (Some(a), Some(b), Some(c), None) if !a.is_empty() && !b.is_empty() && !c.is_empty()
    )
}

#[cfg(test)]
mod tests {
    use super::looks_like_jwt;

    #[test]
    fn recognizes_jwt_shaped_tokens() {
        assert!(looks_like_jwt("header.payload.signature"));
    }

    #[test]
    fn rejects_api_key_shaped_tokens() {
        assert!(!looks_like_jwt("sk-abc123def456"));
        assert!(!looks_like_jwt("header.payload")); // only 2 segments
        assert!(!looks_like_jwt("a.b.c.d")); // 4 segments
        assert!(!looks_like_jwt("a..c")); // empty segment
        assert!(!looks_like_jwt(""));
    }
}
