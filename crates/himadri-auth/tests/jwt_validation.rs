//! Offline OIDC/JWT validation tests (finding 5.4): the IdP is mocked by
//! injecting a locally generated JWKS via `OidcDiscovery::from_parts`, and
//! tokens are signed with the matching RSA test key — no network involved.
//!
//! The RSA key in `test_rsa.pem` is a throwaway generated for these tests.

use himadri_auth::{JwtClaims, OidcDiscovery};
use jsonwebtoken::{encode, EncodingKey, Header};

const ISSUER: &str = "https://idp.test";
const AUDIENCE: &str = "himadri-gateway";
const KID: &str = "test-key-1";
const TEST_RSA_PEM: &[u8] = include_bytes!("test_rsa.pem");

fn jwk_json(kid: &str) -> serde_json::Value {
    serde_json::json!({
        "kty": "RSA",
        "use": "sig",
        "alg": "RS256",
        "kid": kid,
        "n": "ztpPHM_0WVINwodVnggKm5OKyYe9gTkS1le5UR-jXxqKkr8jHiQbJ0RCkUP084N9LEShsLBfoVmwbcVqWnU9MBypyR4rebtwGDfxMCX_UgUVYofw9tyyIu3XBrhwUmuqqY68cJdSeEAzVVS1XYjT-SWAITLszH7Q2EIQ3VqrIWoOIj9XeVew8FJi0Id0syVX0isZYys1EgYMENNGS-gCZ7gfEYOhG3tGfzmiIa60p-jigesAtiMLSbznZwFkvjIhqcOVbHY9XTIuNdTCc3omBd_7_i3iVpOqYbpEG5oIXGi2GnkLZlkezctmWTCa3dIHb9GivrN5VO-q-UWqtSfpjw",
        "e": "AQAB",
    })
}

fn discovery() -> std::sync::Arc<OidcDiscovery> {
    let jwk: jsonwebtoken::jwk::Jwk = serde_json::from_value(jwk_json(KID)).unwrap();
    OidcDiscovery::from_parts(ISSUER, AUDIENCE, vec![jwk])
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn sign(claims: &serde_json::Value, kid: &str) -> String {
    let mut header = Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some(kid.to_string());
    encode(
        &header,
        claims,
        &EncodingKey::from_rsa_pem(TEST_RSA_PEM).unwrap(),
    )
    .unwrap()
}

fn base_claims() -> serde_json::Value {
    serde_json::json!({
        "sub": "user-1",
        "iss": ISSUER,
        "aud": AUDIENCE,
        "exp": now() + 600,
        "iat": now(),
        "roles": ["admin"],
    })
}

#[test]
fn valid_token_is_accepted_and_maps_roles() {
    let token = sign(&base_claims(), KID);
    let claims: JwtClaims = discovery().validate_token(&token).unwrap();
    assert_eq!(claims.sub, "user-1");
    assert!(claims.roles().contains(&"admin".to_string()));

    let ctx = claims.into_auth_context();
    assert_eq!(ctx.scope, himadri_core::AuthScope::Admin);
    assert_eq!(ctx.key_id.as_deref(), Some("user-1"));
}

#[test]
fn expired_token_is_rejected() {
    let mut claims = base_claims();
    claims["exp"] = serde_json::json!(now() - 3600);
    claims["iat"] = serde_json::json!(now() - 7200);
    let token = sign(&claims, KID);
    assert!(discovery().validate_token(&token).is_err());
}

#[test]
fn wrong_audience_is_rejected() {
    let mut claims = base_claims();
    claims["aud"] = serde_json::json!("some-other-app");
    let token = sign(&claims, KID);
    assert!(discovery().validate_token(&token).is_err());
}

#[test]
fn wrong_issuer_is_rejected() {
    let mut claims = base_claims();
    claims["iss"] = serde_json::json!("https://evil.test");
    let token = sign(&claims, KID);
    assert!(discovery().validate_token(&token).is_err());
}

#[test]
fn unknown_kid_is_rejected() {
    let token = sign(&base_claims(), "not-in-jwks");
    assert!(discovery().validate_token(&token).is_err());
}

#[test]
fn tampered_token_is_rejected() {
    let token = sign(&base_claims(), KID);
    // Corrupt the payload segment: signature no longer matches.
    let mut parts: Vec<&str> = token.split('.').collect();
    let tampered_payload = format!("{}x", parts[1]);
    parts[1] = &tampered_payload;
    let tampered = parts.join(".");
    assert!(discovery().validate_token(&tampered).is_err());
}

#[test]
fn zitadel_project_roles_are_collected() {
    let mut claims = base_claims();
    claims["roles"] = serde_json::Value::Null;
    claims["urn:zitadel:iam:org:project:roles"] =
        serde_json::json!({ "gateway-admin": { "org1": "example.org" } });
    let token = sign(&claims, KID);
    let claims: JwtClaims = discovery().validate_token(&token).unwrap();
    assert!(claims.roles().contains(&"gateway-admin".to_string()));
    assert_eq!(
        claims.into_auth_context().scope,
        himadri_core::AuthScope::Admin
    );
}

#[test]
fn rpm_under_60_does_not_truncate_to_zero_rps() {
    let mut claims = base_claims();
    claims["rate_limit_rpm"] = serde_json::json!(30);
    let token = sign(&claims, KID);
    let claims: JwtClaims = discovery().validate_token(&token).unwrap();
    let override_cfg = claims.into_auth_context().rate_limit_override.unwrap();
    assert_eq!(override_cfg.requests_per_second, Some(1));
    assert_eq!(override_cfg.burst_size, Some(30));
}

#[test]
fn not_yet_valid_token_is_rejected() {
    // Regression: `nbf` must be enforced during decode (jsonwebtoken's
    // default validation skips it).
    let mut claims = base_claims();
    claims["nbf"] = serde_json::json!(now() + 3600);
    let token = sign(&claims, KID);
    assert!(discovery().validate_token(&token).is_err());
}
