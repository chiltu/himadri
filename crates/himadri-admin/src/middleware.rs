use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::Response,
};
use std::sync::Arc;

use crate::store::StoreBackend;
use himadri_core::{AuthContext, AuthScope, RateLimitOverride};

/// Constant-time byte comparison to prevent timing side-channel attacks.
/// Returns false immediately if lengths differ (length is not secret),
/// otherwise compares all bytes using XOR accumulation.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut result = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        result |= x ^ y;
    }
    result == 0
}

pub struct AuthMiddleware {
    store: StoreBackend,
    master_key: Option<String>,
}

impl AuthMiddleware {
    pub fn new(store: StoreBackend, master_key: Option<String>) -> Self {
        Self { store, master_key }
    }

    /// Whether authentication is bypassed (no master key configured — dev mode).
    pub fn is_bypass(&self) -> bool {
        self.master_key.is_none()
    }

    /// Validate a bearer token as either the master key or a stored API key.
    ///
    /// Returns `Ok(Some(ctx))` for a valid token, `Ok(None)` for an unknown or
    /// invalid token, and `Err(())` on a store backend error.
    pub async fn authenticate(&self, api_key: &str) -> Result<Option<AuthContext>, ()> {
        if let Some(master_key) = &self.master_key {
            if constant_time_eq(api_key.as_bytes(), master_key.as_bytes()) {
                return Ok(Some(AuthContext {
                    api_key: api_key.to_string(),
                    key_id: None,
                    scope: AuthScope::Admin,
                    org_id: None,
                    team_id: None,
                    user_id: None,
                    rate_limit_override: None,
                    roles: vec!["admin".to_string()],
                    budget_limit_usd: None,
                }));
            }
        }

        match self.store.validate(api_key).await {
            Ok(Some(key)) => {
                let rate_limit_override = key.rate_limit_override.map(|r| RateLimitOverride {
                    requests_per_second: r.requests_per_second,
                    burst_size: r.burst_size,
                });
                Ok(Some(AuthContext {
                    api_key: api_key.to_string(),
                    key_id: Some(key.id),
                    scope: if key.scopes.contains(&"admin".to_string()) {
                        AuthScope::Admin
                    } else if key.scopes.contains(&"read-only".to_string()) {
                        AuthScope::ReadOnly
                    } else {
                        AuthScope::ApiKey
                    },
                    org_id: key.org_id,
                    team_id: key.team_id,
                    user_id: key.user_id,
                    rate_limit_override,
                    roles: key.scopes,
                    budget_limit_usd: key
                        .token_budget
                        .as_ref()
                        .and_then(|b| b.cost_limit_per_month),
                }))
            }
            Ok(None) => Ok(None),
            Err(_) => Err(()),
        }
    }

    pub async fn middleware(
        State(auth): State<Arc<Self>>,
        headers: HeaderMap,
        mut request: axum::extract::Request,
        next: Next,
    ) -> Result<Response, StatusCode> {
        // If no master_key configured, bypass auth (testing mode)
        if auth.is_bypass() {
            let ctx = AuthContext::anonymous();
            request.extensions_mut().insert(Some(ctx));
            return Ok(next.run(request).await);
        }

        let auth_header = headers
            .get("Authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));

        let api_key = match auth_header {
            Some(key) => key.to_string(),
            None => {
                request.extensions_mut().insert(None::<AuthContext>);
                return Err(StatusCode::UNAUTHORIZED);
            }
        };

        match auth.authenticate(&api_key).await {
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
