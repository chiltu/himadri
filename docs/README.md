# himadri Documentation

**himadri** is an OpenAI-compatible AI gateway in Rust that proxies many LLM
providers, adding authentication (API keys and JWT/OIDC), routing strategies
with failover, rate limiting, spend budgets, RBAC, plugins/guardrails,
circuit breaking, response caching, and observability. See the
[root README](../README.md) for the full feature overview.

## Guides

| Doc | What's inside |
|---|---|
| [Configuration guide](./configuration.md) | Complete env-var reference, JSON config schema, providers, routing strategies, rate limiting, caching, orgs/teams/guardrails, CORS. |
| [Database configuration](./database.md) | In-memory (default), SQLite (default build), and Postgres backends; what each persists; migrations. |
| [Zitadel configuration & FAQ](./zitadel.md) | OIDC/JWT setup with Zitadel, role-claim mapping, user onboarding script, troubleshooting FAQ. |
| [End-to-end testing](./E2E.md) | E2E testing notes, including the real-Zitadel OIDC stack. |
| [Code review checklist](../CODE_REVIEW.md) | Active source-review checklist (2026-07-05): correctness fixes, dead-code deletions, and dedup refactors, worked one by one. Replaces the earlier refactoring backlog. |
| [Gap analysis](./GAP_ANALYSIS.md) | Historical feature-parity analysis vs. Bifrost (see the status note at its top — most gaps have since been closed). |

See also, at the repository root: [ARCHITECTURE.md](../ARCHITECTURE.md)
(system overview, request lifecycle, crate map) and
[DEVELOPMENT.md](../DEVELOPMENT.md) (getting a dev environment running).

## At a glance

```bash
# Development (no auth, in-memory) — boots with zero config
export OPENAI_API_KEY=sk-...
cargo run -p himadri

# Production-shaped
export DATABASE_URL=sqlite://himadri.db      # or postgres:// with --features postgres
export MASTER_KEY=$(openssl rand -hex 32)    # without this, auth is bypassed
export OPENAI_API_KEY=sk-...
export JWT_ISSUER=https://your-instance.zitadel.cloud   # optional: enable OIDC
cargo run -p himadri --release
```

## Key defaults to remember

- **No `MASTER_KEY` / `JWT_ISSUER` → authentication is disabled** (anonymous
  Admin). Dev only.
- **No `DATABASE_URL` → in-memory store**, lost on restart.
- **SQLite is the default build**; **Postgres requires `--features postgres`**.
- **Config file is JSON only** (set via `GATEWAY_CONFIG`).
- **CLI flags:** `--migrate` (migrate the DB to the latest schema before
  starting, fail-hard), `--port <PORT>` (overrides `PORT`), `--help`.

See the [project AGENTS.md](../AGENTS.md) for repository layout and build/test
commands.
