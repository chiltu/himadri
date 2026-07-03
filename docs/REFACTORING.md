# Refactoring Backlog — Code-Size Reduction

Findings from an architectural review (2026-07-02) of the ~23.4k-line Rust
workspace, ranked by payoff. Roughly 4,000–5,000 lines can be removed by
finishing abstractions that already exist in the codebase, deleting dead code,
and extracting repeated pipelines.

Status legend: ☐ open · ◐ in progress · ☑ done

## ☑ 1. Delete the copy-pasted OpenAI-compatible providers (~1,100 lines)

`himadri-provider/src/compatible/provider.rs` is a config-driven
`OpenAiCompatibleProvider` written specifically to replace the per-vendor
OpenAI-shaped providers — yet the copies remained:

- `openai/provider.rs` and `openrouter/provider.rs` were byte-for-byte
  identical (373 lines each) except for name, base URL, model list, and two
  extra headers.
- `azure/provider.rs` (391 lines) differed only in auth header + URL path,
  both already supported by `OpenAiCompatibleConfig`.

**Fix:** replace the three structs with three `OpenAiCompatibleConfig`
factory functions. Future OpenAI-compatible vendors (Groq, Together,
Fireworks, …) become config one-liners.

## ☑ 2. Extract a shared SSE decoder (~250 lines)

All provider implementations hand-rolled the identical stream loop:
byte-buffer accumulation, newline splitting, `data: [DONE]` detection,
`data: ` prefix stripping, JSON parse, error wrapping.

**Fix:** `sse::sse_events(byte_stream)` decodes bytes into
`SseEvent { event, data }` items (line buffering, `event:` tracking,
`[DONE]` termination, transport-error mapping); each provider keeps only its
chunk-translation closure via `map` / `filter_map` / `flat_map`. The
repeated `handle_error` bodies moved to `ProviderError::from_openai_response`
and the parameterized `ProviderError::from_response` in `error.rs`
(providers differ only in auth status codes and message extraction).

## ☑ 3. Delete dead `postgres_store.rs` (374 lines)

`himadri-admin/src/postgres_store.rs` defined a second `PostgresStore` that
was never declared in `lib.rs`'s module tree — the live one lives inside
`store.rs`. It compiled into nothing.

## ◐ 4. Unify the SQLite/Postgres store pairs

Two parallel-implementation pairs:

- `store.rs`: `PostgresStore` + `SqliteStore` (API keys).
- `provider_store.rs` vs `postgres_provider_store.rs` — the same CRUD twice.

**Findings from analysis (2026-07-02):**

- The "Postgres provider/model stores not wired" gap previously documented in
  `docs/database.md` was already fixed (`provider_backend.rs` selects the
  backend by scheme); the doc was stale and has been corrected.
- sqlx's `Any` driver is ruled out: it doesn't support chrono/uuid/serde_json,
  which these rows use. Full unification means a generic
  `Store<DB: sqlx::Database>` + per-backend trait (or a macro), plus a
  Postgres migration converting `UUID` ids to `TEXT`. Placeholders are not a
  real divergence (SQLite accepts `$1`), nor are timestamps if bound from
  Rust on both sides.

**Done (the cheap, high-value parts):**

- Rewrote `PostgresStore::update`'s 90-line dynamic-`SET` builder in the
  fetch-merge-update style every other store method already uses.
- Fixed a parity bug found in the process: `SqliteStore::update` silently
  dropped `expires_at`, `models`, `rate_limit_override` and `token_budget`
  (Postgres persisted them). Covered by a new SQLite test and a new gated
  Postgres parity test (`postgres_api_key_update_parity`), both verified —
  the Postgres ones against a real Postgres 16 container.
- Fixed `connect_provider_model_stores` clobbering an existing query string
  when appending `?mode=rwc` to SQLite URLs.

**Remaining (optional):** the generic/macro unification of the two pairs
(~600 lines). Given these are stable CRUD methods, the drift risk is low;
revisit if a third backend or new store methods appear. Run the gated
Postgres tests via `TEST_POSTGRES_URL=postgres://... cargo test --features
postgres postgres` against a throwaway container before touching Postgres SQL.

