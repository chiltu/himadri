pub mod config;
pub mod error;
pub mod introspect;
pub mod jwt;
pub mod middleware;
pub mod oauth2_client;
pub mod oidc;
pub mod zitadel;

pub use config::AuthConfig;
pub use config::JwtConfig;
pub use error::AuthError;
pub use introspect::TokenIntrospector;
pub use jwt::JwtClaims;
pub use middleware::JwtAuthMiddleware;
pub use oauth2_client::TokenClient;
pub use oidc::OidcDiscovery;
pub use zitadel::ZitadelClaims;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jwt::JwtClaims;

    fn make_test_claims(exp: u64) -> JwtClaims {
        JwtClaims {
            sub: "user123".to_string(),
            iss: "https://example.com".to_string(),
            aud: "client123".to_string(),
            exp,
            iat: 0,
            nbf: None,
            jti: None,
            scope: None,
            org_id: None,
            team_id: None,
            email: None,
            email_verified: None,
            roles: None,
            rate_limit_rpm: None,
            budget_limit_usd: None,
            custom: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn test_jwt_claims_aud_accepts_array() {
        // Zitadel (and many OIDC providers) issue `aud` as an array. RFC 7519
        // permits both a string and an array of strings.
        let json = serde_json::json!({
            "sub": "u1", "iss": "http://localhost:8080",
            "aud": ["proj-123", "client-456"],
            "exp": 9999999999u64, "iat": 0
        });
        let claims: JwtClaims = serde_json::from_value(json).unwrap();
        assert_eq!(claims.aud, "proj-123,client-456");
    }

    #[test]
    fn test_jwt_claims_aud_accepts_string() {
        let json = serde_json::json!({
            "sub": "u1", "iss": "http://localhost:8080",
            "aud": "single-aud", "exp": 9999999999u64, "iat": 0
        });
        let claims: JwtClaims = serde_json::from_value(json).unwrap();
        assert_eq!(claims.aud, "single-aud");
    }

    #[test]
    fn test_jwt_claims_is_expired() {
        assert!(make_test_claims(0).is_expired());
    }

    #[test]
    fn test_jwt_claims_is_not_expired() {
        assert!(!make_test_claims(9999999999).is_expired());
    }

    #[test]
    fn test_jwt_claims_scopes() {
        let mut claims = make_test_claims(9999999999);
        claims.scope = Some("openid profile email admin".to_string());

        let scopes = claims.scopes();
        assert_eq!(scopes.len(), 4);
        assert!(scopes.contains(&"openid".to_string()));
        assert!(scopes.contains(&"admin".to_string()));
    }

    #[test]
    fn test_jwt_claims_into_auth_context() {
        let mut claims = make_test_claims(9999999999);
        claims.scope = Some("openid admin".to_string());
        claims.org_id = Some("org123".to_string());
        claims.team_id = Some("team456".to_string());

        let auth_ctx = claims.into_auth_context();
        assert_eq!(auth_ctx.api_key, "jwt:user123");
        assert_eq!(auth_ctx.key_id, Some("user123".to_string()));
        assert_eq!(auth_ctx.org_id, Some("org123".to_string()));
        assert_eq!(auth_ctx.team_id, Some("team456".to_string()));
    }

    #[test]
    fn test_jwt_claims_rate_limit_override() {
        let mut claims = make_test_claims(9999999999);
        claims.rate_limit_rpm = Some(600);

        let auth_ctx = claims.into_auth_context();
        let rl = auth_ctx.rate_limit_override.unwrap();
        assert_eq!(rl.requests_per_second, Some(10)); // 600/60
        assert_eq!(rl.burst_size, Some(600));
    }

    #[test]
    fn test_auth_config_default() {
        let config = AuthConfig::default();
        assert!(config.api_key_enabled);
        assert!(config.master_key.is_none());
        assert!(config.jwt.is_none());
        assert!(config.oauth2.is_none());
    }

    #[test]
    fn test_jwt_config_default() {
        let config = JwtConfig::default();
        assert!(!config.enabled);
        assert!(config.issuer.is_empty());
        assert!(config.audience.is_empty());
        assert_eq!(config.jwks_refresh_interval, 3600);
    }

    #[test]
    fn test_introspection_result_into_auth_context() {
        use crate::introspect::IntrospectionResult;

        let result = IntrospectionResult {
            active: true,
            scope: Some("openid admin".to_string()),
            client_id: Some("client123".to_string()),
            username: Some("user@example.com".to_string()),
            token_type: Some("Bearer".to_string()),
            exp: Some(9999999999),
            iat: Some(0),
            sub: Some("user123".to_string()),
            aud: Some("client123".to_string()),
            iss: Some("https://example.com".to_string()),
            jti: None,
        };

        let auth_ctx = result.into_auth_context().unwrap();
        assert_eq!(auth_ctx.api_key, "oauth2:user123");
        assert_eq!(auth_ctx.key_id, Some("client123".to_string()));
        assert_eq!(auth_ctx.user_id, Some("user123".to_string()));
    }

    #[test]
    fn test_introspection_inactive_token() {
        use crate::introspect::IntrospectionResult;

        let result = IntrospectionResult {
            active: false,
            scope: None,
            client_id: None,
            username: None,
            token_type: None,
            exp: None,
            iat: None,
            sub: None,
            aud: None,
            iss: None,
            jti: None,
        };

        assert!(result.into_auth_context().is_err());
    }

    #[test]
    fn test_zitadel_claims() {
        use crate::zitadel::ZitadelClaims;

        let mut claims = make_test_claims(9999999999);
        claims.scope = Some("openid admin".to_string());
        claims.rate_limit_rpm = Some(1200);
        claims.budget_limit_usd = Some(50.0);
        claims
            .custom
            .insert("zitadel_org_id".to_string(), serde_json::json!("org-abc"));
        claims.custom.insert(
            "zitadel_project_id".to_string(),
            serde_json::json!("proj-123"),
        );

        let zitadel = ZitadelClaims::from_jwt_claims(&claims);
        assert_eq!(zitadel.zitadel_org_id, Some("org-abc".to_string()));
        assert_eq!(zitadel.zitadel_project_id, Some("proj-123".to_string()));
        assert_eq!(zitadel.rate_limit_rpm, Some(1200));
        assert_eq!(zitadel.budget_limit_usd, Some(50.0));

        let auth_ctx = zitadel.into_auth_context();
        assert_eq!(auth_ctx.org_id, Some("org-abc".to_string()));
        assert_eq!(auth_ctx.team_id, Some("proj-123".to_string()));

        let rl = auth_ctx.rate_limit_override.unwrap();
        assert_eq!(rl.requests_per_second, Some(20)); // 1200/60
        assert_eq!(rl.burst_size, Some(1200));
    }

    #[test]
    fn test_zitadel_claims_no_rate_limit() {
        use crate::zitadel::ZitadelClaims;

        let claims = make_test_claims(9999999999);
        let zitadel = ZitadelClaims::from_jwt_claims(&claims);
        let auth_ctx = zitadel.into_auth_context();
        assert!(auth_ctx.rate_limit_override.is_none());
    }
}
