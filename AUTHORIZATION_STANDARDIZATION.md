# Authorization Standardization — Implementation Guide

This document summarizes the standardized role and authorization framework for the AI Gateway, including vendor mappings and deployment instructions.

---

## What Was Implemented

### 1. Standard Role Taxonomy (`crates/himadri-core/src/roles.rs`)

Defined portable, vendor-neutral roles:

| Role | Purpose | Capabilities |
|------|---------|---|
| **user** | Basic access | Submit to assigned models/providers |
| **power-user** | Extended access | Use any model/provider (no RBAC restriction) |
| **analyst** | Audit access | View usage logs and audit trails (read-only) |
| **admin** | Full control | All capabilities (RBAC bypass) |

Each role maps to a `RoleCapabilities` struct with fine-grained permission flags.

### 2. Vendor Role Mappings

Automatic mapping from vendor-specific role names to standard roles:

- **Zitadel:** `admin` → admin, `editor` → power-user, `viewer` → analyst, `member` → user
- **Auth0:** `Admin` → admin, `Editor` → power-user, `Viewer` → analyst, `User` → user
- **Keycloak:** `realm-admin` → admin, `realm-editor` → power-user, etc.
- **Entra ID:** `GlobalAdmin` → admin, `Editor` → power-user, `Reader` → analyst, `User` → user
- **Okta:** `System Administrator` → admin, `Application Administrator` → power-user, `Help Desk Administrator` → analyst, `Okta User` → user
- **Ping Identity:** `Administrator` → admin, `Editor` → power-user, `Auditor` → analyst, `User` → user

All mappings are defined in `VendorRoleMapping` with pre-built instances for each provider.

### 3. Standard JWT Claims Shape

```json
{
  "sub": "user-id",
  "iss": "https://your-idp.example.com",
  "aud": "gateway-client-id",
  "exp": 1234567890,
  "iat": 1234567800,
  "roles": ["user", "analyst"],
  "org_id": "org-123",
  "team_id": "team-456",
  "email": "user@example.com",
  "custom:billing_tier": "pro",
  "custom:rate_limit_rpm": 600,
  "custom:budget_limit_usd": 100.0
}
```

The gateway extracts and uses all standard claims; custom claims are preserved for plugins.

### 4. Vendor Setup Templates

Complete setup scripts and infrastructure-as-code for each provider:

```
deploy/
├── README.md                      # Master setup guide
├── zitadel/
│   ├── setup.sh                   # Bash setup script
│   └── terraform/main.tf          # Terraform IaC
├── auth0/
│   ├── setup.sh                   # Bash setup script
│   └── terraform/main.tf          # Terraform IaC
├── keycloak/
│   ├── setup.sh                   # Bash setup script
│   └── realm-export.json          # Pre-configured realm
├── entra/
│   ├── setup.ps1                  # PowerShell setup script
│   ├── terraform/main.tf          # Terraform IaC
│   └── bicep/main.bicep           # Azure Bicep template
├── okta/
│   ├── setup.sh                   # Bash setup script
│   └── terraform/main.tf          # Terraform IaC
└── ping/
    └── setup.sh                   # Bash setup script
```

Each template:
- Creates OIDC application with correct redirect URIs
- Defines standard roles (admin, power-user, analyst, user)
- Creates groups/roles matching standard role set
- Outputs environment variables ready for gateway deployment

### 5. Documentation

| Document | Purpose |
|----------|---------|
| [`docs/roles.md`](./docs/roles.md) | Standard role definitions, hierarchy, combinations, vendor mappings |
| [`docs/config-standard-roles.json`](./docs/config-standard-roles.json) | Starter RBAC config with standard roles |
| [`deploy/README.md`](./deploy/README.md) | Master setup guide for all vendors |

---

## Quick Start

### Step 1: Choose an Identity Provider

Pick one from: **Zitadel**, **Auth0**, **Keycloak**, **Azure AD/Entra**, **Okta**, or **Ping Identity**.

### Step 2: Run Setup for Your Provider

```bash
cd deploy/<provider>

# Example: Zitadel
export ZITADEL_DOMAIN="your-domain.zitadel.cloud"
export ZITADEL_API_TOKEN="$(cat ~/.zitadel-pat.txt)"
bash setup.sh

# OR: Auth0
export AUTH0_DOMAIN="your-tenant.auth0.com"
export AUTH0_CLIENT_ID="<management-api-client-id>"
export AUTH0_CLIENT_SECRET="<management-api-client-secret>"
bash setup.sh

# OR: Keycloak (import realm)
curl -X POST http://localhost:8080/admin/realms \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d @realm-export.json
```

### Step 3: Save Environment Variables

Each setup script outputs required env vars. Example:

