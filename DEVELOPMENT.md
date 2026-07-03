# Development Guide

Getting a local **himadri** environment running — gateway API, admin
dashboard, and (optionally) a real database.

## Prerequisites

- Docker + Docker Compose (preferred path, see below), **or**
- Rust (stable, 1.75+) and Cargo, for building/running the gateway manually
- Node.js 20+ and npm, if you're working on the admin dashboard (`web/`)

## Option A: Docker Compose (preferred)

The fastest way to get a working gateway is `docker compose`. It builds the
image from the repo `Dockerfile` and runs the binary with sane defaults.

```bash
# from the repo root
export OPENAI_API_KEY=sk-...        # any provider keys you have
export ANTHROPIC_API_KEY=sk-ant-...
export GEMINI_API_KEY=...

docker compose up --build
```

This starts `himadri` on `http://localhost:8080` with:

- `RUST_LOG=himadri=info,tower_http=info`
- A health check against `GET /health`
- Provider keys passed through from your shell environment (unset ones are
  passed through empty — that provider is simply not registered)

Verify it's up:

```bash
curl http://localhost:8080/health
curl http://localhost:8080/v1/models
```

Notes on the default compose setup:

- No `DATABASE_URL` is set, so the gateway uses the **in-memory store** —
  fine for trying things out, but API keys/usage/providers reset on restart.
- No `MASTER_KEY` is set, so **admin auth is disabled** (anonymous admin).
  This is intentional for local development only — never run this way in
  production.
- To persist data across restarts, add a `DATABASE_URL` to the `environment:`
  block in `docker-compose.yml` (e.g. `sqlite:///data/himadri.db` with a
  mounted volume) — see [`docs/database.md`](docs/database.md).

Stop with `docker compose down` (add `-v` only if you also want to drop any
volumes you've added).

## Option B: Build and run manually with Cargo

Useful when iterating on Rust code, since it gives you incremental
compilation and direct access to `cargo test` / `cargo clippy`.

```bash
# Minimal: proxy OpenAI only, no auth, in-memory store
export OPENAI_API_KEY=sk-...
cargo run -p himadri

# Production-shaped: persistence + admin auth
export DATABASE_URL=sqlite://himadri.db
export MASTER_KEY=$(openssl rand -hex 32)
export OPENAI_API_KEY=sk-...
cargo run -p himadri --release
```

The server listens on `0.0.0.0:$PORT` (default `8080`). The binary also
accepts `--port <PORT>` (overrides the `PORT` env var), `--migrate` (migrate
the database at `DATABASE_URL` to the latest schema version before starting,
exiting non-zero on failure), and `--help`.

Common commands:

```bash
cargo build                    # build the workspace
cargo test                     # run all tests
cargo clippy --all-targets     # lint
cargo fmt                      # format
cargo run -p himadri           # run the gateway binary
cargo run -p himadri --features postgres  # run with Postgres support compiled in
```

Full environment variable and JSON config reference lives in
[`docs/configuration.md`](docs/configuration.md); database backend details
(in-memory vs SQLite vs Postgres) are in [`docs/database.md`](docs/database.md).

## Admin dashboard (`web/`)

The admin UI is a separate Next.js app in `web/` and talks to the gateway's
`/admin/*` API. It is not started by `docker compose up` — run it directly:

```bash
cd web
npm install
npm run dev          # http://localhost:3000, proxies to the gateway API
```

Other useful scripts: `npm run build`, `npm run lint`, `npm run typecheck`,
`npm run format`. See [`web/AGENTS.md`](web/AGENTS.md) before making frontend
changes — this is a Next.js 16 / React 19 app with breaking changes from
older versions.

## Running the two together

1. `docker compose up --build` (or `cargo run -p himadri`) to get the gateway
   on `:8080`.
2. `cd web && npm run dev` to get the dashboard on `:3000`.
3. Point the dashboard at the gateway (see `web/` config for the API base
   URL) and log in with your `MASTER_KEY` if auth is enabled.

## Tests

```bash
cargo test                          # unit + integration tests across the workspace
cargo test -p himadri usecase_e2e   # gateway end-to-end usecase tests
```

Postgres parity tests are skipped unless `TEST_POSTGRES_URL` is set. To run
them against a throwaway container:

```bash
docker run -d --rm --name himadri-pg-test \
  -e POSTGRES_PASSWORD=test -e POSTGRES_DB=himadri_test \
  -p 5432:5432 postgres:16-alpine

TEST_POSTGRES_URL="postgres://postgres:test@localhost:5432/himadri_test" \
  cargo test -p himadri --features postgres --test usecase_e2e_tests postgres

docker stop himadri-pg-test
```

Run these whenever you touch Postgres-specific SQL — the regular suite never
exercises it. See [`docs/E2E.md`](docs/E2E.md) for end-to-end testing notes.

## Where to go next

- [`docs/README.md`](docs/README.md) — documentation index
- [`docs/configuration.md`](docs/configuration.md) — full env-var / JSON config reference
- [`docs/database.md`](docs/database.md) — persistence backends
- [`docs/zitadel.md`](docs/zitadel.md) — OIDC/JWT setup
- [`ARCHITECTURE.md`](ARCHITECTURE.md) — how the pieces fit together
- [`AGENTS.md`](AGENTS.md) — repository layout, for contributors and coding agents
</content>