## ☑ 5. Deduplicate admin CRUD handlers in `main.rs`

The 12 provider/model handlers all repeated: call admin → re-fetch providers
+ models → `rebuild_targets_from_db`.

**Done:** `AppState::rebuild_targets` plus three response helpers
(`created` / `updated` / `deleted`) encapsulate the mutate-then-rebuild
pattern; each handler is now 3–6 lines and the rebuild can no longer be
forgotten on a new mutation endpoint.

**Also done:** deleted the old `crates/himadri/src/handlers.rs` (303 lines) —
a dead parallel implementation of the HTTP handlers (`Routes` struct)
exported from the lib but referenced by nothing; the live handlers were the
ones in `main.rs`. The stale copy shows why two handler homes invite drift.

**Also done:** the live handlers now have their own module (a fresh
`handlers.rs`, bin-only, the single home this time), and `main()` is ~50
lines of orchestration over `build_gateway` / `register_providers_from_env` /
`wire_plugins` / `build_admin` / `init_jwt_discovery` / `build_router`.

## ☑ 6. Share the pre-flight/failover pipeline in `gateway.rs`

`route` and `route_stream` duplicated the four `check_*` guards →
before-plugins → strategy selection → RBAC filter sequence, and
`route_stream` re-implemented the circuit-breaker failover loop that `route`
got from `execute_with_fallback`. Drift risk was a security concern: a guard
added to one path and forgotten in the other.

**Fix:** `prepare_request` (guards + before-plugins) and `select_targets`
(strategy + RBAC filter) are shared by both paths; `with_failover<T>` is the
single failover loop, generic over the per-target operation (`complete` vs
`complete_stream`). An internal `AttemptError` enum keeps infrastructure
failures distinct from provider errors so each caller preserves its exact
error surface. Streaming intentionally passes `record_latency: false`
(time-to-open-stream is not comparable with completion latency for the
least-latency strategy), and the breaker still records success at
stream-open, not stream-end.

## ☑ 8. Streaming requests bypass usage recording and metrics (gap, not a refactor)

`route` recorded usage (tokens, cost), Prometheus metrics, and request logs;
`route_stream` emitted only an audit event with zeroed token counts, so all
streamed traffic was invisible to billing/usage dashboards and `/metrics`.

**Fix (three parts):**

1. `StreamUsageRecorder` in `gateway.rs` mirrors `route`'s recording
   (requests/tokens/cost/duration metrics, `UsageStore` record, request-log
   entry). It takes usage from the last chunk that carries it and fires once
   at stream end — or via `Drop`, so client disconnects mid-stream still get
   recorded (as failures when an error was observed).
2. `GuardrailStream` feeds the recorder as chunks pass through.
3. `OpenAiCompatibleProvider` now sends
   `stream_options: {"include_usage": true}` on streaming requests, since
   OpenAI-style APIs otherwise send no usage at all when streaming.
   (Anthropic already reports usage via `message_delta`.) Note: an
   OpenAI-compatible vendor that rejects unknown request fields would now
   fail streaming requests — none of the built-in presets are known to.

## ☑ 7. Generic keyed-store registry in plugins

`budget.rs` and `rate_limit.rs` each implemented the same global registry of
named stores (`get_or_create_store`, `reset_store_key`, `reset_store`,
getters) around different value types.

**Fix:** `StoreRegistry<T>` in `himadri-plugin` (`registry.rs`) with
`get_or_create` (double-checked locking) and `with` (run a closure against a
named store). Both plugins' registries and admin helper functions now
delegate to it; future stateful plugins get the pattern for free.

## Status

Items 1–3 and 6–8 are done; 4 and 5 are partially done with the remaining
work explicitly optional (see each item). Net effect of the completed work:
roughly −1,600 lines against the starting ~23.4k, with test count up from
250 to 262.
