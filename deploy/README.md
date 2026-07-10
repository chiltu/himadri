# AI Gateway Identity Provider Setup

This directory contains setup scripts and infrastructure-as-code for integrating the AI Gateway with various OIDC/OAuth 2.0 identity providers.

---

## Overview

The AI Gateway uses standardized roles mapped to fine-grained RBAC (role-based access control). Each identity provider below includes:

1. **Setup script** — Automates resource creation (apps, roles, groups)
2. **Infrastructure-as-code** — Terraform or Bicep for repeatable deployments
3. **Configuration guide** — How to map vendor roles to gateway roles

**Supported Providers:**

- [Zitadel](#zitadel) — Open-source identity platform
- [Auth0](#auth0) — Auth0 SaaS platform
- [Keycloak](#keycloak) — Open-source identity and access management
- [Azure AD / Entra ID](#azure-ad--entra-id) — Microsoft cloud identity
- [Okta](#okta) — Enterprise identity management
- [Ping Identity](#ping-identity) — Enterprise access management

---

## Standard Roles

All providers are configured with the same standard role set:

| Role | Purpose |
|------|---------|
| **user** | Basic authenticated access to assigned models/providers |
| **power-user** | Extended access to all models/providers (no RBAC restrictions) |
| **analyst** | Read-only access to usage logs and audit trails |
| **admin** | Full gateway control (all capabilities) |

Custom/org-specific roles can be added in the gateway config (`rbac.roles`).

See [`docs/roles.md`](../docs/roles.md) for complete role definitions and JWT claim shapes.

---

## Gateway Configuration

After setting up your identity provider, add these environment variables:

```bash
# Required
export JWT_ISSUER="<your-idp-issuer-url>"
export JWT_AUDIENCE="<your-client-id>"

# Optional
export JWT_JWKS_URI="<explicit-jwks-endpoint>"  # If not auto-discovered
export JWT_JWKS_REFRESH_SECS="3600"               # JWKS refresh interval
export JWT_REQUIRED_ROLES="user,analyst,power-user,admin"  # Require at least one
```

Each provider section below includes the exact values to use.

---

## Zitadel

**Type:** Open-source identity platform  
**Hosting:** Self-hosted or Zitadel Cloud  
**OIDC Support:** ✓ Full compliance  
**Native Roles:** ✓ Project roles (vendor-mapped to standard roles)

### Quick Start

```bash
cd deploy/zitadel

# Using bash setup script
export ZITADEL_DOMAIN="your-domain.zitadel.cloud"
export ZITADEL_API_TOKEN="<pat>"
bash setup.sh

# OR using Terraform
export ZITADEL_DOMAIN="your-domain.zitadel.cloud"
export ZITADEL_TOKEN="<pat>"
cd terraform && terraform apply
```

### Environment Variables

```bash
export JWT_ISSUER="https://your-domain.zitadel.cloud"
export JWT_AUDIENCE="<client-id-from-setup>"
export JWT_JWKS_URI="https://your-domain.zitadel.cloud/oauth/v2/keys"
```

### Role Mapping

Zitadel project roles are automatically recognized:

```
Zitadel Role  →  Standard Role
────────────────────────────
admin         →  admin
editor        →  power-user
viewer        →  analyst
member        →  user
```

### Docs

- [Zitadel Configuration](../docs/zitadel.md)
- [Setup Script](./zitadel/setup.sh)
- [Terraform](./zitadel/terraform/main.tf)

---

## Auth0

**Type:** Auth0 SaaS  
**Hosting:** Cloud-only  
**OIDC Support:** ✓ Full compliance  
**Native Roles:** ✓ With custom rules to emit roles claim

### Quick Start

```bash
cd deploy/auth0

# Using bash setup script
export AUTH0_DOMAIN="your-tenant.auth0.com"
export AUTH0_CLIENT_ID="<management-api-client-id>"
export AUTH0_CLIENT_SECRET="<management-api-client-secret>"
bash setup.sh

# OR using Terraform
export AUTH0_DOMAIN="your-tenant.auth0.com"
export AUTH0_CLIENT_ID="<management-api-client-id>"
export AUTH0_CLIENT_SECRET="<management-api-client-secret>"
cd terraform && terraform apply
```

### Environment Variables

```bash
export JWT_ISSUER="https://your-tenant.auth0.com/"
export JWT_AUDIENCE="<client-id-from-setup>"
```

### Role Mapping

Configure in Auth0 console or use custom rules:

```
Auth0 Role  →  Standard Role
────────────────────────────
Admin       →  admin
Editor      →  power-user
Viewer      →  analyst
User        →  user
```

Use Auth0 Rules to emit roles in the ID token:

```javascript
function (user, context, callback) {
  var roles = user.roles || [];
  context.idToken = context.idToken || {};
  context.idToken.roles = roles;
  callback(null, user, context);
}
```

### Docs

- [Setup Script](./auth0/setup.sh)
- [Terraform](./auth0/terraform/main.tf)

---

## Keycloak

**Type:** Open-source identity and access management  
**Hosting:** Self-hosted  
**OIDC Support:** ✓ Full compliance  
**Native Roles:** ✓ Realm roles (mapped to standard roles)

### Quick Start

```bash
cd deploy/keycloak

# Option 1: Import pre-configured realm
curl -X POST http://localhost:8080/admin/realms \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d @realm-export.json

# Option 2: Setup script
export KEYCLOAK_URL="http://localhost:8080"
export KEYCLOAK_ADMIN="admin"
export KEYCLOAK_PASSWORD="<password>"
bash setup.sh
```

### Environment Variables

```bash
export JWT_ISSUER="http://localhost:8080/realms/ai-gateway"
export JWT_AUDIENCE="ai-gateway"
```

### Role Mapping

Keycloak realm roles mapped to standard roles:

```
Keycloak Role  →  Standard Role
──────────────────────────────
realm-admin    →  admin
realm-editor   →  power-user
realm-viewer   →  analyst
realm-user     →  user
```

### Docs

- [Setup Script](./keycloak/setup.sh)
- [Realm Export](./keycloak/realm-export.json)

---

## Azure AD / Entra ID

**Type:** Microsoft cloud identity  
**Hosting:** Cloud-only  
**OIDC Support:** ✓ Full compliance (via v2.0 endpoint)  
**Native Roles:** ✓ App roles + security groups

### Quick Start

```bash
cd deploy/entra

# Using PowerShell setup script (Windows/PowerShell)
.\setup.ps1 -TenantId "<tenant-id>" -GatewayDomain "localhost:8080"

# OR using Terraform
export AZURE_TENANT_ID="<tenant-id>"
cd terraform && terraform apply

# OR using Bicep
az deployment tenant create \
  --template-file bicep/main.bicep \
  --location eastus \
  --parameters tenantId="<tenant-id>"
```

### Environment Variables

```bash
export JWT_ISSUER="https://login.microsoftonline.com/<tenant-id>/v2.0"
export JWT_AUDIENCE="<client-id-from-setup>"
```

### Role Mapping

Azure AD app roles mapped to standard roles:

```
Entra App Role  →  Standard Role
─────────────────────────────────
admin            →  admin
power-user       →  power-user
analyst          →  analyst
user             →  user
```

Assign users to security groups (Gateway-admin, Gateway-power-user, etc.) and configure group membership to populate the `roles` claim.

### Docs

- [PowerShell Setup](./entra/setup.ps1)
- [Terraform](./entra/terraform/main.tf)
- [Bicep](./entra/bicep/main.bicep)

---

## Okta

**Type:** Enterprise identity management  
**Hosting:** Cloud-only  
**OIDC Support:** ✓ Full compliance  
**Native Roles:** ✓ Groups as roles (vendor-mapped)

### Quick Start

```bash
cd deploy/okta

# Using bash setup script
export OKTA_DOMAIN="https://dev-xxxxx.okta.com"
export OKTA_API_TOKEN="<token>"
bash setup.sh

# OR using Terraform
export OKTA_ORG_NAME="your-org"
export OKTA_API_TOKEN="<token>"
cd terraform && terraform apply
```

### Environment Variables

```bash
export JWT_ISSUER="https://your-org.okta.com/oauth2/default"
export JWT_AUDIENCE="<client-id-from-setup>"
```

### Role Mapping

Use Okta groups to represent roles (setup script creates Gateway-admin, Gateway-power-user, etc.):

```
Okta Group           →  Standard Role
────────────────────────────────────
Gateway-admin        →  admin
Gateway-power-user   →  power-user
Gateway-analyst      →  analyst
Gateway-user         →  user
```

Configure Okta to emit group names as the `roles` claim in the ID token.

### Docs

- [Setup Script](./okta/setup.sh)
- [Terraform](./okta/terraform/main.tf)

---

## Ping Identity

**Type:** Enterprise access management  
**Hosting:** PingOne Cloud or PingFederate self-hosted  
**OIDC Support:** ✓ Full compliance  
**Native Roles:** ✓ Custom roles + groups

### Quick Start

```bash
cd deploy/ping

# Using bash setup script (PingOne)
export PINGONE_REGION="NorthAmerica"
export PINGONE_ENVIRONMENT_ID="<env-id>"
export PINGONE_CLIENT_ID="<service-account-id>"
export PINGONE_CLIENT_SECRET="<service-account-secret>"
bash setup.sh
```

### Environment Variables

```bash
# For PingOne
export JWT_ISSUER="https://auth.pingone.com/<environment-id>"
export JWT_AUDIENCE="<client-id-from-setup>"

# For PingFederate
export JWT_ISSUER="https://your-pingfed.example.com:9031"
export JWT_AUDIENCE="<client-id-from-setup>"
```

### Role Mapping

Create custom roles and groups in Ping Identity:

```
Ping Group          →  Standard Role
─────────────────────────────────
Gateway-admin       →  admin
Gateway-power-user  →  power-user
Gateway-analyst     →  analyst
Gateway-user        →  user
```

Configure Ping Identity to emit group membership as the `roles` claim.

### Docs

- [Setup Script](./ping/setup.sh)

---

## Configuration Checklist

After provider setup, verify:

- [ ] **OIDC Discovery:** JWT issuer responds to `/.well-known/openid-configuration`
- [ ] **JWKS Endpoint:** JWKS keys are available and refreshing
- [ ] **Test Token:** Generate and decode a JWT to verify structure:
  ```bash
  jwt decode <your-jwt-token>
  ```
- [ ] **Gateway Env Vars:** Set `JWT_ISSUER`, `JWT_AUDIENCE` (see provider section above)
- [ ] **Role Claims:** Verify `roles` claim contains standard role names (`user`, `power-user`, `analyst`, `admin`)
- [ ] **Gateway Start:** Deploy gateway with above env vars:
  ```bash
  export JWT_ISSUER="..."
  export JWT_AUDIENCE="..."
  cargo run -p himadri --release
  ```
- [ ] **Test Auth:** Make a request with a valid JWT:
  ```bash
  curl -H "Authorization: Bearer <jwt>" http://localhost:8080/v1/models
  ```

---

## RBAC Configuration

Once roles are flowing in JWT claims, configure RBAC in your gateway config file:

```json
{
  "rbac": {
    "enabled": true,
    "default_role": "user",
    "roles": {
      "user": {
        "models": ["gpt-4o-mini", "claude-3-5-haiku"],
        "providers": null
      },
      "power-user": {
        "models": null,
        "providers": null
      },
      "analyst": {
        "models": null,
        "providers": null
      },
      "admin": {
        "models": null,
        "providers": null
      }
    }
  }
}
```

See [`docs/configuration.md#rbac-tiered-access`](../docs/configuration.md#rbac-tiered-access) for full RBAC documentation.

---

## Migration from API Keys to JWT

If you have existing API key users, you can run both simultaneously:

1. **Phase 1:** Deploy gateway with JWT enabled, API key auth still works
   ```bash
   export JWT_ISSUER="..."
   export JWT_AUDIENCE="..."
   # API_KEY auth (existing) still works
   ```

2. **Phase 2:** Create users in your OIDC provider, issue JWTs, test
3. **Phase 3:** Migrate teams gradually; both auth methods work in parallel
4. **Phase 4:** Disable API key auth when ready (remove API key master key, revoke API keys)

See [`docs/configuration.md#authentication`](../docs/configuration.md#authentication) for details.

---

## Troubleshooting

### JWT Validation Fails

```
error: invalid token: key not found
```

**Cause:** JWKS endpoint is unreachable or kid in token header doesn't match any cached key.

**Fix:**
1. Verify `JWT_ISSUER` is correct
2. Manually fetch JWKS: `curl $JWT_ISSUER/.well-known/openid-configuration`
3. Check key rotation; if issuer rotates keys frequently, increase `JWT_JWKS_REFRESH_SECS`

### Roles Not Recognized

```
"error": "no role grants access to this gateway"
```

**Cause:** RBAC is enabled, but principal's roles don't match any entry in config.

**Fix:**
1. Decode JWT: `jwt decode <token>` — verify `roles` claim has expected values
2. Check role mapping: are vendor roles mapped to standard roles? (see provider section)
3. Add a `default_role` in RBAC config if unrecognized roles should fall back

### No `roles` Claim in JWT

**Cause:** Identity provider isn't emitting `roles` claim.

**Fix:**
1. Check provider docs for how to enable/map roles to JWT
2. For custom providers, ensure custom claims are forwarded to ID token
3. If vendor doesn't support roles, use the `scope` claim as fallback (RBAC will treat it as OAuth scopes)

---

## Further Reading

- [Standard Role Definitions](../docs/roles.md)
- [RBAC Configuration](../docs/configuration.md#rbac-tiered-access)
- [JWT Claims Reference](../docs/roles.md#jwt-claims-shape-standard)
- [Vendor Mapping Guide](../docs/roles.md#vendor-role-mappings)
