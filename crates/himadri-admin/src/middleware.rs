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

    pub async fn middleware(
        State(auth): State<Arc<Self>>,
        headers: HeaderMap,
        mut request: axum::extract::Request,
        next: Next,
    ) -> Result<Response, StatusCode> {
        // If no master_key configured, bypass auth (testing mode)
        if auth.master_key.is_none() {
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

        if let Some(master_key) = &auth.master_key {
            if constant_time_eq(api_key.as_bytes(), master_key.as_bytes()) {
                let ctx = AuthContext {
                    api_key,
                    key_id: None,
                    scope: AuthScope::Admin,
                    org_id: None,
                    team_id: None,
                    user_id: None,
                    rate_limit_override: None,
                };
                request.extensions_mut().insert(Some(ctx));
                return Ok(next.run(request).await);
            }
        }

        match auth.store.validate(&api_key).await {
            Ok(Some(key)) => {
                let rate_limit_override = key.rate_limit_override.map(|r| RateLimitOverride {
                    requests_per_second: r.requests_per_second,
                    burst_size: r.burst_size,
                });
                let ctx = AuthContext {
                    api_key,
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
                };
                request.extensions_mut().insert(Some(ctx));
                Ok(next.run(request).await)
            }
            Ok(None) => {
                request.extensions_mut().insert(None::<AuthContext>);
                Err(StatusCode::UNAUTHORIZED)
            }
            Err(_) => {
                request.extensions_mut().insert(None::<AuthContext>);
                Err(StatusCode::INTERNAL_SERVER_ERROR)
            }
        }
    }
}
