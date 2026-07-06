# himadri

An **OpenAI-compatible AI gateway in Rust**. Point your existing OpenAI SDK at
one `/v1/chat/completions` endpoint and himadri routes the request to any of a
dozen LLM providers — adding authentication, routing strategies with failover,
rate limiting, spend budgets, RBAC, guardrails, circuit breaking, response
caching, and full observability along the way. A Next.js admin dashboard lives
in [`web/`](web/).

- **New to the project?** Start with the [Development Guide](DEVELOPMENT.md).
- **Configuring a deployment?** See the [docs/ directory](docs/README.md),
  especially the [configuration guide](docs/configuration.md).

## Contents

- [Features](#features)
- [Quick start](#quick-start)
- [API surface](#api-surface)
- [Configuration](#configuration)
- [Providers](#providers)
- [Routing & resilience](#routing--resilience)
- [Authentication & access control](#authentication--access-control)
- [Traffic controls](#traffic-controls)
- [Persistence](#persistence)
- [Admin API & dashboard](#admin-api--dashboard)
- [Observability](#observability)
- [Deployment](#deployment)
- [Development & testing](#development--testing)
- [Documentation index](#documentation-index)

## Features

**API compatibility**

- OpenAI-compatible endpoints: `POST /v1/chat/completions` (streaming and
  non-streaming), `POST /v1/completions`, `POST /v1/embeddings`,
  `GET /v1/models` — drop-in for OpenAI SDKs and tools.
- **Tool calling** (`tools` / `tool_choice`) forwarded and translated for all
  providers, including Anthropic, Gemini, and Bedrock native schemas;
  `tool_calls` are surfaced in both regular and streamed responses.
- Transparent `* /v1/*` passthrough proxy (behind the same auth, body capped
  at 10 MiB) for provider endpoints himadri doesn't model explicitly.

**Providers**

- OpenAI, Anthropic, Gemini, Azure OpenAI, AWS Bedrock, OpenRouter,
  Together AI, Groq, Fireworks AI, DeepInfra, Cerebras, Novita AI — plus any
  other OpenAI-compatible endpoint via a custom `base_url`.
- Providers are enabled simply by setting their API-key environment variables,
  or registered dynamically at runtime through the admin API (with optional
  AES-256-GCM encryption of stored keys).

**Routing & resilience**

- Eight routing strategies: `single`, `fallback` (retry-on-failure across
  targets), `loadbalance` (weighted), `leastlatency`, `costoptimized`,
  `conditional`, `content_based`, and `ab_test`.
- Per-target **circuit breakers** so unhealthy providers are skipped;
  automatic failover preserves target order.
- Optional `redis` build feature shares rate-limit and circuit-breaker state
  across replicas.

**Authentication & authorization**

- Three credential types side by side: a master admin key, admin-issued API
  keys (with scopes, expiry, rotation, revocation, per-key rate limits and
  budgets), and JWT/OIDC bearer tokens (e.g. [Zitadel](docs/zitadel.md)) with
  JWKS discovery and background key rotation.
- **RBAC tiered access**: per-role model/provider allow-lists with `*`
  wildcards, a `default_role`, union across roles, and admin bypass.
- **Orgs & teams**: per-organization allowed/blocked models, guardrails,
  rate limits, and token budgets, narrowed further per team.
- Auth failures (401/403) are recorded to the audit log with reason and
  client IP.

**Traffic controls**

- Rate limiting per API key, per client IP, per org, and globally
  (token bucket), with per-key overrides from JWT claims or stored keys.
- **Spend budgets**: cumulative USD caps per principal, computed from token
  usage and configured per-million-token pricing; global and per-principal
  caps compose.
- Guardrails: word-filter blocklists, `max_tokens` caps, allowed/blocked model
  lists, and a `ResponseGuardrail` trait for post-hoc response inspection.
- **Response caching**: in-process TTL cache with bounded size and
  hit/miss metrics.

**Persistence & operations**

- Pluggable storage: in-memory (zero config), SQLite (default build), or
  Postgres (`--features postgres`), with embedded migrations and a
  `--migrate` CLI flag for fail-hard schema upgrades.
- Encryption at rest for upstream provider keys (`PROVIDER_ENCRYPTION_KEY`).
- Live config reload, versioned config history, and rollback via the admin
  API — no restarts to change routing.
- Prometheus `/metrics`, structured tracing with optional OTLP export, JSONL
  audit logs, request logs, and usage/cost accounting that covers **streaming
  responses** too (usage captured from the final stream chunk, even on client
  disconnect).

## Quick start

With Docker Compose (preferred):

```bash
export OPENAI_API_KEY=sk-...       # any provider keys you have
docker compose up --build

curl http://localhost:8080/health
curl http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"model": "gpt-4o-mini", "messages": [{"role": "user", "content": "hi"}]}'
```

Or directly with Cargo:

```bash
export OPENAI_API_KEY=sk-...
cargo run -p himadri              # listens on 0.0.0.0:8080
```

> ⚠️ By default (no `MASTER_KEY`, no `DATABASE_URL`) the gateway runs with
> **auth bypassed** and **in-memory storage** — development only. The
> production-shaped setup is:

```bash
export DATABASE_URL=sqlite://himadri.db            # or postgres:// with --features postgres
export MASTER_KEY=$(openssl rand -hex 32)          # admin auth; without it auth is bypassed
export PROVIDER_ENCRYPTION_KEY=$(openssl rand -base64 32)  # encrypt stored provider keys
export OPENAI_API_KEY=sk-...
cargo run -p himadri --release
```

Full setup walkthrough (Docker, Cargo, dashboard, tests):
[DEVELOPMENT.md](DEVELOPMENT.md).

## API surface

| Endpoint | Auth | Purpose |
|---|---|---|
| `GET /health` | none | Liveness probe |
| `GET /metrics` | bearer (`METRICS_TOKEN` or `MASTER_KEY`, when set) | Prometheus metrics |
| `GET /v1/models` | none | Model list (DB-backed, else provider defaults) |
| `POST /v1/chat/completions` | bearer | OpenAI-compatible chat (streaming supported) |
| `POST /v1/completions` | bearer | Legacy completions |
| `POST /v1/embeddings` | bearer | Embeddings |
| `* /v1/*` (fallback) | bearer | Transparent proxy to the first target |
| `/admin/*` | master key (Admin scope) | Keys, providers, models, config, usage, logs |

`bearer` accepts an admin-issued API key, the master key, or (when
`JWT_ISSUER` is configured) a JWT/OIDC token.

## Configuration

Two independent sources, both optional — the gateway boots with zero config:

1. **Environment variables** — providers, persistence, auth, rate limits,
   caching, budgets, observability.
2. **A JSON config file** (`GATEWAY_CONFIG=/path/to/config.json`) — routing
   strategy and targets, orgs/teams, RBAC, guardrails, CORS. JSON only; every
   key has a default, so partial files are valid.

The complete reference — every env var, the full JSON schema, and worked
examples — is in **[docs/configuration.md](docs/configuration.md)**. CLI
flags: `--port <PORT>` (overrides `PORT`), `--migrate` (run DB migrations
before startup, exit non-zero on failure), `--help`.

## Providers

Set a provider's key env var and it is registered at startup:

| Env var(s) | Provider |
|---|---|
| `OPENAI_API_KEY` (+ `OPENAI_BASE_URL`, `OPENAI_SECONDARY_BASE_URL`) | OpenAI |
| _(always registered)_ | Anthropic (`ANTHROPIC_API_KEY`), Gemini (`GEMINI_API_KEY`) |
| `AZURE_OPENAI_API_KEY` + `AZURE_OPENAI_ENDPOINT` + `AZURE_OPENAI_DEPLOYMENT` | Azure OpenAI |
| `AWS_ACCESS_KEY_ID` + `AWS_SECRET_ACCESS_KEY` | AWS Bedrock (Bearer-token frontends; no native SigV4) |
| `OPENROUTER_API_KEY` | OpenRouter |
| `TOGETHER_API_KEY` | Together AI |
| `GROQ_API_KEY` | Groq |
| `FIREWORKS_API_KEY` | Fireworks AI |
| `DEEPINFRA_API_KEY` | DeepInfra |
| `CEREBRAS_API_KEY` | Cerebras |
| `NOVITA_API_KEY` | Novita AI |

Providers can also be created at runtime via `POST /admin/providers` (stored
in the database, optionally encrypted at rest, and routable immediately —
including after restarts). Provider base URLs set through the admin API are
validated by an SSRF guard (no loopback/private/metadata hosts unless
`ALLOW_PRIVATE_PROVIDER_URLS=1`).

See [Providers in the configuration guide](docs/configuration.md#providers).

## Routing & resilience

Targets bind a provider to a credential and optional model allowlist; the
`strategy.mode` picks how requests are distributed across them:

| Mode | Behavior |
|---|---|
| `single` | Always the first target (default) |
| `fallback` | Try targets in order; fail over on error/timeout |
| `loadbalance` | Weighted distribution |
| `leastlatency` | Lowest observed latency wins |
| `costoptimized` | Cheapest target wins |
| `conditional` | Rule-based (headers/model/etc.), with fallback strategy |
| `content_based` | Route by request content |
| `ab_test` | Traffic split across variants |

All strategies are guarded by per-target circuit breakers — targets that keep
failing are skipped until they recover. See
[Routing strategies](docs/configuration.md#routing-strategies).

## Authentication & access control

- **Master key** (`MASTER_KEY`) — protects `/admin/*` (with Admin-scope
  enforcement) and acts as a global super-key on `/v1/*`.
- **API keys** — minted via the admin API/dashboard with scopes, expiry,
  per-key rate-limit overrides, and token budgets; rotate and revoke without
  redeploying clients.
- **JWT / OIDC** — set `JWT_ISSUER` + `JWT_AUDIENCE` to accept OIDC bearer
  tokens validated against the issuer's JWKS (auto-discovered, refreshed in
  the background). `JWT_REQUIRED_ROLES` gates access by role. Full Zitadel
  walkthrough (including a user-onboarding script) in
  [docs/zitadel.md](docs/zitadel.md).
- **RBAC** — per-role model/provider allow-lists enforced on the `/v1`
  inference endpoints. See [RBAC](docs/configuration.md#rbac-tiered-access).
- **Orgs & teams** — per-org policy (models, guardrails, rate limits, token
  budgets) keyed off the principal's `org_id`/`team_id`. See
  [Orgs, teams & guardrails](docs/configuration.md#orgs-teams--guardrails).

> ⚠️ **No `MASTER_KEY` and no `JWT_ISSUER` means authentication is fully
> bypassed** (anonymous Admin). The server logs a `SECURITY:` warning. Never
> run a shared or production deployment this way.

## Traffic controls

| Control | How to enable | Docs |
|---|---|---|
| Per-key rate limit | `RATE_LIMIT_KEY_RPM` | [Rate limiting](docs/configuration.md#rate-limiting) |
| Per-IP rate limit | `RATE_LIMIT_IP_RPM` | ″ |
| Global token bucket | `rate_limit` block in JSON config | ″ |
| Spend budgets (USD) | `BUDGET_SPEND_LIMIT_USD` + `BUDGET_INPUT_PER_M_TOKENS` / `BUDGET_OUTPUT_PER_M_TOKENS` | [Budget limits](docs/configuration.md#budget-limits) |
| Response cache | `CACHE_TTL_SECS` (+ `CACHE_MAX_ENTRIES`) | [Caching](docs/configuration.md#caching) |
| Word filter | `WORD_FILTER_BLOCKLIST` | [Guardrails](docs/configuration.md#environment-variable-reference) |
| Max-tokens cap | `MAX_TOKENS_LIMIT` | ″ |

These are implemented as ordered plugins (`rate_limit`, `budget`, `cache`,
`max_token`, `word_filter`, `logger`) running at defined pipeline stages;
custom behavior can be added via the `Plugin` and `ResponseGuardrail` traits
in `himadri-plugin`.

## Persistence

| Backend | Selected by | Notes |
|---|---|---|
| In-memory | `DATABASE_URL` unset | Zero config; everything lost on restart. Dev only. |
| SQLite | `DATABASE_URL=sqlite://...` | Default build. Durable keys/providers/models/usage. |
| Postgres | `DATABASE_URL=postgres://...` + `--features postgres` | Everything durable, including request logs. |

Migrations are embedded; run them fail-hard with `--migrate`, or let the
store apply them at connect time. Details, including exactly what each
backend persists: [docs/database.md](docs/database.md).

## Admin API & dashboard

Everything operational is driven over HTTP (`/admin/*`, master key required):

- **API keys** — `GET/POST /admin/keys`, `PUT/DELETE /admin/keys/{id}`,
  `POST /admin/keys/{id}/revoke`, `POST /admin/keys/{id}/rotate`
- **Providers & models** — full CRUD plus `/toggle`; changes rebuild routing
  targets live (and survive restarts when a database is configured)
- **Config** — `GET/PUT /admin/config`, `POST /admin/reload`,
  `GET /admin/config/history`, `POST /admin/config/rollback/{version}`
- **Insight** — `GET /admin/dashboard`, `GET /admin/usage`,
  `GET /admin/usage/{key_id}`, `GET/DELETE /admin/logs`

The **admin dashboard** (`web/`, Next.js 16 / React 19) is a UI over exactly
this API — keys, providers, models, routing config, usage, and logs:

```bash
cd web && npm install && npm run dev   # http://localhost:3000
```

## Observability

- **Metrics** — Prometheus format at `GET /metrics` (protect with
  `METRICS_TOKEN`); includes request, latency, cache hit/miss, and usage
  metrics.
- **Tracing** — structured `tracing` logs with optional OTLP export
  (OpenTelemetry); tune with `RUST_LOG`.
- **Audit log** — JSONL files (one per day) under `AUDIT_LOG_DIR`, covering
  requests and auth failures; prompt/response content is excluded unless
  `AUDIT_CAPTURE_CONTENT=true` (and redacted even then).
- **Usage & cost accounting** — token usage and computed cost recorded per
  key/principal for both regular and streamed responses.

## Deployment

- [`Dockerfile`](Dockerfile) — multi-stage build producing a single binary.
- [`docker-compose.yml`](docker-compose.yml) — local single-container setup.
- [`deploy/ecs`](deploy/ecs) — AWS ECS assets for production.

## Development & testing

**[DEVELOPMENT.md](DEVELOPMENT.md)** is the canonical guide: Docker Compose
and manual Cargo setups, running the dashboard alongside the gateway, and the
gated Postgres parity test workflow. The short version:

```bash
cargo build                     # build the workspace
cargo test                      # 260+ unit/integration/e2e tests
cargo clippy --all-targets      # lint
cargo test -p himadri usecase_e2e   # end-to-end usecase suite
```

End-to-end testing notes (including a real-Zitadel OIDC stack) are in
[docs/E2E.md](docs/E2E.md). Architecture, request lifecycle, and the crate
map are in [ARCHITECTURE.md](ARCHITECTURE.md).

## Documentation index

| Document | What's inside |
|---|---|
| [DEVELOPMENT.md](DEVELOPMENT.md) | Getting a dev environment running; build/test commands; dashboard setup |
| [docs/README.md](docs/README.md) | Documentation index with key defaults to remember |
| [docs/configuration.md](docs/configuration.md) | Complete env-var reference, JSON config schema, providers, routing, auth, RBAC, budgets, orgs/teams, CORS |
| [docs/database.md](docs/database.md) | In-memory / SQLite / Postgres backends, what each persists, migrations |
| [docs/zitadel.md](docs/zitadel.md) | OIDC/JWT setup with Zitadel, role mapping, onboarding script, FAQ |
| [docs/E2E.md](docs/E2E.md) | End-to-end testing notes |
| [ARCHITECTURE.md](ARCHITECTURE.md) | System overview, request lifecycle, crate map |
| [CODE_REVIEW.md](CODE_REVIEW.md) | Active source-review checklist: correctness fixes, dead-code deletions, dedup refactors |
| [docs/GAP_ANALYSIS.md](docs/GAP_ANALYSIS.md) | Historical feature-parity analysis vs. Bifrost (see note at top — most gaps since closed) |
| [CHANGELOG.md](CHANGELOG.md) | Notable changes |
| [AGENTS.md](AGENTS.md) | Repository layout notes for contributors and coding agents |
