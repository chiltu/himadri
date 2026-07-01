# Zitadel Configuration & FAQ

himadri authenticates `/v1/*` requests with OIDC JWTs in addition to gateway API
keys. [Zitadel](https://zitadel.com) is a first-class OIDC provider for this: a
user logs into Zitadel, Zitadel issues a JWT access token, and the client sends
it as `Authorization: Bearer <jwt>`. The gateway validates it against Zitadel's
JWKS and maps its claims (including **project roles**) into the request's
authorization context.

- [How it works](#how-it-works)
- [Step 1 — Create a project & roles in Zitadel](#step-1--create-a-project--roles-in-zitadel)
- [Step 2 — Create an application (client)](#step-2--create-an-application-client)
- [Step 3 — Project the role claim into tokens](#step-3--project-the-role-claim-into-tokens)
- [Step 4 — Configure the gateway](#step-4--configure-the-gateway)
- [Claim mapping reference](#claim-mapping-reference)
- [Onboarding users](#onboarding-users)
- [FAQ / Troubleshooting](#faq--troubleshooting)

See also: [Configuration guide](./configuration.md) · [Database](./database.md)

---

## How it works

```
┌────────┐   1. login    ┌──────────┐   2. JWT (access token)
│  User  │ ────────────▶ │ Zitadel  │ ─────────────────────────┐
└────────┘               └──────────┘                          │
     │                                                          ▼
     │  3. Bearer <jwt>                                  ┌─────────────┐
     └─────────────────────────────────────────────────▶│   himadri   │
                                                          │  gateway    │
   4. validate signature against Zitadel JWKS            └─────────────┘
   5. check exp/nbf/audience
   6. map claims → AuthContext (sub, org, roles, rate limit)
   7. optional JWT_REQUIRED_ROLES gate → 403 if missing
```

The middleware tries a bearer token as a JWT first (when `JWT_ISSUER` is set and
the token is JWT-shaped); if that fails it falls back to API-key validation. So
Zitadel tokens and gateway API keys both work on the same endpoints.

---

## Step 1 — Create a project & roles in Zitadel

1. In the Zitadel console, create (or pick) a **Project**.
2. Under the project's **Roles**, add the roles your gateway should recognize.
   The gateway maps role *keys* directly, with two reserved meanings:

   | Role key | Effect in gateway |
   |---|---|
   | `admin` or `gateway-admin` | `AuthScope::Admin` |
   | `read-only`, `readonly`, or `read` | `AuthScope::ReadOnly` |
   | anything else (e.g. `user`) | `AuthScope::ApiKey` (normal access), stored in `roles` |

   You can define any additional role keys you like (e.g. `user`,
   `data-science`); they are carried in the principal's `roles` list and can be
   required via `JWT_REQUIRED_ROLES` or used to grant **tiered access to specific
   models/providers** via [RBAC](./configuration.md#rbac-tiered-access). For
   example, an `analyst` role limited to `gpt-4o-mini` while `engineer` gets
   `gpt-4o` and `claude-*`.
3. Note the **Project resource ID** — you'll need it for onboarding grants.

## Step 2 — Create an application (client)

Create an **Application** inside the project for the client that will obtain
tokens (e.g. a Web, Native, or API app, depending on your flow). Record its
**Client ID** — this is the value you'll set as `JWT_AUDIENCE` (Zitadel includes
the project's client IDs in the token `aud`).

## Step 3 — Project the role claim into tokens

By default Zitadel does **not** put project roles in the access token. Enable it:

- On the **Application** (or project) settings, turn on
  **"Assert Roles on Authentication"** (and, for access tokens, ensure roles are
  added to the token rather than only userinfo).
- Request the appropriate scopes from your client. Useful Zitadel reserved
  scopes:
  - `openid profile email` — standard.
  - `urn:zitadel:iam:org:project:id:{projectid}:aud` — adds the project to the
    audience (so `JWT_AUDIENCE` matching works).
  - `urn:zitadel:iam:org:projects:roles` — request roles in the token.

When enabled, the token contains a claim like:

```json
{
  "sub": "298069234735512345",
  "iss": "https://your-instance.zitadel.cloud",
  "aud": ["298069200000000000"],
  "urn:zitadel:iam:org:project:roles": {
    "admin": { "298069100000000000": "acme.zitadel.cloud" },
    "user":  { "298069100000000000": "acme.zitadel.cloud" }
  }
}
```

The gateway also accepts the project-scoped variant
`urn:zitadel:iam:org:project:{project_id}:roles`. In both cases the **keys** of
that object (`admin`, `user`, …) are the granted role names.

### Optional: per-user rate limit / budget metadata

If you set Zitadel **user metadata** keys `rate_limit_rpm` and/or
`budget_limit_usd` (and configure the project to project metadata into tokens as
custom claims of those names), the gateway will read them:

- `rate_limit_rpm` → per-key rate-limit override (RPS = `rpm / 60`).
- `budget_limit_usd` → per-principal cumulative USD spend cap, enforced by the
  budget plugin. **Requires the gateway to have token pricing configured**
  (`BUDGET_INPUT_PER_M_TOKENS` / `BUDGET_OUTPUT_PER_M_TOKENS`) so cost can be
  computed — otherwise cost is always 0 and the cap never triggers. See
  [Budget limits](./configuration.md#budget-limits).

## Step 4 — Configure the gateway

Set these environment variables and (re)start himadri:

```bash
export JWT_ISSUER="https://your-instance.zitadel.cloud"   # enables JWT auth
export JWT_AUDIENCE="298069200000000000"                  # your app's client id
# export JWT_JWKS_URI="..."        # optional; otherwise discovered from issuer
# export JWT_JWKS_REFRESH_SECS=3600 # optional; key-rotation refresh interval

# Optional: require a role to use the gateway (else 403)
export JWT_REQUIRED_ROLES="user,admin"
```

On startup you should see:

```
INFO JWT/OIDC authentication enabled (issuer: https://your-instance.zitadel.cloud)
```

Verify end-to-end:

```bash
curl -s https://gateway:8080/v1/chat/completions \
  -H "Authorization: Bearer $ZITADEL_ACCESS_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}'
```

---

## Claim mapping reference

| JWT claim | Maps to `AuthContext` | Notes |
|---|---|---|
| `sub` | `user_id`, `key_id`, `api_key="jwt:<sub>"` | Principal identity. |
| `urn:zitadel:iam:org:project:roles` (object keys) + flat `roles` | `roles` (+ derived `scope`) | `admin`/`gateway-admin` → Admin; `read-only`/`readonly`/`read` → ReadOnly. |
| `scope` (OAuth string) | `scope` fallback | Used only if roles don't determine scope. |
| `org_id` | `org_id` | Used for org guardrails. **Custom claim — see FAQ.** |
| `team_id` | `team_id` | Used for team guardrails. **Custom claim.** |
| `rate_limit_rpm` | `rate_limit_override` | Custom claim/metadata. RPS = rpm/60. |
| `budget_limit_usd` | `budget_limit_usd` | Custom claim/metadata. Cumulative USD cap; enforced when pricing is configured. |
| `exp`, `nbf` | validation | Expired / not-yet-valid tokens are rejected. |

---

## Onboarding users

Use the bundled script to provision a user, grant roles, and stamp metadata via
Zitadel's REST APIs:

```bash
export ZITADEL_DOMAIN="https://your-instance.zitadel.cloud"
export ZITADEL_PAT="<service-user personal access token>"
export ZITADEL_PROJECT_ID="298069..."

scripts/zitadel_onboard.sh \
  --email jane@example.com \
  --first-name Jane --last-name Doe \
  --username jane \
  --roles user,admin \
  --rate-limit-rpm 600 \
  --budget-usd 50
```

What it does:

1. Creates the human user (v2 user API). With no `--password` it triggers an
   initialization email so the user sets their own; `--verified` skips email
   verification.
2. Grants the named **project roles** (these become the `roles` claim).
3. Optionally writes `rate_limit_rpm` / `budget_limit_usd` user metadata.

The service-user token (`ZITADEL_PAT`) needs org user-management permissions
(`ORG_OWNER` or `USER_MANAGER`) plus `PROJECT_OWNER` to grant roles. Run
`scripts/zitadel_onboard.sh --help` for all flags. The script is
idempotent-friendly: re-running resolves the existing user and skips duplicate
grants.

---

## FAQ / Troubleshooting

**Q: I'm authenticated but my Zitadel roles don't seem to apply.**
Roles only appear in the token if you enabled **"Assert Roles on
Authentication"** (Step 3) *and* requested the roles scope. Decode your JWT
(e.g. at jwt.io) and confirm a `urn:zitadel:iam:org:project:roles` (or
`...:project:{id}:roles`) claim is present. If it's missing, the gateway sees no
roles.

**Q: Requests return 401 even with a valid-looking token.**
Check, in order: (1) `JWT_ISSUER` exactly matches the token's `iss` (scheme,
host, no trailing slash mismatch); (2) the token isn't expired (`exp`); (3) the
JWKS is reachable from the gateway — look for `JWKS refresh failed` warnings;
(4) the token is a JWT (three dot-separated segments) — opaque tokens fall
through to API-key validation and will 401 if not a known key.

**Q: Requests return 403.**
You've set `JWT_REQUIRED_ROLES` and the principal holds none of those roles.
Either grant the user one of the required roles (onboarding script) or adjust
`JWT_REQUIRED_ROLES`.

**Q: Audience validation fails.**
Set `JWT_AUDIENCE` to your application's **Client ID**, and make sure the project
is added to the token audience — request the
`urn:zitadel:iam:org:project:id:{projectid}:aud` scope so Zitadel includes it in
`aud`. The gateway accepts `aud` as either a string or an array.

**Q: `org_id` / `team_id` are always empty, so org guardrails don't fire.**
Zitadel does **not** emit `org_id`/`team_id` as top-level claims by default. You
must add them as **custom claims** with exactly those names (via a Zitadel Action
that projects org/metadata into the token). Until then, `org_id`/`team_id` are
`None` and org/team policy is skipped.

**Q: Can I use both Zitadel JWTs and gateway API keys at the same time?**
Yes. The middleware tries JWT validation first (for JWT-shaped tokens) and falls
back to API-key validation. Both are accepted on `/v1/*`. `/admin/*` always
requires the master key.

**Q: I set `rate_limit_rpm` to 30 and it seems unlimited.**
Rate is computed as `rpm / 60` (integer). Values under 60 truncate toward 0 RPS.
Use 60+ for meaningful per-user limits.

**Q: `budget_limit_usd` in my token does nothing.**
The per-principal cap is enforced only when the gateway can compute request cost
— set `BUDGET_INPUT_PER_M_TOKENS` and `BUDGET_OUTPUT_PER_M_TOKENS`. Without
pricing, every request costs $0 and no cap is ever reached. The cap is
cumulative (lifetime) spend per principal; there is no automatic monthly reset
yet. See [Budget limits](./configuration.md#budget-limits).

**Q: How does key rotation work?**
The gateway refreshes Zitadel's JWKS every `JWT_JWKS_REFRESH_SECS` (default
3600s) in the background, so rotated signing keys are picked up without a
restart.

**Q: Does the transparent `/v1/*` proxy enforce roles/guardrails?**
The proxy authenticates the request (and the `JWT_REQUIRED_ROLES` gate applies),
but it does **not** currently apply org guardrails or per-user budget checks.
Prefer the typed endpoints (`/v1/chat/completions`, `/v1/embeddings`) where full
policy is enforced.
