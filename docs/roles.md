# Standard Role Taxonomy

This document defines the **portable, vendor-neutral role set** used across the gateway. These roles map to RBAC policies and work with any OIDC provider (Zitadel, Auth0, Keycloak, Okta, Entra, Ping Identity).

---

## Role Hierarchy

```
                            admin (full control)
                              |
              ┌───────────────┴───────────────┐
              |                               |
         power-user               analyst + others
        (broad access)        (observation & audit)
              |                       |
              └───────────────┬───────┘
                              |
                            user (basic)
```

---

## Standard Roles

### `user`

**Description:** Basic authenticated user. Can submit requests to assigned models/providers.

**Capabilities:**
- ✓ Submit requests to allowed models
- ✓ Read list of available models
- ✗ No model/provider restrictions (use RBAC config to limit)
- ✗ Cannot view usage or logs
- ✗ Cannot manage anything

**RBAC Config Example:**
```json
{
  "user": {
    "models": ["gpt-4o-mini", "claude-3-5-haiku"],
    "providers": null
  }
}
```

**JWT Claim:**
```json
{
  "roles": ["user"]
}
```

---

### `power-user`

**Description:** Extended execution access. Can use all models/providers without RBAC restriction.

**Capabilities:**
- ✓ Submit requests to **any** model
- ✓ Submit requests via **any** provider
- ✓ Read list of available models
- ✗ Cannot view usage or logs
- ✗ Cannot manage anything

**RBAC Config Example:**
```json
{
  "power-user": {
    "models": null,
    "providers": null
  }
}
```

**JWT Claim:**
```json
{
  "roles": ["power-user"]
}
```

---

### `analyst`

**Description:** Observation and audit access. Can view usage logs and audit trails (no request execution).

**Capabilities:**
- ✓ Read list of available models
- ✓ View usage/billing information
- ✓ Read audit logs
- ✗ Cannot submit requests
- ✗ Cannot manage anything

**RBAC Config Example:**
```json
{
  "analyst": {
    "models": null,
    "providers": null
  }
}
```

**JWT Claim:**
```json
{
  "roles": ["analyst"]
}
```

---

### `admin`

**Description:** Full gateway control. All capabilities.

**Capabilities:**
- ✓ Submit requests to **any** model
- ✓ Submit requests via **any** provider
- ✓ Read/manage API keys
- ✓ Modify gateway configuration
- ✓ Manage team members
- ✓ Modify budget settings
- ✓ View usage and audit logs
- ✓ RBAC bypass (skips all model/provider restrictions)

**RBAC Config Example:**
```json
{
  "admin": {
    "models": null,
    "providers": null
  }
}
```

**JWT Claim:**
```json
{
  "roles": ["admin"]
}
```

Or OAuth `scope` claim:
```json
{
  "scope": "openid profile email admin"
}
```

---

## Custom/Org-Specific Roles

Organizations can define additional roles beyond the standard set. Examples:

### `ml-engineer`
Access to ML-specific models and providers.

```json
{
  "ml-engineer": {
    "models": ["gpt-4o", "claude-3-5-sonnet", "*-large"],
    "providers": ["openai", "bedrock", "anthropic"]
  }
}
```

### `team-lead`
Manages a team's access (inherited from `analyst` + team management).

```json
{
  "team-lead": {
    "models": null,
    "providers": null
  }
}
```

---

## JWT Claims Shape (Standard)

Every OIDC provider should emit (or be configured to emit) this claim shape:

```json
{
  "sub": "user-123",
  "iss": "https://your-idp.example.com",
  "aud": "gateway-client-id",
  "exp": 1234567890,
  "iat": 1234567800,
  "nbf": 1234567800,

  "roles": [
    "user",
    "analyst"
  ],

  "org_id": "org-acme",
  "org_name": "Acme Corp",

  "team_id": "team-ml-platform",
  "team_name": "ML Platform Team",

  "scope": "openid profile email",

  "email": "alice@acme.com",
  "email_verified": true,

  "custom:billing_tier": "pro",
  "custom:rate_limit_rpm": 600,
  "custom:budget_limit_usd": 100.0
}
```

**Standard Claims (Required for Auth):**
- `sub` — Subject (user ID)
- `iss` — Issuer (OIDC provider URL)
- `aud` — Audience (client ID)
- `exp` — Expiration (Unix timestamp)
- `iat` — Issued at (Unix timestamp)

