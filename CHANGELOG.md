# Changelog

## 2026-07-03 — DB-registered provider fixes, `--migrate` CLI option

### Added

- **Command-line options** (hand-parsed; no new dependency):
  `--migrate` runs the embedded database migrations (SQLite or Postgres,
  selected by `DATABASE_URL`'s scheme) to the latest version just before
  startup and exits non-zero on failure — unlike the store connect paths,
  which log and fall back. Backed by a new public
  `himadri_admin::migrate_to_latest(database_url)`. Also added
  `--port <PORT>` (overrides the `PORT` env var; `tests/e2e_test.sh` already
  passed this flag, but the binary silently ignored it) and `--help`.

### Fixed

- **DB-registered providers were inactive after a restart.** Routing targets
  were only rebuilt from the provider/model stores on a mutation
  (create/update/delete/toggle), so providers registered via `/admin/providers`
  stayed unroutable after a restart until something was toggled. Startup now
  syncs once after the stores connect: if the DB contains providers, targets
  are rebuilt from them (a DB with no providers leaves env/file-configured
  targets untouched). Verified by restarting against a populated SQLite DB and
  completing a live OpenRouter chat request with no prior admin mutation.

- **DB-registered providers always failed upstream auth.** Providers created
  through `/admin/providers` (the `PROVIDER_ENCRYPTION_KEY` at-rest-encryption
  path) never had their API key attached to outbound requests:
  `rebuild_targets_from_db` built targets with `api_key_env: None`, and
  `Gateway::get_api_key` only knew how to read env vars, so every request to a
  DB-registered provider went out with an empty `Authorization` header and was
  rejected upstream (e.g. OpenRouter: `Missing Authentication header`). The
  gateway now keeps a `provider_keys` map (provider name → decrypted key),
  populated on every `rebuild_targets_from_db`, and `get_api_key` falls back to
  it when a target has no `api_key_env`. The key is deliberately kept off the
  serializable `Target` struct so it cannot leak into `/admin/config` responses
  or config history. Verified live end-to-end against OpenRouter: provider +
  model registered via the admin API with the key stored as `enc:v1:…` in
  SQLite, then non-streaming and streaming chat completions routed through the
  gateway using only the DB-decrypted key (the `OPENROUTER_API_KEY` env var was
  set to a dummy value to prove the DB key was used), with usage and request
  logs recorded.

## 2026-07-02 — Architectural cleanup, streaming usage accounting, docs

An architectural review focused on reducing code size (findings and status in
[`docs/REFACTORING.md`](docs/REFACTORING.md)), followed by staged refactors,
several bug fixes found along the way, and a documentation pass. Net effect:
roughly −1,700 lines, workspace test suite grown from 250 to 262 tests
(0 failures), clippy clean throughout.

### Refactoring

- **Deleted three copy-pasted providers** (`openai/`, `openrouter/`,
  `azure/`, ~1,100 lines): all three were unreferenced duplicates of the
  config-driven `OpenAiCompatibleProvider`, which the runtime already used
  via its `::openai()` / `::openrouter()` / `::azure()` presets.
- **Shared SSE decoder** (`himadri-provider/src/sse.rs`): all providers
  hand-rolled the same stream loop (line buffering across chunk boundaries,
  `event:` tracking, `data: [DONE]`, error mapping). Providers now keep only
  their chunk-translation closures. Repeated `handle_error` bodies moved to
  `ProviderError::from_openai_response` / `from_response` in `error.rs`.
- **Deleted dead code:** `himadri-admin/src/postgres_store.rs` (374 lines,
  a second `PostgresStore` never declared in the module tree) and the old
  `himadri/src/handlers.rs` (303 lines, a drifted parallel `Routes`
  implementation of the HTTP handlers referenced by nothing).
- **Unified the gateway request pipeline** (`gateway.rs`): `route` and
  `route_stream` now share `prepare_request` (guards + before-plugins),
  `select_targets` (strategy + RBAC filter), and a generic `with_failover`
  loop — a guard added to one path can no longer be forgotten in the other.
  An internal `AttemptError` enum preserves each path's exact error surface.
- **Deduplicated admin CRUD handlers:** `AppState::rebuild_targets` plus
  `created` / `updated` / `deleted` helpers encapsulate the
  mutate-then-rebuild pattern; the 12 provider/model handlers are 3–6 lines
  each.
- **Generic `StoreRegistry<T>`** in `himadri-plugin`: replaces the identical
  global named-store registries hand-rolled by the budget and rate-limit
  plugins; admin helper signatures unchanged.
- **Rewrote `PostgresStore::update`** (90-line dynamic `SET` builder) in the
  fetch-merge-update style every other store method uses.
- **Split `main.rs`:** HTTP handlers moved to a new bin-only `handlers.rs`
  (single home); `main()` is now ~50 lines over `build_gateway` /
  `register_providers_from_env` / `wire_plugins` / `build_admin` /
  `init_jwt_discovery` / `build_router`. Verified by booting the server and
  exercising `/health`, `/v1/models`, and `/admin/*` auth.

### Security

- **Broken access control on `/admin/*` (privilege escalation), fixed.**
  The admin routes were protected only by *authentication* — any valid bearer
  token (including a plain non-admin API key) passed, because neither the auth
  middleware nor the handlers checked `AuthScope::Admin`. A regular API key
  could therefore read decrypted upstream provider secrets
  (`GET /admin/providers`), mint admin-scoped keys, revoke keys, rewrite
  routing config, and wipe logs. Added a `require_admin_scope` middleware layer
  on the admin router that runs after authentication and rejects any
  non-`Admin` principal with 403. Dev-bypass mode (no `MASTER_KEY`) is
  unaffected (its anonymous context is `Admin`). Covered by new tests
  (`admin_scope_tests`). This was a pre-existing issue, not introduced by the
  refactors above.
- **Raw API key no longer logged (CWE-532).** The required-roles denial path
  in `combined_auth.rs` logged `ctx.api_key` — the raw bearer secret for an
  API-key principal — at WARN. It now logs the non-secret `key_id` / `user_id`.
- **Passthrough request body is now bounded (CWE-770).** The `/v1/*`
  passthrough buffered the body with `to_bytes(.., usize::MAX)`, allowing an
  authenticated caller to exhaust memory. Capped at 10 MiB; oversized requests
  get `413 Payload Too Large` instead of being silently truncated to empty.
- **SSRF guard on provider base URLs (A10).** Provider `base_url`s set via the
  admin API were forwarded to verbatim, so one could point at loopback / RFC
  1918 / link-local hosts or the cloud metadata endpoint. `himadri-core`'s new
  `net_guard` rejects non-`http(s)` schemes and internal IP literals /
  metadata hostnames at provider create/update time; self-hosted private
  backends can opt in with `ALLOW_PRIVATE_PROVIDER_URLS=1`. (Hostnames that
  resolve to internal IPs at request time are a documented residual limit.)

### Fixed

- **Streamed requests were invisible to billing and metrics:** `route_stream`
  recorded no usage, cost, request logs, or Prometheus metrics. A
  `StreamUsageRecorder` now mirrors `route`'s accounting, capturing usage
  from the final stream chunk and recording at stream end — or via `Drop` on
  client disconnect (marked failed when a mid-stream error was observed).
  `OpenAiCompatibleProvider` now sends `stream_options: {"include_usage":
  true}` on streaming requests, since OpenAI-style APIs send no usage
  otherwise (Anthropic already reports it via `message_delta`).
- **SQLite `update` silently dropped fields:** `expires_at`, `models`,
  `rate_limit_override`, and `token_budget` were not persisted on API-key
  update in the default SQLite build (Postgres kept them). Both backends now
  update all fields; covered by a new SQLite test and a new gated
  `postgres_api_key_update_parity` test, verified against a real Postgres 16
  container.
- **SQLite URL clobbering:** `connect_provider_model_stores` appended
  `?mode=rwc` unconditionally, corrupting `DATABASE_URL`s that already carry
  a query string.
- **Postgres request-log filters were never applied.** `list()` / `delete()`
  in `postgres_backends.rs` built `$N` placeholders but never `.bind()`-ed the
  values, so any filtered request-log query errored at runtime. The filter
  values are now bound (in placeholder order); verified against a real
  Postgres 16 instance by a new gated test.

### Documentation

- New root **`README.md`** (the repo previously had none), **`ARCHITECTURE.md`**
  (system overview, request lifecycle, crate map), **`DEVELOPMENT.md`**
  (Docker Compose preferred + manual Cargo setup, web dashboard, gated
  Postgres test workflow), and **`docs/REFACTORING.md`** (review findings and
  status).
- Corrected a stale claim in `docs/database.md`: dynamic provider/model
  stores **are** wired on Postgres (the gap it described was already fixed).
- `AGENTS.md` and `docs/README.md` updated for the new layout.

## Tests — use-case-driven e2e suite

Added `crates/himadri/tests/usecase_e2e_tests.rs` — 23 end-to-end tests across three groups.

### Group A — Gateway-driven (RBAC, budgets, failover, cache)
- `rbac_denies_model_not_in_role_policy`
- `rbac_allows_model_in_role_policy`
- `rbac_admin_scope_bypasses_restrictions`
- `rbac_default_role_applies_when_no_role_matches`
- `budget_blocks_after_limit_exceeded`
- `budget_tracks_keys_independently`
- `fallback_strategy_retries_next_provider_on_failure`
- `response_cache_avoids_duplicate_provider_call`
- `embeddings_unsupported_provider_returns_error`

### Group B — Admin HTTP API (real SQLite file)
- `provider_full_crud_lifecycle`
- `provider_delete_blocked_when_models_exist`
- `provider_disable_blocked_when_enabled_models_exist`
- `model_create_fails_for_disabled_provider`
- `model_full_crud_lifecycle`
- `provider_encryption_at_rest_transparent`
- `provider_created_at_is_real_timestamp_not_epoch`
- `api_key_created_at_is_real_timestamp_not_epoch`
- `api_key_full_lifecycle_via_admin_handlers`
- `dashboard_key_count_reflects_created_keys`
- `config_get_update_roundtrip`
- `config_history_and_rollback`

### Group C — Postgres parity (skipped unless `TEST_POSTGRES_URL` is set)
- `postgres_provider_crud_parity`
- `postgres_encryption_at_rest_transparent`

All 23 pass; verified against both SQLite and a live Postgres instance, alongside the full existing workspace test suite (261 tests, 0 failures).
