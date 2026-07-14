# Architecture

A high-level map of how **himadri** is put together. For repository layout
and build commands see [`AGENTS.md`](AGENTS.md); for configuration and
persistence details see [`docs/configuration.md`](docs/configuration.md) and
[`docs/database.md`](docs/database.md).

## System overview

```
                         ┌───────────────────────┐
                         │   web/ (Next.js UI)   │
                         │  admin dashboard       │
                         └───────────┬────────────┘
                                     │ /admin/* (master key)
                                     ▼
 clients ──▶ /v1/chat/completions   ┌────────────────────────────────────┐
 clients ──▶ /v1/completions        │            himadri (axum)          │
 clients ──▶ /v1/embeddings   ────▶ │  auth → plugins → Gateway.route()  │
 clients ──▶ /v1/* (proxy)          └───────────────┬────────────────────┘
                                                     │
                        ┌────────────────────────────┼────────────────────────────┐
                        ▼                            ▼                            ▼
                 himadri-plugin(s)            himadri-provider              himadri-admin
             pii_guardrail / budget /     OpenAI, Anthropic, Gemini,   API keys, providers,
             cache / logger / max_token    Azure, OpenRouter,           models, usage & request
             rate_limit / word_filter      OpenAI-compatible             logs (in-memory / SQLite
                                                                          / Postgres)
                        │                            │                            │
                        ▼                            ▼                            ▼
               himadri-ratelimit /          upstream provider APIs       himadri-observability
               himadri-circuitbreaker                                    (metrics, tracing)
```

The binary is a single axum HTTP server (`crates/himadri`). Everything else
in `crates/` is a library crate it composes.

## Request lifecycle

1. **HTTP layer** (`crates/himadri/src/main.rs`, `handlers.rs`) — axum routes
   requests to handlers for `/health`, `/metrics`, `/v1/models`,
   `/v1/chat/completions`, `/v1/completions`, `/v1/embeddings`, a catch-all
   `/v1/*` proxy, and `/admin/*`.
2. **Auth** (`combined_auth.rs`, `himadri-auth`, `himadri-admin`) — bearer
   tokens are checked against the API-key store; `/admin/*` requires the
   `MASTER_KEY` (or OIDC/JWT if `JWT_ISSUER` is configured). If no
   `MASTER_KEY`/`JWT_ISSUER` is set, auth is bypassed entirely (dev only).
3. **Plugin pipeline** (`himadri-plugin` trait, impls in `himadri-plugins`) —
   ordered `Plugin`s run at defined `Stage`s (e.g. pre-request, post-response)
   and can short-circuit a request: `pii_guardrail` (inline PII
   redaction/blocking via `redact-core`, resolved per org/team against the
   live config — see `docs/SPEC_GUARDRAILS.md`), `rate_limit`, `budget`,
   `max_token`, `word_filter`, `cache` (serves cached responses), `logger`.
   Before-request plugins may rewrite `ctx.request`; the gateway forwards
   the pipeline's copy to providers, so redactions are what the upstream,
   the response cache, and the audit log see. A separate
   `ResponseGuardrail` trait allows post-hoc response inspection/blocking.
4. **Routing** (`crates/himadri/src/strategy.rs`, `Gateway::route` in
   `gateway/route.rs`) — the `Gateway` holds a set of configured `Target`s (provider
   + model + weight/priority) and picks one per request according to the
   active routing strategy (e.g. priority, weighted, round-robin — see
   `strategy.rs`), guarded by `himadri-circuitbreaker` so unhealthy targets
   are skipped.
5. **Provider dispatch** (`himadri-provider`, `Provider` trait) — the chosen
   target's provider implementation translates the OpenAI-shaped request into
   the upstream API's format, calls it, and translates the response/stream
   back. OpenAI-shaped vendors (OpenAI, Azure, OpenRouter, Groq, Together,
   Fireworks, …) are all configuration presets of the single
   `OpenAiCompatibleProvider`; only Anthropic and Gemini have bespoke
   implementations. All streaming responses are decoded by the shared SSE
   module (`sse.rs`) and go through `Gateway::route_stream`.
