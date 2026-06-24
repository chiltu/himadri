use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::Response,
};
use std::sync::Arc;
use tracing::{debug, warn};

use crate::config::JwtConfig;
use crate::introspect::TokenIntrospector;
use crate::oidc::OidcDiscovery;

/// JWT + OAuth2 authentication middleware for axum
pub struct JwtAuthMiddleware {
    discovery: Arc<OidcDiscovery>,
    #[allow(dead_code)]
    config: JwtConfig,
    introspector: Option<Arc<TokenIntrospector>>,
}

impl JwtAuthMiddleware {
    pub fn new(
        discovery: Arc<OidcDiscovery>,
        config: JwtConfig,
        introspector: Option<Arc<TokenIntrospector>>,
    ) -> Self {
        Self {
            discovery,
            config,
            introspector,
        }
    }

    /// Middleware function for axum
    pub async fn middleware(
        State(auth): State<Arc<Self>>,
        headers: HeaderMap,
        mut request: axum::extract::Request,
        next: Next,
    ) -> Result<Response, StatusCode> {
        // Extract Bearer token
        let token = headers
            .get("Authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));

        let token = match token {
            Some(t) => t.to_string(),
            None => return Err(StatusCode::UNAUTHORIZED),
        };

        // Try JWT validation first
        match auth.discovery.validate_token(&token) {
            Ok(claims) => {
                // Check if token is expired
                if claims.is_expired() {
                    return Err(StatusCode::UNAUTHORIZED);
                }

                // Check if token is not yet valid
                if claims.is_not_yet_valid() {
                    return Err(StatusCode::UNAUTHORIZED);
                }

                // Convert claims to AuthContext
                let auth_ctx = claims.into_auth_context();

                // Add auth context to request extensions
                request.extensions_mut().insert(Some(auth_ctx));

                Ok(next.run(request).await)
            }
            Err(_) => {
                // JWT validation failed, try introspection if available
                if let Some(introspector) = &auth.introspector {
                    debug!("JWT validation failed, attempting token introspection");

                    match introspector.introspect(&token).await {
                        Ok(result) => match result.into_auth_context() {
                            Ok(auth_ctx) => {
                                request.extensions_mut().insert(Some(auth_ctx));
                                Ok(next.run(request).await)
                            }
                            Err(e) => {
                                warn!("Failed to create auth context from introspection: {}", e);
                                Err(StatusCode::UNAUTHORIZED)
                            }
                        },
                        Err(e) => {
                            warn!("Token introspection failed: {}", e);
                            Err(StatusCode::UNAUTHORIZED)
                        }
                    }
                } else {
                    Err(StatusCode::UNAUTHORIZED)
                }
            }
        }
    }
}
