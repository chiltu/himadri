# himadri

An OpenAI-compatible AI gateway in Rust. One `/v1/chat/completions` endpoint
in front of many LLM providers, adding authentication, routing strategies
with failover, rate limiting, budgets, plugins/guardrails, circuit breaking,
caching, and observability. A Next.js admin dashboard lives in [`web/`](web/).

## Features

- **OpenAI-compatible API** — `/v1/chat/completions`, `/v1/completions`,
  `/v1/embeddings`, `/v1/models`, plus a transparent `/v1/*` passthrough.
- **Many providers** — OpenAI, Anthropic, Gemini, Azure OpenAI, AWS Bedrock,
  OpenRouter, Together, Groq, Fireworks, DeepInfra, Cerebras, Novita, and any
  other OpenAI-compatible endpoint. Enabled by setting their API keys.
- **Routing strategies with failover** — priority, weighted, round-robin,
  least-latency; unhealthy targets are skipped via per-provider circuit
  breakers.
- **Auth** — API keys (admin-issued), a master admin key, and optional
  JWT/OIDC (e.g. [Zitadel](docs/zitadel.md)) side by side.
- **Controls** — rate limiting (per key / user / IP), spend budgets,
  token caps, word filters, response guardrails, orgs/teams RBAC.
- **Persistence** — in-memory (default), SQLite (default build), or Postgres
  (`--features postgres`); see [docs/database.md](docs/database.md).
- **Observability** — Prometheus `/metrics`, structured tracing with optional
  OTLP export, request logs, audit log, usage/cost accounting (streaming
  included).

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

By default (no `MASTER_KEY`, no `DATABASE_URL`) the gateway runs with auth
bypassed and in-memory storage — development only. See the
[configuration guide](docs/configuration.md) for the production-shaped setup.

## Admin dashboard

```bash
cd web && npm install && npm run dev   # http://localhost:3000
```

The dashboard drives the gateway entirely through the `/admin/*` API
(keys, providers, models, routing config, usage, logs).

## Documentation

- [DEVELOPMENT.md](DEVELOPMENT.md) — getting a dev environment running,
  build/test commands
- [ARCHITECTURE.md](ARCHITECTURE.md) — system overview, request lifecycle,
  crate map
- [docs/](docs/README.md) — configuration, database backends, Zitadel/OIDC,
  refactoring backlog
- [AGENTS.md](AGENTS.md) — repository layout notes for contributors and
  coding agents
