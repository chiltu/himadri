# Enterprise Authentication Requirements

## Current State

The gateway has basic API key authentication only:
- Bearer token in `Authorization` header
- Master key for admin access
- Simple scopes: `Admin`, `ReadOnly`, `ApiKey`
- No JWT validation, no OAuth2/OIDC, no centralized identity

## Goal

Support enterprise-level authentication with centralized identity providers (Zitadel, Keycloak, Auth0, etc.) while maintaining backward compatibility with existing API key auth.

---

## Architecture

```
                    ┌──────────────────────┐
                    │   AI Gateway         │
                    │   (himadri)     │
                    └──────────┬───────────┘
                               │
          ┌────────────────────┼────────────────────┐
          ▼                    ▼                    ▼
   ┌─────────────┐    ┌─────────────┐    ┌─────────────┐
   │ API Key     │    │ JWT/OIDC    │    │ OAuth2      │
   │ Auth        │    │ Auth        │    │ Auth        │
   │ (existing)  │    │ (new)       │    │ (new)       │
   └──────┬──────┘    └──────┬──────┘    └──────┬──────┘
          │                    │                    │
          ▼                    ▼                    ▼
   ┌─────────────┐    ┌─────────────┐    ┌─────────────┐
   │ Local Store │    │ Zitadel /   │    │ Any OAuth2  │
   │ (SQLite/    │    │ Keycloak /  │    │ Provider    │
   │  Postgres)  │    │ Auth0       │    │ (GitHub,    │
   └─────────────┘    └─────────────┘    │  Google)    │
                                         └─────────────┘
```

---

## Phase 1: JWT Token Validation (Weeks 1-2)

### New Crate: `himadri-auth`

```
crates/himadri-auth/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── jwt.rs          # JWT validation (decode, verify, extract claims)
    ├── oidc.rs         # OIDC discovery (fetch JWKS, auto-refresh)
    ├── middleware.rs    # Auth middleware for axum
    ├── config.rs       # Auth configuration
    └── error.rs        # Auth error types
```

### JWT Validation Flow

```
Request → Extract Bearer Token → Decode JWT → Verify Signature (JWKS) → Extract Claims → Build AuthContext
                                    │
                                    ├── Signature valid? → Check expiry → Check issuer → Check audience
                                    │
                                    └── Invalid → 401 Unauthorized
```

### OIDC Discovery

```rust
pub struct OidcDiscovery {
    issuer: String,
    jwks: RwLock<Vec<Jwk>>,
    jwks_uri: String,
    last_refresh: Instant,
}

impl OidcDiscovery {
    /// Auto-discover OIDC endpoints from issuer URL
    pub async fn discover(issuer: &str) -> Result<Self, AuthError> {
        // GET {issuer}/.well-known/openid-configuration
        // Extract jwks_uri, token_endpoint, etc.
    }

    /// Validate JWT signature against cached JWKS
    pub async fn validate_token(&self, token: &str) -> Result<Claims, AuthError> {
        // Decode header → Find matching key by kid → Verify signature
    }

    /// Refresh JWKS when keys rotate
    async fn refresh_jwks(&self) -> Result<(), AuthError> {
        // GET jwks_uri → Cache new keys
    }
}
```

### Claims → AuthContext Mapping

```rust
pub struct JwtClaims {
    pub sub: String,           // Subject (user ID)
    pub iss: String,           // Issuer (Zitadel URL)
    pub aud: String,           // Audience (client ID)
    pub exp: u64,              // Expiry
    pub iat: u64,              // Issued at
    pub scope: Option<String>, // Space-separated scopes
    pub org_id: Option<String>,
    pub team_id: Option<String>,
    pub email: Option<String>,
    pub roles: Option<Vec<String>>,
}

impl JwtClaims {
    pub fn into_auth_context(self) -> AuthContext {
        AuthContext {
            api_key: format!("jwt:{}", self.sub),
            key_id: Some(self.sub),
            scope: self.determine_scope(),
            org_id: self.org_id,
            team_id: self.team_id,
            user_id: Some(self.sub),
            rate_limit_override: None,
            token_type: Some(TokenType::Jwt),
            issuer: Some(self.iss),
            audience: Some(self.aud),
            expires_at: Some(self.exp),
            claims: Some(self),
        }
    }
}
```

### Configuration

```yaml
auth:
  # API key auth (existing)
  api_key:
    enabled: true
    master_key: ${MASTER_KEY}

  # JWT/OIDC auth (new)
  jwt:
    enabled: true
    issuer: "https://your-domain.zitadel.cloud"
    audience: "your-client-id"
    jwks_refresh_interval: 3600  # seconds
    # Or explicit JWKS URL (bypasses OIDC discovery)
    # jwks_uri: "https://your-domain.zitadel.cloud/oauth/v2/keys"
```