6. **Post-processing** — usage is recorded (`himadri-admin::UsageStore`),
   request logs are persisted (`RequestLogStore` — Postgres-only durability,
   see `docs/database.md`), and metrics/traces are emitted via
   `himadri-observability`. Streamed requests are accounted the same way: a
   `StreamUsageRecorder` captures usage from the final stream chunk and
   records at stream end (or client disconnect).

The `/v1/*` fallback in step 1 (`Gateway::proxy`) sits behind the same
bearer-token auth middleware as the other `/v1` endpoints and transparently
forwards anything not matched by a specific handler to the first configured
target — used for provider endpoints himadri doesn't model explicitly.

## Crate map

| Crate | Responsibility |
|---|---|
| `himadri` | Binary: `main.rs` (startup wiring: `build_gateway` / `wire_plugins` / `build_admin` / `build_router`), `handlers.rs` (HTTP handlers), `gateway/` (`Gateway` orchestrator, split into route/stream/policy/audit/config/rebuild/providers/proxy modules), `strategy.rs` (routing) |
| `himadri-core` | Shared types (`Config`, `Target`, `ModelObject`, errors) used across crates |
| `himadri-provider` | `Provider` trait, the config-driven `OpenAiCompatibleProvider` (with presets for OpenAI, Azure, OpenRouter, Groq, …), bespoke Anthropic/Gemini impls, shared SSE decoder (`sse.rs`) |
| `himadri-plugin` | `Plugin` / `ResponseGuardrail` traits, `PluginType`/`Stage`/`PluginError` |
| `himadri-plugins` | Concrete plugins: budget, cache, logger, max_token, pii_guardrail (+ `pii_engine`, feature `guardrails`), rate_limit, word_filter |
| `himadri-admin` | Key/provider/model CRUD, usage & request-log stores (in-memory/SQLite/Postgres), auth middleware, embedded DB migrations |
| `himadri-auth` | JWT/OIDC/OAuth2 primitives (Zitadel-oriented; see `docs/zitadel.md`) |
| `himadri-ratelimit` | Rate-limiting primitives (used by the `rate_limit` plugin) |
| `himadri-circuitbreaker` | Per-target circuit breaking so routing skips unhealthy targets |
| `himadri-observability` | Metrics (Prometheus) and tracing (OpenTelemetry) wiring |
| `load-test-sink` | Standalone helper binary for load testing |

## State & persistence

`Gateway` holds most runtime state in memory (`Arc<...>` fields set up in
`main.rs`): registered providers, targets/routing config, rate limiter,
circuit breakers, audit log, metrics. Config can be reloaded and rolled back
live via `/admin/*` (`reload_config`, `rollback_config`, `config_history`),
which is how the admin dashboard pushes routing/provider changes without a
restart.

Durable state (API keys, providers/models, usage, request logs) is delegated
to `himadri-admin`'s store abstraction, backed by one of three interchangeable
implementations selected via `DATABASE_URL`:

- **In-memory** (default, no `DATABASE_URL`) — volatile, lost on restart.
- **SQLite** (default build feature) — durable keys/providers/models; request
  logs still in-memory.
- **Postgres** (`--features postgres`) — durable keys, providers/models, and
  request logs (see `docs/database.md`).

For multi-replica deployments, the `redis` feature moves rate-limit and
circuit-breaker state into Redis so it's shared across instances.

## Admin dashboard (`web/`)

A Next.js 16 / React 19 app that drives the gateway entirely through the
`/admin/*` API (key/provider/model/config CRUD, usage and request-log views).
It has no direct access to gateway internals or the database — everything
goes through HTTP, same as any other admin API client.

## Observability

`himadri-observability` wires up:

- **Metrics** — exposed at `GET /metrics` in Prometheus format.
- **Tracing** — structured logs via `tracing`/`tracing-subscriber`, with
  optional OTLP export via `opentelemetry`/`tracing-opentelemetry`.

## Deployment

- `Dockerfile` — multi-stage build (`rust:1.75-slim` builder →
  `debian:bookworm-slim` runtime), producing a single `himadri` binary.
- `docker-compose.yml` — local single-container setup (see
  [`DEVELOPMENT.md`](DEVELOPMENT.md)).
- `deploy/ecs` — AWS ECS deployment assets for production.
</content>
