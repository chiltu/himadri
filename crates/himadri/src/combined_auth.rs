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
use himadri_observability::{AuditLog, AuditStatus};
use tracing::{debug, warn};

/// Optional role gate. When `JWT_REQUIRED_ROLES` is set (comma-separated), an
/// authenticated principal must hold at least one of these roles to access the
/// protected `/v1` endpoints. Empty/unset means no role is required (any
/// successfully authenticated principal is allowed), preserving prior behavior.
static REQUIRED_ROLES: once_cell::sync::Lazy<Vec<String>> = once_cell::sync::Lazy::new(|| {
    std::env::var("JWT_REQUIRED_ROLES")
        .ok()
        .map(|v| {
            v.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
});

/// Enforce the optional required-roles gate against an authenticated context.
fn enforce_required_roles(ctx: &AuthContext) -> Result<(), StatusCode> {
    if REQUIRED_ROLES.is_empty() || ctx.has_any_role(&REQUIRED_ROLES) {
        Ok(())
    } else {
        warn!(
            "Principal '{}' lacks any required role {:?}; denying",
            ctx.api_key, *REQUIRED_ROLES
        );
        Err(StatusCode::FORBIDDEN)
    }
}

/// Shared state for the combined auth middleware.
pub struct CombinedAuth {
    api_key: Arc<AuthMiddleware>,
    /// Present when JWT/OIDC auth is enabled.
    jwt: Option<Arc<OidcDiscovery>>,
    /// Records 401/403 auth failures when present.
    audit: Option<Arc<AuditLog>>,
}

impl CombinedAuth {
    pub fn new(
        api_key: Arc<AuthMiddleware>,
        jwt: Option<Arc<OidcDiscovery>>,
        audit: Option<Arc<AuditLog>>,
    ) -> Self {
        Self {
            api_key,
            jwt,
            audit,
        }
    }

    fn audit_failure(
        &self,
        status: AuditStatus,
        reason: &str,
        remote_ip: Option<String>,
        ctx: Option<&AuthContext>,
    ) {
        if let Some(audit) = &self.audit {
            audit.log_auth_failure(
                status,
                reason,
                remote_ip,
                ctx.and_then(|c| c.user_id.clone()),
                ctx.and_then(|c| c.key_id.clone()),
            );
        }
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

        let remote_ip = request
            .extensions()
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|ci| ci.0.ip().to_string());

        let token = headers
            .get("Authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(|t| t.to_string());

        let token = match token {
            Some(t) => t,
            None => {
                request.extensions_mut().insert(None::<AuthContext>);
                auth.audit_failure(
                    AuditStatus::Unauthorized,
                    "missing bearer token",
                    remote_ip,
                    None,
                );
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
                            auth.audit_failure(
                                AuditStatus::Unauthorized,
                                "expired or not-yet-valid token",
                                remote_ip,
                                None,
                            );
                            return Err(StatusCode::UNAUTHORIZED);
                        }
                        let ctx = claims.into_auth_context();
                        if let Err(code) = enforce_required_roles(&ctx) {
                            auth.audit_failure(
                                AuditStatus::Forbidden,
                                "principal lacks a required role",
                                remote_ip,
                                Some(&ctx),
                            );
                            return Err(code);
                        }
                        request.extensions_mut().insert(Some(ctx));
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
                if let Err(code) = enforce_required_roles(&ctx) {
                    auth.audit_failure(
                        AuditStatus::Forbidden,
                        "principal lacks a required role",
                        remote_ip,
                        Some(&ctx),
                    );
                    return Err(code);
                }
                request.extensions_mut().insert(Some(ctx));
                Ok(next.run(request).await)
            }
            Ok(None) => {
                request.extensions_mut().insert(None::<AuthContext>);
                auth.audit_failure(
                    AuditStatus::Unauthorized,
                    "invalid or unknown token",
                    remote_ip,
                    None,
                );
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