### Dependencies

```toml
jsonwebtoken = "9"          # JWT decode/verify
josekit = "0.8"             # JOSE/JWK/JWT utilities
reqwest = { features = ["json"] }  # OIDC discovery
```

### Tests

- [ ] Decode valid JWT and extract claims
- [ ] Reject expired JWT
- [ ] Reject JWT with wrong signature
- [ ] Reject JWT with wrong audience
- [ ] JWKS auto-refresh on key rotation
- [ ] OIDC discovery from issuer URL
- [ ] Backward compatibility with API key auth

---

## Phase 2: OAuth2 Client Credentials (Weeks 3-4)

### Client Credentials Flow

```
Service → POST /oauth/token → Zitadel → Access Token → Gateway validates JWT
```

### Token Introspection

```rust
pub struct TokenIntrospector {
    introspection_endpoint: String,
    client_id: String,
    client_secret: String,
}

impl TokenIntrospector {
    /// Introspect token with provider (for opaque tokens)
    pub async fn introspect(&self, token: &str) -> Result<TokenInfo, AuthError> {
        // POST introspection_endpoint with token
        // Returns { active, sub, client_id, scope, exp, ... }
    }
}
```

### Configuration

```yaml
auth:
  oauth2:
    enabled: true
    client_id: ${OAUTH_CLIENT_ID}
    client_secret: ${OAUTH_CLIENT_SECRET}
    token_endpoint: "https://your-domain.zitadel.cloud/oauth/v2/token"
    introspection_endpoint: "https://your-domain.zitadel.cloud/oauth/v2/introspect"
```

### Tests

- [ ] Client credentials flow returns valid token
- [ ] Token introspection returns correct info
- [ ] Expired token rejected
- [ ] Invalid client credentials rejected

---

## Phase 3: Zitadel-Specific Integration (Weeks 5-6)

### Zitadel Claims Mapping

```rust
pub struct ZitadelClaims {
    // Standard OIDC claims
    pub sub: String,
    pub iss: String,
    pub aud: String,
    pub exp: u64,

    // Zitadel-specific
    pub zitadel_org_id: Option<String>,
    pub zitadel_project_id: Option<String>,
    pub zitadel_user_id: Option<String>,
    pub zitadel_username: Option<String>,
    pub zitadel_email: Option<String>,
    pub zitadel_verified: bool,

    // Custom claims for rate limiting
    pub rate_limit_override: Option<RateLimitOverride>,
    pub budget_limit: Option<f64>,
}
```

### Zitadel User/Team Resolution

```rust
pub struct ZitadelResolver {
    client: reqwest::Client,
    admin_api_url: String,
    pat: String, // Personal Access Token for admin API
}

impl ZitadelResolver {
    /// Resolve user details from Zitadel admin API
    pub async fn get_user(&self, user_id: &str) -> Result<ZitadelUser, AuthError> {
        // GET /admin/v1/users/{userId}
    }

    /// Get user's organizations
    pub async fn get_user_orgs(&self, user_id: &str) -> Result<Vec<ZitadelOrg>, AuthError> {
        // GET /admin/v1/users/{userId}/orgs
    }

    /// Get user's memberships (teams/roles)
    pub async fn get_memberships(&self, user_id: &str) -> Result<Vec<ZitadelMembership>, AuthError> {
        // GET /admin/v1/users/{userId}/memberships
    }
}
```

### Zitadel Event Webhooks

```rust
pub struct ZitadelWebhook {
    webhook_secret: String,
}

impl ZitadelWebhook {
    /// Verify webhook signature and parse event
    pub fn verify_and_parse(&self, body: &[u8], signature: &str) -> Result<ZitadelEvent, AuthError> {
        // Verify HMAC signature
        // Parse event type
    }
}

pub enum ZitadelEvent {
    UserAdded { user_id: String, org_id: String },
    UserRemoved { user_id: String, org_id: String },
    MembershipAdded { user_id: String, org_id: String, roles: Vec<String> },
    MembershipRemoved { user_id: String, org_id: String },
}
```

### Configuration

```yaml
auth:
  zitadel:
    enabled: true
    admin_api_url: "https://your-domain.zitadel.cloud/admin/v1"
    pat: ${ZITADEL_PAT}  # Personal Access Token
    webhook_secret: ${ZITADEL_WEBHOOK_SECRET}
```

### Tests

