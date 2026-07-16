# Configuration Guide

himadri is configured through **environment variables** (runtime wiring, secrets,
providers, persistence, auth) and an optional **JSON config file** (routing
strategy, targets, orgs/teams, guardrails, CORS). Everything has a working
default, so the gateway boots with zero configuration — but that default runs
with **authentication disabled** and **in-memory storage** (see warnings below).

- [Quick start](#quick-start)
- [Command-line options](#command-line-options)
- [Configuration sources](#configuration-sources)
- [Environment variable reference](#environment-variable-reference)
- [The JSON config file](#the-json-config-file)
- [Providers](#providers)
- [Routing strategies](#routing-strategies)
- [Authentication](#authentication)
- [Rate limiting](#rate-limiting)
- [Caching](#caching)
- [Orgs, teams & guardrails](#orgs-teams--guardrails)
- [CORS](#cors)
- See also: [Database configuration](./database.md) · [Zitadel configuration](./zitadel.md)

---

## Quick start

```bash
# Minimal: proxy OpenAI, no auth, in-memory (development only)
export OPENAI_API_KEY=sk-...
cargo run -p himadri

# Production-shaped: persistence + admin auth + a provider
export DATABASE_URL=sqlite://himadri.db
export MASTER_KEY=$(openssl rand -hex 32)
export OPENAI_API_KEY=sk-...
export PORT=8080
cargo run -p himadri --release
```

The server listens on `0.0.0.0:$PORT` (default `8080`) and exposes:

| Endpoint | Auth | Purpose |
|---|---|---|
| `GET /health` | none | Liveness |
| `GET /metrics` | bearer (`METRICS_TOKEN` or `MASTER_KEY`, when configured) | Prometheus metrics |
| `GET /v1/models` | none | Model list (DB-backed, else provider defaults) |
| `POST /v1/chat/completions` | bearer | OpenAI-compatible chat |
| `POST /v1/completions` | bearer | Legacy completions |
| `POST /v1/embeddings` | bearer | Embeddings |
| `* /v1/*` (fallback) | bearer | Transparent proxy to first target |
| `/admin/*` | master key | Key/provider/model/config CRUD |

---

## Command-line options

Everything is configurable through environment variables; the binary
additionally accepts a few flags:

```
himadri [OPTIONS]

OPTIONS:
    --migrate        Migrate the database (DATABASE_URL) to the latest
                     schema version before starting
    --port <PORT>    Listen port (overrides the PORT env var; default 8080)
    -h, --help       Print help
```

- `--migrate` runs the embedded migrations (SQLite or Postgres, selected by
  `DATABASE_URL`'s scheme) to the latest version **before** the server starts,
  and exits non-zero if `DATABASE_URL` is unset or the migration fails. Use it
  in deployments where you want schema failures to stop the rollout instead of
  the default connect-time behavior (which logs the error and falls back to
  in-memory stores). See [Database configuration](./database.md#migrations).
- `--port` takes precedence over the `PORT` environment variable.

---

## Configuration sources

There are two, evaluated independently at startup:

1. **Environment variables** — read directly by `main.rs`. They control which
   providers are registered, persistence, auth, rate limiting and caching.
2. **JSON config file** — pointed to by `GATEWAY_CONFIG`. Parsed into the
   `Config` struct. If `GATEWAY_CONFIG` is unset, a built-in default config is
   used (a single OpenAI target keyed off `OPENAI_API_KEY`).

> **Only JSON is supported for the config file.** The loader dispatches on the
> file extension and rejects anything other than `.json`. A `.yaml`/`.toml` file
> will fail to load.

Precedence note: `MASTER_KEY` (env) **overrides** `admin.master_key` from the
JSON file.

---

## Environment variable reference

### Core

| Variable | Default | Description |
|---|---|---|
| `PORT` | `8080` | TCP port to bind on `0.0.0.0`. |
| `GATEWAY_CONFIG` | _(unset)_ | Path to a `.json` config file. Unset → built-in default config. |
| `MASTER_KEY` | _(unset)_ | Bearer token for `/admin/*` and a global super-key for `/v1/*`. **Unset disables all authentication** (see [Authentication](#authentication)). |
| `DATABASE_URL` | _(unset)_ | `sqlite://...` or `postgres://...`. Unset → in-memory store. See [Database](./database.md). |
| `HIMADRI_PROVIDER_SOURCE` | `auto` | Provider-routing source. `auto`: env/config targets route until the database produces targets, which then own routing (env keys stay as the fallback). `db`: strict — env provider registration is skipped entirely; boot **fails** without `DATABASE_URL` or on an unrecognized value. Every boot logs one `Provider routing: …` line stating the active source. |
| `PROVIDER_ENCRYPTION_KEY` | _(unset)_ | Base64-encoded 32-byte AES-256-GCM key (e.g. `openssl rand -base64 32`). Encrypts the `providers.api_key` column at rest. **Unset stores upstream provider API keys in plaintext** — set this in production. See [Encryption at rest](#encryption-at-rest). |

### Authentication (JWT/OIDC — e.g. Zitadel)

| Variable | Default | Description |
|---|---|---|
| `JWT_ISSUER` | _(unset)_ | OIDC issuer URL. **Setting this enables JWT auth.** |
| `JWT_AUDIENCE` | _(required with `JWT_ISSUER`)_ | Expected `aud` claim. The gateway refuses to start if `JWT_ISSUER` is set and this is empty (an empty audience would reject every token). |
| `JWT_JWKS_URI` | _(discovered)_ | Explicit JWKS endpoint; otherwise discovered from the issuer. |
| `JWT_JWKS_REFRESH_SECS` | `3600` | Background JWKS refresh interval (key rotation). |
| `JWT_REQUIRED_ROLES` | _(unset)_ | Comma-separated. If set, an authenticated principal must hold **at least one** of these roles or gets `403`. Applies to both JWT and API-key principals. |

See [Zitadel configuration](./zitadel.md) for the full OIDC setup.

### Dev / break-glass admin login

| Variable | Default | Description |
|---|---|---|
| `DEV_ADMIN_PASSWORD` | _(unset)_ | **Setting this enables the admin login** (`POST /auth/admin/login`). Use it in development without an OIDC provider, or as a break-glass credential to regain admin access when OIDC is down. Setting it also disables the dev auth bypass. |
| `DEV_ADMIN_USERNAME` | `admin` | Login name for the admin account. |
| `DEV_ADMIN_TOKEN_TTL_SECS` | `43200` (12h) | Lifetime of issued login tokens. |

The login exchanges the username+password for a short-lived admin JWT, signed
HS256 with a **random per-boot secret** — there is no signing key to configure
or leak, and restarting the gateway revokes every issued token. Failed logins
are audit-logged (with source IP) and rate-slowed. When disabled, the endpoint
answers `404`.

```bash
curl -X POST http://localhost:8080/auth/admin/login \
  -H 'content-type: application/json' \
  -d '{"username":"admin","password":"…"}'
# → {"access_token":"eyJ…","token_type":"Bearer","expires_in":43200}
```

**The dashboard signs in exclusively through this account** — the old
master-key login form was removed. The master key remains valid as an API
bearer token (curl, scripts, `/metrics`), but to use the web dashboard set
`DEV_ADMIN_PASSWORD`. The dashboard then holds only the short-lived login
token, never the master key itself.

### Rate limiting & caching

| Variable | Default | Description |
|---|---|---|
| `RATE_LIMIT_KEY_RPM` | _(unset)_ | Per-API-key requests/minute (registers a rate-limit plugin). |
| `RATE_LIMIT_IP_RPM` | _(unset)_ | Per-client-IP requests/minute. |
| `CACHE_TTL_SECS` | _(unset)_ | Enables response caching with this TTL. |
| `CACHE_MAX_ENTRIES` | `10000` | Max cached responses (only with `CACHE_TTL_SECS`). |

### Guardrails & observability

| Variable | Default | Description |
|---|---|---|
| `WORD_FILTER_BLOCKLIST` | _(unset)_ | Comma-separated words; requests containing any of them are rejected with `400`. Unset disables the word filter. |
| `MAX_TOKENS_LIMIT` | _(unset)_ | Reject requests whose `max_tokens` exceeds this cap. Unset disables the cap. |
| `GUARDRAILS_PII_MODE` | _(unset)_ | Global default for the PII guardrail: `redact` (rewrite spans inline before provider dispatch), `block` (reject with `400`), or `observe` (metrics only). Unset → no global default; config-file `guardrails.pii` sections still apply. |
| `GUARDRAILS_PII_STRATEGY` | `replace` | How redacted spans are rewritten: `replace` (`[EMAIL_ADDRESS]`), `mask`, `hash`, `encrypt`, `remove`. |
| `GUARDRAILS_PII_ENTITIES` | _(all)_ | Comma-separated entity types to act on (e.g. `EMAIL_ADDRESS,US_SSN,CREDIT_CARD`). |
| `GUARDRAILS_PII_MIN_CONFIDENCE` | `0.6` | Detections below this confidence are ignored. |
| `GUARDRAILS_PII_SCAN_TOOL_ARGS` | `false` | Also scan tool-call argument strings. |
| `GUARDRAILS_PII_FAIL_OPEN` | `false` | On engine errors, forward unscanned instead of failing the request (response side: allow instead of withholding). Truthy values are `1`, `true`, `yes` (previously only `true` was honored — `1`/`yes` were silently ignored and failed closed). |
| `GUARDRAILS_PII_RESPONSE_MODE` | `off` | Global default for scanning **model output**: `observe`, `redact`, or `block`. Non-streaming only; for streams the check is post-hoc at stream end. |
| `GUARDRAILS_HASH_SALT` | _(unset)_ | Salt for the `hash` strategy. Env-only secret — never part of the config file. |
| `GUARDRAILS_ENCRYPTION_KEY` | _(unset)_ | Key for the `encrypt` strategy (required to use it). Env-only secret. |
| `GUARDRAILS_INLINE_LIMIT_BYTES` | `16384` | Scans larger than this run on the blocking thread pool. |
| `AUDIT_LOG_DIR` | _(unset)_ | Directory for JSONL audit logs (one file per day). Unset → audit events go to tracing output. |
| `AUDIT_CAPTURE_CONTENT` | `false` | Include prompt/response content in audit events (always redacted). Off by default so user content never reaches logs/telemetry. |
| `METRICS_TOKEN` | _(unset)_ | Dedicated bearer token for `GET /metrics`. Falls back to `MASTER_KEY`; if neither is set (dev mode), metrics are unauthenticated. |

> **Distributed tracing.** Spans can be exported over OTLP/gRPC to an
> OpenTelemetry Collector via the `observability.tracing` block in the JSON
> config file. See **[OpenTelemetry Tracing](opentelemetry.md)** for the full
> setup, endpoint/TLS resolution, sampling, and a collector example.

### Budget

| Variable | Default | Description |
|---|---|---|
| `BUDGET_SPEND_LIMIT_USD` | _(unset)_ | Global cumulative spend cap per principal (USD). |
| `BUDGET_INPUT_PER_M_TOKENS` | _(unset)_ | Price per 1M input (prompt) tokens. |
| `BUDGET_OUTPUT_PER_M_TOKENS` | _(unset)_ | Price per 1M output (completion) tokens. |

See [Budget limits](#budget-limits) for how global and per-principal caps interact.

### Provider registration

OpenAI, Anthropic and Gemini are always registered. Every other provider is
registered only when its variable(s) below are set. Each OpenAI-compatible vendor
is gated on `{PROVIDER_TYPE}_API_KEY` — `openrouter` on `OPENROUTER_API_KEY`, and
so on for the rest.

This whole section applies to env/config routing. Once the database provides
routing targets (models + endpoints via the admin API), it owns routing and the
variables below stop feeding it — the gateway warns at boot naming any that are
set, and they remain the routing *fallback* if the database stops producing
targets. Under `HIMADRI_PROVIDER_SOURCE=db` they are never registered at all.

| Variable(s) | Provider |
|---|---|
| `OPENAI_API_KEY`, `OPENAI_BASE_URL`, `OPENAI_SECONDARY_BASE_URL` | OpenAI (always registered; secondary base URL adds a fallback OpenAI target) |
| _(always registered)_ | Anthropic, Gemini |
| `AZURE_OPENAI_API_KEY` + `AZURE_OPENAI_ENDPOINT` + `AZURE_OPENAI_DEPLOYMENT` (+ `AZURE_OPENAI_API_VERSION`, default `2024-10-21`) | Azure OpenAI |
| `OPENROUTER_API_KEY` | OpenRouter |
| `TOGETHER_API_KEY` | Together AI |
| `GROQ_API_KEY` | Groq |
| `FIREWORKS_API_KEY` | Fireworks AI |
| `DEEPINFRA_API_KEY` | DeepInfra |
| `CEREBRAS_API_KEY` | Cerebras |
| `NOVITA_API_KEY` | Novita AI |

> **How keys flow:** the env var above *registers/enables* the provider. The key
> actually used at request time is resolved from the **target's `api_key_env`**
> in the config file (see [Providers](#providers)). For the built-in default
> config the single target uses `OPENAI_API_KEY`.

---

## The JSON config file

Set `GATEWAY_CONFIG=/path/to/config.json`. Full schema with defaults:

```json
{
  "strategy": {
    "mode": "single",
    "fallback_timeout_ms": 0,
    "conditional_rules": [],
    "content_rules": [],
    "ab_variants": [],
    "strategy_fallback": null
  },
  "targets": [
    {
      "provider": "openai",
      "weight": 1.0,
      "models": null,
      "api_key_env": "OPENAI_API_KEY",
      "base_url": null
    }
  ],
  "plugins": [],
  "observability": {
    "tracing": {
      "enabled": false,
      "service_name": "himadri",
      "endpoint": null,
      "sample_ratio": 1.0
    },
    "metrics": {
      "enabled": true,
      "path": "/metrics"
    }
  },
  "rate_limit": {
    "enabled": false,
    "requests_per_second": 100,
    "burst_size": 200
  },
  "admin": {
    "enabled": true,
    "master_key": null
  },
  "orgs": {},
  "cors": {
    "enabled": true,
    "allowed_origins": [],
    "allowed_methods": ["GET", "POST", "PUT", "DELETE", "OPTIONS"],
    "allowed_headers": ["Authorization", "Content-Type"]
  }
}
```

Every top-level key has a serde default, so a partial config is valid — e.g. a
file containing only `{ "targets": [...] }` is fine.

### Target fields

| Field | Default | Description |
|---|---|---|
| `provider` | _(required)_ | The name the provider is registered under: `openai`, `openai-secondary`, `anthropic`, `gemini`, `azure-openai`, `openrouter`, `together`, `groq`, `fireworks`, `deepinfra`, `cerebras`, `novita`. Must match a registered provider (see [Provider registration](#provider-registration)) or the target never resolves. |
| `weight` | `1.0` | Relative weight for `loadbalance`. |
| `models` | `null` | Restrict this target to specific model IDs. |
| `api_key_env` | `null` | Env var holding the API key for this target. |
| `base_url` | `null` | Override the provider's default base URL. |

---

## Providers

Providers are registered at startup based on env vars (see the table above).
Routing across them is driven by the `targets` array and the `strategy.mode`.

A target binds a provider to a credential and (optionally) a model allowlist:

```json
{
  "strategy": { "mode": "fallback", "fallback_timeout_ms": 5000 },
  "targets": [
    { "provider": "openai",    "api_key_env": "OPENAI_API_KEY" },
    { "provider": "anthropic", "api_key_env": "ANTHROPIC_API_KEY" }
  ]
}
```

The OpenAI, Anthropic and Gemini providers are always registered; the rest
register only when their key env var is present. Anthropic and Gemini use their
native auth schemes internally (`x-api-key` / `?key=`), not Bearer.

### Encryption at rest

Providers created via `POST/PUT /admin/providers` (not the env-var-registered
ones above) store their `api_key` in the `providers` table. Set
`PROVIDER_ENCRYPTION_KEY` to encrypt that column with AES-256-GCM instead of
storing it in plaintext:

```bash
export PROVIDER_ENCRYPTION_KEY=$(openssl rand -base64 32)
```

- Ciphertext is stored as `enc:v1:<base64>`; the API always returns the
  decrypted plaintext to authenticated admin callers.
- Rows written before the key was set remain readable (they're plaintext with
  no `enc:v1:` prefix) and are transparently re-encrypted the next time
  they're updated — no migration step needed.
- Losing the key makes existing encrypted rows permanently undecryptable;
  back it up the same way you'd back up `MASTER_KEY`.
- This does **not** cover the `api_keys` table (client-facing gateway keys) —
  those are opaque bearer tokens, not upstream credentials, and are looked up
  by exact match rather than decrypted.

---

## Routing strategies

`strategy.mode` (serialized lowercase):

| Mode | Behavior |
|---|---|
| `single` | Always use the first target. (Default.) |
| `fallback` | Try targets in order; advance on failure/timeout (`fallback_timeout_ms`). |
| `loadbalance` | Distribute by `weight`. |
| `leastlatency` | Pick the target with lowest observed latency. |
| `costoptimized` | Prefer the cheapest target. |
| `conditional` | Match `conditional_rules` in order; else `strategy_fallback`. |
| `content_based` | Match `content_rules` (by request content); else `strategy_fallback`. |
| `ab_test` | Split traffic across `ab_variants`. |

---

## Authentication

Two credential types are accepted on `/v1/*`, in this order:

1. **JWT / OIDC bearer token** — validated against `JWT_ISSUER`'s JWKS (enabled
   only when `JWT_ISSUER` is set). See [Zitadel](./zitadel.md).
2. **API key / master key** — validated against the key store. The `MASTER_KEY`
   acts as a global super-key.

`/admin/*` goes through the same combined authentication and additionally
requires **Admin scope**: the master key, an admin-scoped API key, a
[dev/break-glass admin login](#dev--break-glass-admin-login) token, or an OIDC
token carrying an `admin`/`gateway-admin` role.

> ⚠️ **No `MASTER_KEY`, no `JWT_ISSUER`, and no `DEV_ADMIN_PASSWORD` =
> authentication is fully bypassed.** In that mode every request is treated as
> an anonymous principal with **Admin** scope. The server logs a `SECURITY:`
> warning at startup. This is intended for local development only — configure
> at least one auth mechanism in any shared or production deployment
> (production/staging deployments additionally refuse to start without
> `MASTER_KEY`).

### Roles & scopes

Authorization derives an `AuthScope` (`Admin` / `ReadOnly` / `ApiKey`) and a list
of `roles`:

- **JWT:** roles come from the flat `roles` claim **and** Zitadel's
  `urn:zitadel:iam:org:project:roles` claim. An `admin`/`gateway-admin` role →
  `Admin`; `read-only`/`readonly`/`read` → `ReadOnly`.
- **API key:** roles come from the key's stored `scopes`.
- **`JWT_REQUIRED_ROLES`** gates access: if set, principals lacking every listed
  role receive `403`.

For fine-grained, per-role model/provider access, see [RBAC](#rbac-tiered-access).

### Auth-failure auditing

Authentication and authorization failures on `/v1/*` are recorded to the audit
log with status `unauthorized` (401) or `forbidden` (403), including the reason
and client IP. This covers missing/invalid/expired tokens and failed
`JWT_REQUIRED_ROLES` checks. (RBAC model/provider denials return `403` from the
gateway and are surfaced to the client; see below.)

---

## Rate limiting

Two independent mechanisms:

- **Env-driven plugins:** `RATE_LIMIT_KEY_RPM` (per key) and `RATE_LIMIT_IP_RPM`
  (per IP) register rate-limit plugins at startup.
- **Config `rate_limit`:** global token-bucket (`requests_per_second`,
  `burst_size`) when `enabled: true`. Per-org overrides live under `orgs`.

Per-key overrides can also arrive from a JWT (`rate_limit_rpm` claim) or a
stored API key's rate-limit override.

---

## Caching

Set `CACHE_TTL_SECS` to enable an in-process response cache (LRU-ish, bounded by
`CACHE_MAX_ENTRIES`, default 10 000). Identical requests within the TTL are
served from cache.

---

## Budget limits

The budget plugin enforces a **cumulative USD spend cap per principal**
(identified by API key, or `jwt:<sub>` for OIDC users). Cost is computed from
each response's token usage and the configured per-token prices.

**Enable it** by setting a global cap, token pricing, or both:

```bash
export BUDGET_INPUT_PER_M_TOKENS=3.0      # $3 / 1M prompt tokens
export BUDGET_OUTPUT_PER_M_TOKENS=15.0    # $15 / 1M completion tokens
export BUDGET_SPEND_LIMIT_USD=100         # optional global cap per principal
```

How the caps interact:

| Global limit | Per-principal cap (JWT `budget_limit_usd` / key) | Effective limit |
|---|---|---|
| set | unset | global |
| set | set (> 0) | **per-principal** (overrides global) |
| unset / 0 | set (> 0) | per-principal |
| unset / 0 | unset | unlimited (cost still tracked if pricing set) |

- A **per-principal cap takes precedence** over the global cap when present.
- **Pricing is required for any enforcement.** With both prices at 0, every
  request costs $0 and no cap is ever reached. If `BUDGET_SPEND_LIMIT_USD > 0`
  but no pricing is set, the plugin refuses to register (it would never fire).
- Per-principal caps come from the JWT `budget_limit_usd` claim, or an API key's
  `token_budget.cost_limit_per_month`.
- Accounting is **cumulative for the process lifetime** and held in memory (per
  `store_id`). There is **no automatic daily/monthly reset yet**; restarting the
  gateway clears accumulated spend.
- Budgets are checked **before** the request and recorded **after** a successful
  response. Enforcement applies to non-streaming `/v1/chat/completions`.

## RBAC (tiered access)

RBAC grants **different roles different access to models and providers** on the
`/v1` endpoints — the mechanism for tiered/differentiated access. It keys off the
principal's `roles` (Zitadel project roles for JWTs, or an API key's scopes).

Configured under `rbac` in the JSON config:

```json
{
  "rbac": {
    "enabled": true,
    "default_role": "analyst",
    "roles": {
      "analyst":     { "models": ["gpt-4o-mini"] },
      "engineer":    { "models": ["gpt-4o", "o1", "claude-*"] },
      "ml-platform": { "providers": ["openai", "anthropic"] },
      "gateway-admin": {}
    }
  }
}
```

Semantics:

- **Disabled by default.** With `enabled: false` (or absent) RBAC is a no-op.
- **`models` / `providers`** are allow-lists supporting `*` wildcards
  (`claude-*`, `*-mini`, `*`). A **missing/`null`** field means *no restriction*
  on that dimension for the role (e.g. `ml-platform` above may use any model but
  only the `openai`/`anthropic` providers; `gateway-admin` with `{}` may use
  anything).
- **Union across roles** — a principal holding multiple roles gets the most
  permissive combination.
- **Admin bypass** — principals with `AuthScope::Admin` (master key, or a JWT
  mapped to admin) skip RBAC entirely.
- **`default_role`** — applied to authenticated principals whose roles match no
  entry (e.g. API-key callers, or users without a tier). If unset and no role
  matches, access is **denied** (`403`).

Enforcement points:

- **Model** — checked at request entry against `request.model` → `403` if not
  allowed.
- **Provider** — the candidate targets are filtered to permitted providers
  (preserving failover order); if none remain → `403`.

Applies to `/v1/chat/completions`, `/v1/completions`, and `/v1/embeddings`.

## Orgs, teams & guardrails

The `orgs` map enforces per-organization policy, keyed by the principal's
`org_id` (from the JWT `org_id` claim or an API key's stored org). Each org:

```json
{
  "orgs": {
    "acme": {
      "name": "Acme Corp",
      "enabled": true,
      "allowed_models": ["gpt-4o", "claude-3-5-sonnet-20241022"],
      "blocked_models": null,
      "rate_limit": { "enabled": true, "requests_per_second": 50, "burst_size": 100 },
      "token_budget": { "max_tokens_per_request": 8192 },
      "guardrails": {
        "enabled": true,
        "blocked_words": ["secret-project-x"],
        "max_tokens_per_request": 8192
      },
      "teams": {
        "research": { "allowed_models": ["gpt-4o"], "token_budget": { "max_tokens_per_request": 4096 } }
      }
    }
  }
}
```

Guardrails (allowed/blocked models, blocked words, max tokens) are enforced on
`/v1/chat/completions` and `/v1/embeddings`. Team config narrows org config for
principals carrying a matching `team_id`:

- **Cumulative rules** — model allow/block lists, token budgets, blocked words,
  and `max_tokens_per_request` are enforced at *every* scope that states them;
  a team can only add restrictions, never widen the org's.
- **Per-scope switch** — each scope's `guardrails.enabled` gates only that
  scope's own words/token guardrails (an org with guardrails off does not
  switch off a team's).
- **PII is the exception** — a scope's `guardrails.pii` section overrides
  wholesale: the most specific scope with one decides entirely, including
  `enabled: false` to opt a team out of a global or org policy (see
  [PII guardrail](#pii-guardrail-guardrailspii)).

> **Note:** these checks run on the typed inference endpoints. The transparent
> `/v1/*` proxy fallback does **not** currently apply org guardrails.

### PII guardrail (`guardrails.pii`)

The PII guardrail detects and redacts/blocks PII (emails, SSNs, credit
cards, API keys, …) in request messages **before they are forwarded to any
provider** (see [SPEC_GUARDRAILS.md](SPEC_GUARDRAILS.md)). The redacted
request is the request of record: the provider, response cache, and audit
log all see redacted content.

A global default lives at the top level of the config file and hot-applies
on `/admin/config` reloads:

```json
{
  "guardrails": {
    "pii": {
      "enabled": true,
      "mode": "redact",
      "strategy": "replace",
      "entities": null,
      "min_confidence": 0.6,
      "apply_to": ["user", "system", "tool"],
      "scan_tool_arguments": false,
      "fail_open": false,
      "response_mode": "off"
    }
  }
}
```

Orgs and teams may carry their own `guardrails.pii` section. A present
section **replaces the global settings wholesale** for that scope —
including `"enabled": false` to opt a scope out of a global policy. A
team's section beats its org's; an org's beats the global one; the global
config-file section beats the `GUARDRAILS_PII_*` env defaults.

Secrets for the `hash`/`encrypt` strategies are env-only
(`GUARDRAILS_HASH_SALT`, `GUARDRAILS_ENCRYPTION_KEY`) and deliberately not
part of the config file: `GET /admin/config` serializes the config
verbatim.

The legacy `content_filter.block_pii` flag is deprecated; configs still
using it are mapped to `pii: { enabled: true, mode: "block" }` on load
(with a warning) when no explicit `pii` section is present.

Setting `response_mode` (per scope, same override rules) additionally
scans **model output**: `redact` rewrites the response before the client
sees it, `block` withholds it with a `400`, `observe` records metrics
only. On engine errors the response side fails closed by withholding the
response (unless `fail_open`).

> **Scope:** request content is scanned on `/v1/chat/completions` and
> `/v1/completions`. Response scanning is enforced on **non-streaming**
> responses; for streams it runs on the buffered text at end-of-stream
> only — chunks already delivered cannot be recalled, so stream actions
> are logged/metered, not enforced. `/v1/embeddings` inputs and the
> `/v1/*` proxy are not scanned.

---

## CORS

`cors` controls the browser CORS layer. Defaults: enabled, all origins allowed
when `allowed_origins` is empty, methods `GET/POST/PUT/DELETE/OPTIONS`, headers
`Authorization/Content-Type`. Set `enabled: false` to disable, or list explicit
origins to restrict.