```bash
export JWT_ISSUER="https://your-domain.zitadel.cloud"
export JWT_AUDIENCE="<client-id-from-setup>"
export JWT_JWKS_URI="https://your-domain.zitadel.cloud/oauth/v2/keys"
export JWT_REQUIRED_ROLES="user,analyst,power-user,admin"
```

### Step 4: Configure Gateway RBAC

Copy [`docs/config-standard-roles.json`](./docs/config-standard-roles.json) as your gateway config:

```bash
export GATEWAY_CONFIG="/path/to/config-standard-roles.json"
```

This config has standard roles pre-configured with sensible model/provider restrictions.

### Step 5: Deploy Gateway

```bash
export JWT_ISSUER="<from-step-3>"
export JWT_AUDIENCE="<from-step-3>"
export GATEWAY_CONFIG="<from-step-4>"
cargo run -p himadri --release
```

### Step 6: Test

Create a user in your identity provider, assign them a role (e.g., `user`), and get a JWT token:

```bash
curl -H "Authorization: Bearer <jwt>" http://localhost:8080/v1/models
# Should succeed if JWT is valid and user has a recognized role
```

---

## Architecture Diagram

```
┌─────────────────────────────────────────────────────┐
│               Client / Application                  │
└────────────────┬────────────────────────────────────┘
                 │ Bearer Token (JWT)
                 ▼
        ┌────────────────────┐
        │  API Gateway       │
        │   /v1/models       │
        │   /v1/chat/...     │
        └────────┬───────────┘
                 │
         ┌───────┴──────────┐
         ▼                  ▼
    ┌─────────────┐    ┌──────────────┐
    │ JWT Validate│    │ API Key Auth │
    │   (OIDC)    │    │  (DB Lookup) │
    └──────┬──────┘    └──────┬───────┘
           │                  │
           └──────────┬───────┘
                      ▼
           ┌─────────────────────┐
           │   AuthContext       │
           ├─────────────────────┤
           │ api_key: "jwt:sub"  │
           │ scope: Admin/RO/Key │
           │ roles: [...]        │
           │ org_id, team_id     │
           │ rate_limit_override │
           └────────┬────────────┘
                    │
    ┌───────────────┼───────────────┐
    ▼               ▼               ▼
┌──────────┐  ┌──────────┐  ┌────────────┐
│Required  │  │  RBAC    │  │   Rate     │
│  Roles   │  │  Check   │  │   Limit    │
│ Gate     │  │  Models/ │  │ &  Budget  │
│(403 if   │  │Providers │  │  Override  │
│ missing) │  │(403 if   │  │            │
│          │  │denied)   │  │            │
└──────────┘  └──────────┘  └────────────┘
    │              │              │
    └──────────────┼──────────────┘
                   ▼
         ┌──────────────────────┐
         │   Request Allowed    │
         │  Route to Provider   │
         └──────────────────────┘
```

---

## Configuration Examples

### Example 1: Minimal Setup (Default Roles Only)

```json
{
  "rbac": {
    "enabled": true,
    "default_role": "user",
    "roles": {
      "user": { "models": ["gpt-4o-mini"], "providers": null },
      "power-user": { "models": null, "providers": null },
      "analyst": { "models": null, "providers": null },
      "admin": { "models": null, "providers": null }
    }
  }
}
```

### Example 2: Tiered Access (Free/Pro/Enterprise)

Combine `JWT_REQUIRED_ROLES` with RBAC:

```bash
export JWT_REQUIRED_ROLES="user,analyst,power-user,admin"
```

```json
{
  "rbac": {
    "enabled": true,
    "default_role": "user",
    "roles": {
      "user": {
        "models": ["gpt-4o-mini"],
        "providers": ["openai"]
      },
      "power-user": {
        "models": ["gpt-4o", "claude-3-5-sonnet"],
        "providers": ["openai", "anthropic"]
      },
      "admin": {
        "models": null,
        "providers": null
      }
    }
  }
}
```

### Example 3: Org-Specific Custom Roles

```json
{
  "rbac": {
    "enabled": true,
    "default_role": "user",
    "roles": {
      "user": { "models": ["gpt-4o-mini"] },
      "ml-engineer": {
        "models": ["gpt-4o", "claude-*", "*-large"],
        "providers": ["openai", "bedrock", "anthropic"]
      },
      "data-scientist": {
        "models": ["gpt-4o", "*-large"],
        "providers": ["openai", "anthropic"]
      },
      "admin": { "models": null, "providers": null }
    }
  }
}
```

---

## Code Integration Points

### In `himadri-core/src/roles.rs`