- [ ] Resolve user details from Zitadel API
- [ ] Get user organizations
- [ ] Get user memberships
- [ ] Verify webhook signature
- [ ] Parse webhook events

---

## Phase 4: Rate Limit & Budget Integration (Weeks 7-8)

### Claim-Based Rate Limits

```rust
pub struct ClaimBasedRateLimit {
    pub user_rpm: Option<u64>,
    pub org_rpm: Option<u64>,
    pub team_rpm: Option<u64>,
    pub tier: RateLimitTier,
}

pub enum RateLimitTier {
    Free { rpm: u64 },
    Pro { rpm: u64 },
    Enterprise { rpm: u64 },
    Custom { rpm: u64 },
}
```

### Budget from Claims

```rust
pub struct ClaimBasedBudget {
    pub budget_limit_usd: Option<f64>,
    pub billing_period: BillingPeriod,
    pub cost_per_m_tokens: Option<TokenPricing>,
}

pub enum BillingPeriod {
    Hourly,
    Daily,
    Monthly,
    Custom { reset_interval: Duration },
}
```

### Plugin Integration

```yaml
plugins:
  - name: budget
    stage: before_request
    config:
      source: jwt_claims
      default_spend_limit_usd: 10.0

  - name: rate-limit
    stage: before_request
    config:
      source: jwt_claims
      default_rpm: 100
```

### Tests

- [ ] Rate limit from JWT claims
- [ ] Budget from JWT claims
- [ ] Fallback to defaults when no claims
- [ ] Per-org rate limits
- [ ] Per-team rate limits

---

## Phase 5: Multi-Tenant Isolation (Weeks 9-10)

### Tenant Context

```rust
pub struct TenantContext {
    pub org_id: String,
    pub project_id: Option<String>,
    pub allowed_models: Vec<String>,
    pub allowed_providers: Vec<String>,
    pub custom_rate_limits: HashMap<String, u64>,
}
```

### RBAC

```rust
pub enum Permission {
    ModelRead { model: String },
    ModelWrite { model: String },
    ProviderUse { provider: String },
    KeyManage,
    ConfigManage,
    AuditRead,
    BudgetView,
    BudgetManage,
}

pub struct RbacPolicy {
    pub role_permissions: HashMap<String, Vec<Permission>>,
}
```

### Audit Logging

```rust
pub struct AuditLog {
    timestamp: DateTime<Utc>,
    user_id: String,
    org_id: String,
    action: AuditAction,
    resource: String,
    details: serde_json::Value,
}

pub enum AuditAction {
    RequestMade { model: String, tokens: u32 },
    KeyCreated { key_id: String },
    KeyRevoked { key_id: String },
    ConfigChanged { field: String },
    AuthFailed { reason: String },
}
```

### Tests

- [ ] Tenant isolation (model/provider restrictions)
- [ ] RBAC permission checks
- [ ] Audit log recording
- [ ] Cross-tenant access denied

---

## Phase 6: Migration & Documentation (Weeks 11-12)

### Backward Compatibility

```yaml
auth:
  # Existing API key auth still works
  api_key:
    enabled: true
    master_key: ${MASTER_KEY}

  # New JWT auth (additive)
  jwt:
    enabled: true
    issuer: "https://your-zitadel.cloud"
    audience: "your-client-id"

  # Auth order: JWT first, then API key fallback
  strategy:
    order: [jwt, api_key]
    fallback: true
```

### Migration Steps

1. Deploy with API key auth only (current state)
2. Add JWT validation (Phase 1) — non-breaking, additive
3. Configure Zitadel OIDC — test with a few users
4. Gradually migrate users — both auth methods work simultaneously
5. Enforce JWT-only — disable API key auth when ready

---

## Dependencies

```toml
jsonwebtoken = "9"          # JWT decode/verify
josekit = "0.8"             # JOSE/JWK/JWT utilities
reqwest = { features = ["json"] }  # OIDC discovery
chrono = { features = ["serde"] }  # Time handling
```

## Estimated Effort

| Phase | Feature | Effort |
|-------|---------|--------|
| 1 | JWT validation + OIDC discovery | 2 weeks |
| 2 | OAuth2 client_credentials | 2 weeks |
| 3 | Zitadel-specific integration | 2 weeks |
| 4 | Claim-based rate limits/budgets | 2 weeks |
| 5 | Multi-tenant + RBAC | 2 weeks |
| 6 | Migration + docs | 2 weeks |

**Total: 12 weeks (1 engineer)**

## Priority

Phase 1 (JWT validation) is the most critical — it unblocks everything else and is relatively straightforward. Start there.