**Role Claims (Required for RBAC):**
- `roles` — Array of role strings (mapped via vendor mapping)

**Organizational Context:**
- `org_id` — Organization identifier
- `team_id` — Team identifier within org

**Custom Overrides (Optional):**
- `custom:billing_tier` — `"free"`, `"pro"`, `"enterprise"`, or org-defined
- `custom:rate_limit_rpm` — Per-principal rate limit override
- `custom:budget_limit_usd` — Per-principal monthly budget cap

---

## Role Combinations & Union Semantics

When a principal holds multiple roles, the gateway applies **union semantics** (most-permissive wins):

```json
{
  "roles": ["analyst", "power-user"]
}
```

**Effective RBAC policy:**
```
analyst:       { models: null, providers: null }  ← Can read anything, no request submission
power-user:    { models: null, providers: null }  ← Can submit to anything
───────────────────────────────────────────────────
Union:         { models: null, providers: null }  ← power-user's submission capability wins
```

The principal **can submit requests** (from `power-user`) **and view usage** (from `analyst`).

---

## Vendor Role Mappings

The gateway automatically maps vendor-specific role names to standard roles during JWT processing. If your OIDC provider uses different role names, configure the mapping.

### Zitadel

Zitadel project roles are automatically recognized. By default:

```
Zitadel              →  Standard
────────────────────────────────
admin                →  admin
editor               →  power-user
viewer               →  analyst
member               →  user
```

### Auth0

Map custom role names in your RBAC config or configure Auth0 to emit roles under a custom claim.

```
Auth0 Role           →  Standard
────────────────────────────────
Admin                →  admin
Editor               →  power-user
Viewer               →  analyst
User                 →  user
```

### Keycloak

Keycloak realm roles can be mapped to standard roles via RBAC config:

```
Keycloak Role        →  Standard
────────────────────────────────
realm-admin          →  admin
realm-editor         →  power-user
realm-viewer         →  analyst
realm-user           →  user
```

### Azure AD / Entra

Map Entra directory roles:

```
Entra Role           →  Standard
────────────────────────────────
GlobalAdmin          →  admin
Admin                →  admin
Editor               →  power-user
Reader               →  analyst
User                 →  user
```

### Okta

Map Okta admin roles:

```
Okta Role                        →  Standard
────────────────────────────────────────────
System Administrator             →  admin
Organization Administrator       →  admin
Application Administrator        →  power-user
Group Administrator              →  power-user
User Administrator               →  analyst
Help Desk Administrator          →  analyst
Okta User                         →  user
```

### Ping Identity

Map Ping Identity roles:

```
Ping Role            →  Standard
────────────────────────────────
Administrator        →  admin
Editor               →  power-user
Auditor              →  analyst
User                 →  user
```

---

## Configuration Examples

### Example 1: Basic Setup (Default Roles)

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

### Example 2: Tiered Access (Free/Pro/Enterprise)

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
        "models": ["gpt-4o", "gpt-4-turbo", "o1"],
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

### Example 3: Org-Specific Roles

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
      "ml-engineer": {
        "models": ["gpt-4o", "claude-3-5-sonnet", "*-large"],
        "providers": ["openai", "bedrock", "anthropic"]
      },
      "data-scientist": {
        "models": ["gpt-4o", "*-large"],
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

---

## Migration Guide

### From Ad-Hoc Roles to Standard Roles

If you have existing custom roles, map them to the standard set:

**Before (ad-hoc):**
```json
{
  "roles": {
    "tier-1": { "models": ["gpt-4o-mini"] },
    "tier-2": { "models": ["gpt-4o", "claude-*"] },
    "tier-3": { "models": null },
    "auditor": { "models": null }
  }
}
```

**After (standard):**
```json
{
  "roles": {
    "user": { "models": ["gpt-4o-mini"] },
    "power-user": { "models": ["gpt-4o", "claude-*"] },
    "admin": { "models": null },
    "analyst": { "models": null }
  }
}
```

Then update your OIDC provider to emit the new role names, or configure a role mapping layer (see vendor setup guides).

---

## See Also

- [JWT Configuration](./zitadel.md) — OIDC setup with Zitadel
- [Vendor Setup Guides](../deploy/) — Auth0, Keycloak, Okta, Entra, Ping Identity
- [RBAC Configuration](./configuration.md#rbac-tiered-access) — Fine-grained access control
- [Rate Limits & Budgets](./configuration.md#rate-limiting) — Per-principal overrides from JWT claims