```rust
use himadri_core::{ StandardRole, RoleCapabilities, VendorRoleMapping };

// Parse vendor role string to standard role
let standard_role = StandardRole::from_string("power-user");

// Get capabilities for a role
let caps = RoleCapabilities::power_user();

// Map Auth0 role to standard role
let mapping = VendorRoleMapping::auth0();
let standard = mapping.map("Editor");  // → "power-user"

// Merge capabilities from multiple roles
let mut merged = RoleCapabilities::user();
merged.merge(&RoleCapabilities::analyst());
// Now merged has both user's submit capability + analyst's audit access
```

### In JWT Claims Processing

The `himadri-auth` crate already extracts roles and uses vendor mappings:

```rust
// From himadri-auth/src/jwt.rs
pub fn roles(&self) -> Vec<String> {
    let mut roles = self.roles.clone().unwrap_or_default();
    
    // Extract Zitadel project roles automatically
    for (key, value) in &self.custom {
        if key.starts_with("urn:zitadel:iam:org:project:") && key.ends_with(":roles") {
            if let Some(map) = value.as_object() {
                roles.extend(map.keys().cloned());
            }
        }
    }
    
    roles.sort();
    roles.dedup();
    roles
}

pub fn into_auth_context(self) -> AuthContext {
    let roles = self.roles();
    
    // Determine scope from roles (admin/power-user/analyst/default)
    let is_admin = roles.iter().any(|r| r == "admin" || r == "gateway-admin");
    let is_readonly = roles.iter().any(|r| r == "read-only" || r == "readonly");
    
    // Build AuthContext with roles for RBAC
    AuthContext {
        api_key: format!("jwt:{}", self.sub),
        key_id: Some(self.sub),
        scope: if is_admin { Admin } else if is_readonly { ReadOnly } else { ApiKey },
        roles,  // Roles for RBAC checks
        org_id: self.org_id,
        team_id: self.team_id,
        // ...
    }
}
```

### In RBAC Enforcement

The `himadri/src/gateway.rs` uses roles for access control:

```rust
// Check if principal's roles permit this model
config.rbac.check_model(roles, is_admin, model)?;

// Filter targets by provider permissions
let permitted_targets = filter_targets_by_rbac(auth, targets).await?;
```

---

## Migration Path

If you have existing API key users:

### Phase 1: Parallel Authentication (Week 1)
- Deploy gateway with JWT auth enabled
- API key auth still works (configured in admin.master_key)
- Both auth methods accepted on same endpoints

### Phase 2: Gradual Migration (Weeks 2-4)
- Create users in OIDC provider
- Issue JWTs to teams; test with both old API keys and new JWTs
- Revoke old API keys gradually as teams migrate
- Monitor dual auth usage to identify stragglers

### Phase 3: JWT-Only (Week 5+)
- Disable API key auth (remove master_key, revoke remaining API keys)
- All principals now use JWTs from OIDC provider
- Leverage OIDC provider's user management (2FA, provisioning, etc.)

---

## Checklist for Operators

- [ ] **Choose OIDC provider** (Zitadel, Auth0, Keycloak, Entra, Okta, Ping)
- [ ] **Run setup script** (creates app, roles, groups)
- [ ] **Save environment variables** (`JWT_ISSUER`, `JWT_AUDIENCE`, etc.)
- [ ] **Create users** in OIDC provider
- [ ] **Assign roles** to users (user, analyst, power-user, admin)
- [ ] **Copy config file** (`config-standard-roles.json`)
- [ ] **Customize RBAC** (adjust models/providers per role as needed)
- [ ] **Deploy gateway** with env vars + config
- [ ] **Test with JWT** (decode token, verify roles, test RBAC)
- [ ] **Monitor auth logs** for 401/403 errors
- [ ] **Migrate existing API keys** (parallel phase → gradual → JWT-only)

---

## Further Reading

- [Standard Roles Reference](./docs/roles.md)
- [Vendor Setup Guide](./deploy/README.md)
- [RBAC Configuration](./docs/configuration.md#rbac-tiered-access)
- [JWT Claims Shape](./docs/roles.md#jwt-claims-shape-standard)
- [Rate Limits & Budgets](./docs/configuration.md#rate-limiting)

---

## Support

For issues or questions:

1. Check [deploy/README.md troubleshooting](./deploy/README.md#troubleshooting)
2. Review vendor-specific setup docs (e.g., [Zitadel](./docs/zitadel.md))
3. Verify JWT token structure: `jwt decode <token>`
4. Check RBAC config matches your expected roles
5. Review gateway logs for auth/RBAC denials

---

## Summary

✓ **Standard role taxonomy** — portable across all OIDC providers  
✓ **Vendor mappings** — automatic normalization of vendor-specific role names  
✓ **Setup automation** — scripts and IaC for all 6 major providers  
✓ **Documentation** — configuration guides, examples, troubleshooting  
✓ **Code integration** — roles module in himadri-core, JWT processing in himadri-auth  

**Next: Pick your OIDC provider and run the setup script from `deploy/`.**
