pub mod error;
pub mod jwt;
pub mod oidc;

pub use error::AuthError;
pub use jwt::JwtClaims;
pub use oidc::OidcDiscovery;

#[cfg(test)]
mod tests {
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
    fn test_jwt_claims_zitadel_project_roles() {
        // Zitadel emits granted project roles under a URN-namespaced claim whose
        // value is an object keyed by role name. It should land in `custom` and
        // be surfaced by roles()/into_auth_context().
        let json = serde_json::json!({
            "sub": "user123", "iss": "http://localhost:8080",
            "aud": "client", "exp": 9999999999u64, "iat": 0,
            "urn:zitadel:iam:org:project:roles": {
                "admin": { "orgid1": "example.com" },
                "user": { "orgid1": "example.com" }
            }
        });
        let claims: JwtClaims = serde_json::from_value(json).unwrap();

        let roles = claims.roles();
        assert!(roles.contains(&"admin".to_string()));
        assert!(roles.contains(&"user".to_string()));

        let auth_ctx = claims.into_auth_context();
        assert_eq!(auth_ctx.scope, himadri_core::AuthScope::Admin);
        assert!(auth_ctx.has_role("user"));
        assert!(auth_ctx.has_role("admin"));
    }

    #[test]
    fn test_jwt_claims_project_scoped_roles_claim() {
        // The project-scoped variant urn:...:project:{id}:roles is also honored.
        let json = serde_json::json!({
            "sub": "user123", "iss": "http://localhost:8080",
            "aud": "client", "exp": 9999999999u64, "iat": 0,
            "urn:zitadel:iam:org:project:300000000000000001:roles": {
                "read-only": { "orgid1": "example.com" }
            }
        });
        let claims: JwtClaims = serde_json::from_value(json).unwrap();
        let auth_ctx = claims.into_auth_context();
        assert_eq!(auth_ctx.scope, himadri_core::AuthScope::ReadOnly);
        assert!(auth_ctx.has_role("read-only"));
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
}
