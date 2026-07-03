# Database Configuration

himadri persists API keys, providers/models, usage and request logs in a
pluggable store selected at runtime by the `DATABASE_URL` environment variable.

Three backends exist:

| Backend | Selected when | Cargo feature | Default |
|---|---|---|---|
| **In-memory** | `DATABASE_URL` unset (or connection fails) | _(always)_ | ✅ when no URL |
| **SQLite** | `DATABASE_URL=sqlite://...` | `sqlite` | ✅ built by default |
| **Postgres** | `DATABASE_URL=postgres://...` | `postgres` | requires opt-in build |

The store backend is chosen by the **scheme prefix** of `DATABASE_URL`
(`sqlite` vs `postgres`). If the configured backend fails to connect, the
gateway logs a warning and **falls back to the in-memory store** rather than
refusing to start.

- [In-memory (default with no URL)](#in-memory-default-with-no-url)
- [SQLite (default build)](#sqlite-default-build)
- [Postgres](#postgres)
- [What each backend persists](#what-each-backend-persists)
- [Migrations](#migrations)
- [Choosing a backend](#choosing-a-backend)

---

## In-memory (default with no URL)

If `DATABASE_URL` is unset, the gateway uses an in-memory store:

```
INFO Using in-memory store (set DATABASE_URL for Postgres/SQLite)
```

- **Everything is lost on restart** — API keys, usage counters, request logs.
- No dynamic providers/models (the admin provider/model stores are not wired).
- Fine for local development and tests; **not** for anything you care about.

---

## SQLite (default build)

SQLite is the default persistence backend. The binary is built with the
`sqlite` feature out of the box (`default = ["sqlite"]`), so no special build
flags are needed.

```bash
export DATABASE_URL=sqlite://himadri.db
cargo run -p himadri
```

Behavior:

- The database **file is created if missing** — the gateway appends
  `?mode=rwc` to the connection string automatically (read-write-create).
- **Embedded migrations run automatically** on startup; only pending versions
  are applied (idempotent across restarts).
- Enables the **API-key store**, **provider store** and **model store** — i.e.
  the admin API and DB-backed `GET /v1/models` work.

Connection string forms:

```bash
DATABASE_URL=sqlite://himadri.db          # relative file
DATABASE_URL=sqlite:///var/lib/himadri.db # absolute path (note the triple slash)
DATABASE_URL=sqlite://:memory:            # ephemeral, per-process
```

> **Request logs on SQLite:** persistent request-log storage is currently wired
> for **Postgres only**. On SQLite (and in-memory) request logs are kept in
> memory and are lost on restart, even though keys/providers/models persist.

---

## Postgres

Postgres is **not** in the default build. You must compile with the `postgres`
feature:

```bash
cargo build -p himadri --release --features postgres
# or, to run:
cargo run -p himadri --features postgres
```

Then point at your database:

```bash
export DATABASE_URL=postgres://user:password@localhost:5432/himadri
cargo run -p himadri --features postgres
```

Behavior:

- Connects via a connection pool and **runs embedded migrations** on startup
  (version-tracked, only pending migrations applied).
- Enables the **API-key store**, the **provider and model stores** (admin
  CRUD and DB-backed `GET /v1/models`, same as SQLite), and the **persistent
  request-log store** — request logs survive restarts.
- The database/role must already exist and be reachable; unlike SQLite, Postgres
  does **not** create the database for you.

Recommended pool/SSL tuning is done via the `DATABASE_URL` query string per
`sqlx` conventions, e.g.:

```bash
DATABASE_URL="postgres://user:pass@db.internal:5432/himadri?sslmode=require"
```

---

## What each backend persists

| Data | In-memory | SQLite | Postgres |
|---|---|---|---|
| API keys | ✅ (volatile) | ✅ | ✅ |
| Usage counters | ✅ (volatile) | ✅ | ✅ |
| Providers / models (dynamic) | ❌ | ✅ | ✅ |
| Request logs | in-memory | in-memory | ✅ persistent |

---

## Migrations

Migrations are **embedded in the binary** and run automatically at startup — you
do not run a separate migration step. They are version-tracked, so restarting
re-applies nothing.

For an explicit, fail-hard migration step, start the binary with `--migrate`:
it brings the database at `DATABASE_URL` to the latest schema version before
the server starts and exits non-zero on any failure. This differs from the
automatic connect-time migrations, which log errors and fall back to in-memory
stores rather than aborting startup:

```bash
DATABASE_URL=sqlite://himadri.db ./himadri --migrate
```

Source locations (for reference / review):

```
crates/himadri-admin/migrations/sqlite/001_initial.sql
crates/himadri-admin/migrations/sqlite/002_providers_models.sql
crates/himadri-admin/migrations/postgres/001_initial.sql
crates/himadri-admin/migrations/postgres/002_providers_models.sql
```

To apply a schema change, add a new numbered `.sql` file under the appropriate
backend directory; it will be picked up on the next startup.

---

## Choosing a backend

| Use case | Recommendation |
|---|---|
| Local dev, tests, throwaway | In-memory (no `DATABASE_URL`) |
| Single-node deployment, simple ops, dynamic models | **SQLite** |
| Multi-replica, durable request logs, shared state | **Postgres** (`--features postgres`) |

For multi-replica deployments also consider the `redis` feature, which moves
rate-limit and circuit-breaker state into Redis so it is shared across
instances (built with `--features redis`).
