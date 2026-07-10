# Code Review — Source Optimization & Maintainability (2026-07-05)

Full crate-by-crate review of the workspace (~23.5k lines of Rust). Goal:
reduce line count through **source optimization only** (dead-code deletion,
deduplication, idiomatic restructuring — no new dependencies) while improving
maintainability. Replaces `docs/REFACTORING.md` (2026-07-02 review; all its
items were done except §4, carried forward here as R33).

Working through the list item by item, top to bottom, is the intended use.
Status legend: ☐ open · ◐ in progress · ☑ done

## Status: 36/37 complete (2026-07-06)

All items done except **R33** (◐ — the SQLite/Postgres store-pair
unification), which the plan already marked *optional*: it's revisit-if a
third backend or new store methods appear, and the R16 dispatch macro
(done) removed the day-to-day drift pain that was its main cost. Left as a
deliberate deferral.

**Note (2026-07-10):** A high-effort code review of the current working-tree changes identified 10 findings (6 CONFIRMED, 4 PLAUSIBLE) including 2 critical issues (API key leak, credential destruction) and several availability bugs. See `docs/CODE_REVIEW_2026-07-10.md` for full details and recommended fix order.

A few items were resolved by a broader decision than the plan's first
option, noted here so the choices are visible:

- **R11** (Bedrock) — deleted (the plan's recommended option), which also
  made **R18**'s Bedrock builder and **R20**'s tool-choice sharing moot.
- **R12** — deleted the two config stores and `PostgresUsageStore`; this also
  resolved **R6** (the SQLite-only `config_store.rs` under a postgres-only
  build).
- **R17** — the type-reuse (part 2) and moot part 3 are done; the
  column-list-const (part 1) was **deferred**: the inline SQL literals can't
  reference a const without wrapping every query in `format!`, which adds
  allocation and an injection surface for negligible benefit on a stable
  schema.
- **R21** — the durable win (Default derives + `Message::user`) plus the
  concentrated test-helper literals are done; the remaining verbose literals
  in the large e2e files are left as-is (future additions use
  `..Default::default()`).
- **R28** — Default + run-loop unify done; the thin `PluginContext` setters
  were kept intentionally (they read better at call sites than raw field
  assignment).
- **R32** — implemented as non-breaking error *logging* (swallowed store
  errors now surface in logs with their reason) rather than threading
  `Result` through the whole admin API surface, which would have cascaded
  into every e2e test's hand-written handler wrappers.
- **R35** — pricing is now overridable via the `PRICING_TABLE` env var
  (defaults preserved); the model-list-as-data parts were already satisfied
  by R22's preset helper and `supported_models` being display-only.

Result: **~295 tests green, warning-free `cargo build --workspace
--all-targets`**, and the postgres-only build compiles (R6). Verification
baseline below.

```bash
cargo test --workspace                 # all green
cargo build --workspace --all-targets  # no warnings
cargo check -p himadri --no-default-features --features postgres  # fixed (was R6)
```

Per-crate index: himadri-core → R21, R26 · himadri-plugin → R28 · 
himadri-provider → R1, R7, R9, R10, R11, R18, R19, R20, R24 · 
himadri-plugins → R3, R27 · himadri-auth → R2, R8, R13, R25 · 
himadri-observability → R5, R22, R31 · himadri-ratelimit / circuitbreaker →
R4, R30 · himadri-admin → R6, R12, R16, R17, R23, R29, R32, R33 · 
himadri (bin) → R3, R14, R15, R34, R35, R36 · repo root → R37.

---

## P0 — Correctness fixes found during review

These are behavior bugs; fix them first (some are fixed *by* the refactors
below, noted where so).

### ☑ R1. OpenAI-compatible requests silently drop `tool_calls`, `tool_call_id`, and all passthrough params (`extra`)

`crates/himadri-provider/src/compatible/provider.rs:107-205` hand-builds the
request JSON and forwards only role/content/name per message plus a fixed
param list. Consequences:

- Multi-turn tool use is broken through the gateway: an assistant message
  carrying `tool_calls` and a `role: tool` message carrying `tool_call_id`
  lose those fields, so OpenAI-compatible upstreams reject the conversation.
- Every passthrough param in the flattened `extra` map (`response_format`,
  `seed`, `n`, `logit_bias`, …) is dropped — while the response cache
  (`himadri-plugins/src/cache.rs`) deliberately keys on `extra` because those
  params "materially change the completion".
- `bedrock/provider.rs:54-162` duplicates the same builder with the same gaps.

**Fix:** see R18 — serialize the already-`Serialize` `ChatCompletionRequest`
instead of hand-rolling. No test currently pins the dropped-field behavior
(verified by grep), so this is a safe fix. Add a regression test asserting a
`tool_call_id` message and one `extra` key survive into the built body.

### ☑ R2. Zitadel rate-limit conversion truncates sub-60 RPM to 0 rps

`crates/himadri-auth/src/zitadel.rs:105` uses `rpm / 60` while
`jwt.rs:186-191` was already fixed to `rpm.div_ceil(60)` with a comment
explaining why (a 0-rate bucket never refills — burst-only). If zitadel.rs
survives R8, apply the same fix; if it's deleted, this goes with it.

### ☑ R3. `RATE_LIMIT_KEY_RPM` silently imposes a global 100 req/s cap

`crates/himadri/src/main.rs:409-419` registers a `RateLimitPlugin` with only
`key_rpm` set. The plugin (`himadri-plugins/src/rate_limit.rs:235`) defaults
its **internal global limiter** to `requests_per_second.unwrap_or(100)` and
checks it on every request — so configuring a per-key limit sneaks in an
unrelated global 100 rps ceiling. The per-IP block just below works around
this with an explicit `1_000_000`.

**Fix:** register **one** plugin carrying both `key_rpm` and `ip_rpm`
(collapses the two blocks, ~20 lines), and make the plugin's global limiter
opt-in (`None` = skip the global check) instead of defaulting to 100.
Add a test: key-RPM-only config must admit >100 requests/sec globally.

### ☑ R4. In-memory usage/request-log stores grow without bound

Every request inserts into `UsageStore.records`
(`himadri-admin/src/usage_store.rs:145`) and
`InMemoryRequestLogStore.entries` (`request_log.rs:66`); nothing ever evicts.
A long-running gateway leaks memory linearly with traffic; `get_dashboard`
also scans the whole map per call. Cap both (ring buffer / max-entries with
oldest-first eviction) the way `SpendStore` and `RateLimiterStore` already
cap their maps. Related: the Postgres-backed alternatives exist but are
unwired — see R16.

### ☑ R5. Gemini API key rides in the URL query string

`crates/himadri-provider/src/gemini/provider.rs:284-287,316-319` builds
`...:generateContent?key={api_key}`. reqwest errors include the URL, and
`ProviderError::Network(e.to_string())` forwards that text into logs, audit
events, and client-facing 502 messages — leaking the provider credential.
Send the key via the `x-goog-api-key` header instead (supported by the same
API) and keep the URL secret-free.

### ☑ R6. `himadri-admin` fails to build with postgres-only features

`crates/himadri-admin/src/lib.rs:1` declares `pub mod config_store;`
unconditionally, but `config_store.rs` uses `SqlitePool`. It compiles only
because `default = ["sqlite"]`. Either cfg-gate it
(`#[cfg(feature = "sqlite")]`, matching `provider_store`) or delete it per
R16. Verify with the postgres-only `cargo check` from the baseline block.

### ☑ R7. The `otlp` feature does not compile (8 errors)

`crates/himadri-observability/src/tracing_setup.rs:3` renames params to
`_service_name`/`_endpoint`/`_sample_ratio`, but the
`#[cfg(feature = "otlp")]` block references the non-underscore names, plus
the opentelemetry-0.27 API calls are wrong (`new_exporter`, missing trait
import). Confirmed: `cargo check -p himadri-observability --features otlp`
fails. Nothing enables the feature, so OTLP tracing has never worked; the
core `TracingConfig` (service_name/endpoint/sample_ratio) plumbs values into
a function that ignores them.

**Decide:** fix the feature or delete it. For the line-reduction goal:
delete the `otlp` feature, the `init_otlp` fn (~45 lines), the four optional
otel deps in `himadri-observability/Cargo.toml`, and the now-unused
opentelemetry entries in the workspace `Cargo.toml` (git preserves it all
for a future re-attempt).

---

## P1 — Dead code to delete (largest wins, zero behavior change)

All verified unreferenced by workspace grep; each deletion should be
followed by `cargo check --workspace --all-targets`.

### ☑ R8. himadri-auth: four unwired modules (−550 to −650 lines)

No code outside `crates/himadri-auth/src` references any of:

| Module | Lines | Contents |
|---|---|---|
| `oauth2_client.rs` | 142 | `TokenClient`, client-credentials flow |
| `middleware.rs` | 101 | `JwtAuthMiddleware` (the bin uses its own `combined_auth.rs`) |
| `introspect.rs` | 164 | `TokenIntrospector` (RFC 7662) |
| `zitadel.rs` | 143 | `ZitadelClaims` (the live Zitadel role mapping is in `jwt.rs::roles()`) |

Only the crate's own `lib.rs` unit tests keep them alive. The live auth path
is `OidcDiscovery` + `JwtClaims` + the bin's `CombinedAuth`. Also mostly dead
in `config.rs`: `AuthConfig`/`OAuth2Config` (JWT settings come from
`JWT_ISSUER`/`JWT_AUDIENCE`/`JWT_JWKS_URI` env vars in `main.rs`).

**Fix:** delete the four modules, their `lib.rs` exports and tests, and trim
`config.rs` to what's used. If OAuth2 introspection is on the roadmap,
resurrect from git when it's actually wired; keeping ~40% of the auth crate
dead invites drift (R2 is exactly that kind of rot).

### ☑ R9. himadri-provider `types.rs` is entirely dead (−229 lines)

`crates/himadri-provider/src/types.rs` defines `OpenAiRequest/Response`,
`Anthropic*`, `Gemini*`, `ModelListResponse` DTOs — leftovers from the
deleted per-vendor providers. Zero references anywhere (all providers build
JSON via `serde_json::json!` or core types; `ModelListResponse` duplicates
`himadri_core`'s). Delete the file and the `pub mod types;` line in `lib.rs`.

### ☑ R10. Redis backends: scaffolded, never constructed (−460 lines)

`RedisCircuitBreaker` (~230 lines, `himadri-circuitbreaker/src/lib.rs`),
`RedisRateLimiter` (114 lines, `himadri-ratelimit/src/redis_store.rs`), and
`RedisLatencyStore` (~115 lines, `himadri/src/latency_store.rs`) are all
feature-gated and constructed nowhere — `main.rs`/`gateway.rs` contain no
`cfg(feature = "redis")` wiring at all. The `redis` features in three crates
plus the optional `redis` dependency exist solely to compile unreachable
code.

**Decide:** delete (recommended — distributed state is a feature to design
deliberately, and git keeps the drafts) or actually wire them behind
`REDIS_URL`. If deleting, also drop the `redis` features from
`himadri/Cargo.toml`, `himadri-circuitbreaker/Cargo.toml`,
`himadri-ratelimit/Cargo.toml`. Bonus: with the Redis breaker gone, the
`STATE_*` u8 constants in the circuit breaker exist only for Redis
serialization — `BreakerInner` can hold `CircuitState` directly and `state()`
becomes a field read (−10 more).

### ☑ R11. Bedrock provider is a non-functional stub (−362 lines, decision)

`crates/himadri-provider/src/bedrock/provider.rs` documents itself as
unusable against real AWS: Bearer auth instead of SigV4 (`provider.rs:20-23,
301-302`), and it parses `application/vnd.amazon.eventstream` (a binary
framing) with the SSE decoder — real Bedrock streaming can never parse.
`main.rs` registers it whenever `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`
exist, so plausible-looking config routes traffic into guaranteed failures.

**Decide:** delete the provider + its registration until SigV4 lands
(recommended; the `himadri-provider/src/tests.rs` cases for it go too), or
keep it but stop registering it from ambient AWS env vars (require an
explicit `BEDROCK_ENABLE=1`). Either way the copy-pasted message builder and
tool-choice translation disappear via R18/R20.

### ☑ R12. Unwired persistence in himadri-admin (−320 lines, decision)

None of these are referenced outside the admin crate:

- `PostgresConfigStore` (`postgres_backends.rs:9-38`) — a **no-op stub**:
  `save` does nothing, `load` returns `None`.
- `SqliteConfigStore` (`config_store.rs`, 105 lines) — config history is
  actually kept in-memory in `gateway.rs::ConfigHistory`.
- `PostgresUsageStore` (`postgres_backends.rs:279-456`) — the gateway only
  ever uses the in-memory `UsageStore`, so usage/billing data is lost on
  restart even when Postgres is configured.

**Decide per store:** delete the two config stores (in-memory history is the
implemented behavior; R6 disappears with them), and either wire
`PostgresUsageStore` into `build_gateway` alongside `PostgresRequestLogStore`
(fixes the R4 durability half) or delete it too and re-derive from git when
usage persistence is scheduled.

### ☑ R13. Strategy::select duplicates select_ordered arm-by-arm (−85 lines)

`crates/himadri/src/strategy.rs:189-276` (`select`, already
`#[allow(dead_code)]`, kept for tests) re-implements every strategy arm that
`select_ordered` also implements; the doc comment even promises the first
element of `select_ordered` "is identical to select". Enforce that
structurally:

```rust
pub async fn select(&self, request: &ChatCompletionRequest, targets: &[Target])
    -> Result<Target, GatewayError>
{
    Ok(self.select_ordered(request, targets).await?.remove(0))
}
```

`strategy_tests.rs` keeps passing (it tests behavior, not implementation),
and the two paths can never drift again. Move `with_latency_store` under
`#[cfg(test)]` while there.

### ☑ R14. Stray root `load_test_sink.rs` (−198 lines)

The repo root file is an older, drifted copy of
`crates/load-test-sink/src/main.rs` (diff confirms divergence). It belongs to
no build target; `cargo run --bin load_test_sink` resolves to the crate.
Delete the root file.

### ☑ R15. Unused `clap` dependency + assorted small dead spots

- `crates/himadri/Cargo.toml` declares `clap = { workspace = true }` but
  `main.rs:33-70` parses args by hand, with a comment claiming the parser
  avoids the dependency. Delete the dep from himadri and from
  `[workspace.dependencies]` (no other crate uses it).
- `AdminHandlers::_master_key` (`himadri-admin/src/handlers.rs:13,22`) is
  never read; drop the field and the constructor param (callers pass the
  master key to `AuthMiddleware` separately already).
- `crates/himadri/Cargo.toml` forces `himadri-admin = { features =
  ["postgres"] }` in both `[dependencies]` and `[dev-dependencies]`, making
  himadri's own `sqlite`/`postgres` feature split half-meaningless (postgres
  code always compiles). Drop the hardcoded feature and let the existing
  `postgres = ["himadri-admin/postgres"]` forwarding do its job; keep the
  dev-deps one only if the e2e tests need it.

---

## P2 — Structural deduplication (medium effort, large payoff)

### ☑ R16. One dispatch macro for the three backend enums (−240 lines)

`StoreBackend` (8 hand-written match-dispatch methods,
`himadri-admin/src/store.rs:193-289`), `ProviderStoreBackend` (7) and
`ModelStoreBackend` (8) (`provider_backend.rs:36-288`) repeat the identical
`match self { Sqlite(s) => s.m(...).await.map_err(|e| e.to_string()), ... }`
body ~23 times. One `macro_rules!` in the admin crate:

```rust
macro_rules! dispatch {
    ($self:expr, $s:ident => $call:expr) => {
        match $self {
            #[cfg(feature = "sqlite")]  Self::Sqlite($s)   => $call.await.map_err(|e| e.to_string()),
            #[cfg(feature = "postgres")] Self::Postgres($s) => $call.await.map_err(|e| e.to_string()),
            ...
        }
    };
}
```

turns every method into 1–3 lines, and adding a store method stops being
23 lines of ceremony. (`StoreBackend` has the extra `Memory` arm — give it
its own arm in the macro or a second macro.)

### ☑ R17. Fetch-merge-update duplication & SQL column lists (−60 lines + drift-proofing)

Still in the admin crate:

- The 16-column select list for `api_keys` is pasted **12 times**
  (`store.rs`). Hoist `const API_KEY_COLUMNS: &str = "id, name, key, ..."`
  and `format!` it into the queries — adding a column becomes a one-place
  change.
- `store.rs` `RateLimitOverride` + `TokenBudget` re-declare
  `himadri_core::RateLimitOverride` / `OrgTokenBudget` field-for-field, and
  `middleware.rs:66-69` exists only to convert between the twins. Use the
  core types and delete the conversion.
- `config_store.rs::parse_timestamp` duplicates `sqlite_time::parse` exactly
  (moot if R12 deletes the file).

### ☑ R18. Build provider request bodies with serde, not by hand (−140 lines, fixes R1)

`ChatCompletionRequest` already derives `Serialize` with the right shapes
(lowercase roles, untagged content, skip-if-none tools). Steps:

1. Add `skip_serializing_if = "Option::is_none"` to the request's optional
   fields in `himadri-core/src/types.rs` (`temperature`, `top_p`,
   `max_tokens`, `stop`, `presence_penalty`, `frequency_penalty`, `user`) so
   serialization omits them instead of emitting `null`s.
2. `OpenAiCompatibleProvider::build_request_body` becomes ~10 lines:
   `serde_json::to_value(request)` + overwrite `stream` + insert
   `stream_options` when streaming.
3. Bedrock's copy of the message builder collapses the same way (if the
   provider survives R11).

This is what fixes R1 (tool_calls / tool_call_id / extra forwarding) —
land them together with the new regression test.

### ☑ R19. Shared helpers across Anthropic/Gemini/Bedrock (−140 lines)

Same logic pasted per provider:

- `MessageContent::flat_text()` already exists in core, yet
  `anthropic/provider.rs:45-56` and `gemini/provider.rs:40-51` re-implement
  it inline. Call the core method.
- Stop-reason mapping (`end_turn→stop`, `max_tokens→length`) appears 5× —
  once each in Anthropic parse_response / message_delta, Bedrock ×2, Gemini
  ×2 (`STOP/MAX_TOKENS`). Two tiny free fns in a shared module.
- Usage extraction (`u["x"].as_u64().unwrap_or(0) as u32` triples) appears
  6× — one helper per vendor shape.
- Derive `Default` on `StreamChunk`, `Delta`, `StreamChoice`,
  `ChatCompletionResponse`, `ResponseMessage` in core, then Anthropic's three
  25-line `StreamChunk` literals (`anthropic/provider.rs:201-272`) and
  Gemini/Bedrock's collapse to `StreamChunk { id, model, choices,
  ..Default::default() }` via a small `fn chunk(delta, finish, usage)`
  builder.
- The one-line `handle_error` wrappers in compatible/anthropic can be
  inlined at call sites.

### ☑ R20. Share the Anthropic tool-choice translation with Bedrock (−25 lines)

`bedrock/provider.rs:140-158` re-inlines
`AnthropicProvider::translate_tool_choice` (`anthropic/provider.rs:120-137`)
verbatim. Move the fn to a crate-level `tool_translation` module (or
`error.rs`-style shared file) and call it from both. Moot for Bedrock if R11
deletes it — still worth doing for Anthropic + any future Claude-schema
vendor.

### ☑ R21. Derive `Default` on request/auth types; kill the 20-line test literals (−250 lines)

19 sites build a full `ChatCompletionRequest { ...12 fields... }` literal
and 13 build a full `AuthContext` literal (counted by grep across
plugins/providers/gateway/e2e tests). Every new field touches all of them —
`extra: Default::default()` was clearly appended 19 times when that field
landed.

**Fix:** `#[derive(Default)]` on `ChatCompletionRequest`, `Message`
(role already has a Default), plus manual `Default` for `AuthContext`
(reuse `anonymous()`), `CreateApiKeyRequest`, `AuditEvent` (status:
Success). Then test sites become
`ChatCompletionRequest { model: "x".into(), messages: vec![Message::user("hi")], ..Default::default() }`
with a tiny `Message::user(impl Into<String>)` constructor in core.
Also lets `log_auth_failure` (`audit.rs:130-160`) and `route_stream`'s
audit literal (`gateway.rs:587-607`) shrink to their non-empty fields.

### ☑ R22. Presets and provider registration as data (−120 lines)

- `OpenAiCompatibleConfig`: the 8 preset constructors
  (`compatible/provider.rs:368-502`) are the same struct with 4 varying
  values. One `fn preset(name, display, base_url, headers, models) -> Self`
  makes each preset 2–4 lines (−80).
- `main.rs::register_providers_from_env:279-333`: seven identical
  "if API-key env var set → register preset" blocks. A table
  `[("OPENROUTER_API_KEY", OpenAiCompatibleConfig::openrouter as fn() -> _), ...]`
  plus a 5-line loop (−40). Adding a vendor becomes one table row.

### ☑ R23. Config structs: container-level serde defaults (−85 lines)

`himadri-core/src/config.rs` carries 10 `default_*` free fns + per-field
`#[serde(default = "...")]` attributes + hand-written `Default` impls that
restate the same values. For every struct whose fields are all defaulted
(`CorsConfig`, `StrategyConfig`, `TracingConfig`, `MetricsConfig`,
`RateLimitConfig`, `AdminConfig`), use container-level `#[serde(default)]`
(serde fills missing fields from `Self::default()`) and keep **one** manual
`Default` impl as the single source of truth. Same trick for
`himadri-admin/src/models.rs` and `himadri-auth/src/config.rs`
(post-R8 remainder). Also:

- `OrgConfig` and `TeamConfig` are field-identical except `teams`; extract a
  `#[serde(flatten)] pub policy: PolicyConfig` shared struct (verify JSON
  round-trip against a config-history test — flatten changes no wire shape
  here but prove it).
- Fold `Config::default_config` into `impl Default for Config` (it's called
  from both `Default` and `load_from_env`).

### ☑ R24. Duplicated per-request send scaffolding in providers (−40 lines)

`complete` / `complete_stream` / `embed` in
`compatible/provider.rs:267-363` each rebuild client + auth header +
Content-Type + extra headers + send + status check. Extract
`async fn send(&self, url, body, streaming: bool) -> Result<reqwest::Response, ProviderError>`;
Anthropic's two copies of its header stack collapse the same way.

### ☑ R25. Auth crate small dedups (−60 lines)

- `deserialize_optional_u64` / `deserialize_optional_f64` are pasted
  verbatim in `jwt.rs:88-113` and `zitadel.rs:59-84` (zitadel copy dies with
  R8; otherwise share one `de` module).
- JWKS fetch + error-mapping appears twice (`oidc.rs:62-70` and `:169-177`)
  — extract `async fn fetch_jwks(client, uri)`.
- The three `static CACHE + get_or_create_*` blocks (oidc, and
  introspect/oauth2_client if kept) are the `StoreRegistry` pattern —
  reuse it if more than one survives R8.

### ☑ R26. rbac.rs: one check fn instead of two (−25 lines)

`check_model` / `check_provider` (`himadri-core/src/rbac.rs:136-173`) differ
only in which `RolePolicy` field they read and which denial they build.
Parameterize: `fn check(&self, roles, is_admin, value, dim: Dimension)` with
a two-variant enum, or a private fn taking
`fn(&RolePolicy) -> &Option<Vec<String>>` + denial constructor. Same for the
two identical union-merge blocks in `effective_policy` (models/providers) —
one small `fn merge(acc: &mut Option<Vec<String>>, item: &Option<Vec<String>>)`.

### ☑ R27. rate_limit / budget plugin cleanups (−50 lines)

`himadri-plugins/src/rate_limit.rs`:

- Four copy-pasted rejection blocks in `execute` → one
  `fn rejected(&self, what: &str) -> PluginError` (−25).
- Three near-identical store creations in `new` → one closure over
  `(prefix, rpm)` (−15).
- `now_micros() -> i64` is cast to `u64` at every call site — return `u64`.
- Rename the `test_sliding_window_*` tests: the type is `FixedWindowCounter`
  (comment drift from a rename), and the budget test comment "must evict
  low-spend (min spend)" describes the old policy — eviction is LRU now.

`budget.rs`: inline the single-use `get_or_create_store` wrapper.

### ☑ R28. PluginManager: derive Default, unify the logged run-loops (−40 lines)

`himadri-plugin/src/manager.rs`: `new()` + manual `Default` → `#[derive(Default)]`
(all fields are `Vec`). `run_after` / `run_after_response` / `run_on_error`
are the same log-and-continue loop over a different Vec — one private
`async fn run_logged(&self, plugins: &[Arc<dyn Plugin>], ctx, stage: &str)`.
Also `PluginContext` (`context.rs`): ten trivial setters over `pub` fields
are pure ceremony — callers can assign; keep only the ones with logic
(`get_full_response_text`, the auth accessors).

### ☑ R29. usage_store: one top-N aggregation, bounded "recent errors" (−30 lines)

`get_dashboard` (`usage_store.rs:206-263`) builds model-stats and
provider-stats with two copy-pasted accumulate/sort/truncate blocks over
structurally identical `ModelUsage`/`ProviderUsage`. One generic
`fn top_usage(records, key: fn(&UsageRecord) -> &str) -> Vec<(String, u64, u64, f64)>`
feeds both. Note while there: `recent_errors` takes 10 in DashMap iteration
order — arbitrary, not recent; sort by `created_at` (or fix via R4's ring
buffer, which makes order meaningful).

### ☑ R30. Misc single-crate dedups (−40 lines)

- `himadri-ratelimit/src/lib.rs`: `check_org`/`check_key` → one
  `check_entity(prefix, id, rate, burst)`.
- `himadri-observability/src/metrics.rs:104-137`: eleven copy-pasted
  `registry.register(...)` blocks → loop over
  `[Box<dyn Collector>; 11]`.
- `himadri/src/handlers.rs:817-834`: `is_private_or_loopback` re-implements
  `himadri-core::net_guard::ip_is_internal` (and line 825 re-checks
  link-local that line 823 already covers). Make core's fn `pub` and reuse.
- `gateway.rs:819-831` (proxy): the five-verb match is unnecessary —
  `method.parse::<reqwest::Method>()` handles all verbs; keep only the
  error mapping.
- `store.rs:28-38` `masked_key_display`: the double-`rev()` dance is a
  byte-slice one-liner for these ASCII values
  (`&s[s.len().saturating_sub(6)..]`).

---

## P3 — Maintainability (not primarily line count)

### ☑ R31. Audit sink polish

`himadri-observability/src/audit.rs::write_loop` calls `create_dir_all` per
event — hoist it before the loop, warn once. `Redactor::redact` reallocates
the full string five times per call — chain `Cow`s or a single pass (hot
only when content capture is on).

### ☑ R32. AdminHandlers swallow every error into `Option` (debuggability)

Every method in `himadri-admin/src/handlers.rs` maps store errors to
`None`/`vec![]`/`false` — a DB outage is indistinguishable from "not found"
(HTTP 404), and `create_provider`'s SSRF rejection reason
(`handlers.rs:96-104`) never reaches the client (generic 500 "Failed to
create provider"). Return `Result<_, String>` through `AdminHandlers`, let
the axum handlers in `himadri/src/handlers.rs` map Err→500-with-reason and
Ok(None)→404. Small diff, large operational payoff.

### ◐ R33. (Carried forward) SQLite/Postgres parallel store unification

**Deferred (2026-07-06):** left open by deliberate decision — it was always
optional, and R16's dispatch macro (done) already removed the drift pain that
was its main cost. Revisit only when a third backend or new store methods
actually land; the generic/macro `Store<DB>` unification isn't worth the
Postgres-migration risk today.

From the 2026-07-02 review §4, still optional: `provider_store.rs` (419) vs
`postgres_provider_store.rs` (444) and the `store.rs` pair implement the
same CRUD twice. sqlx's `Any` driver was already ruled out (no
chrono/uuid/serde_json). Revisit only if a third backend or new store
methods appear; the R16 dispatch macro reduces the day-to-day pain first.
Run gated Postgres tests via
`TEST_POSTGRES_URL=... cargo test --features postgres postgres`
before touching Postgres SQL.

### ☑ R34. Stop compiling gateway/strategy/latency_store twice

`crates/himadri` is lib + bin, and `main.rs:1-5` re-declares `mod gateway;
mod strategy; mod latency_store;` that `lib.rs` also declares — the bin
compiles its own private copies instead of using the lib (integration tests
use the lib via the `himadri-gateway` dev-dep alias). Make the bin consume
the lib (`use himadri::gateway::Gateway;` …), keeping only
`handlers`/`combined_auth` bin-local (or move them into the lib and shrink
`main.rs` to wiring). Cuts duplicate compilation, duplicate lint surface,
and the "which copy am I editing?" hazard.

### ☑ R35. Hardcoded model lists and prices are data, not code

Three stale-prone tables live in source: preset `models:` lists
(`compatible/provider.rs`), `supported_models()` in Anthropic/Gemini/Bedrock,
and `UsageStore::default_pricing` (`usage_store.rs:88-140`) — the last one
**drives real budget math** (unknown model = $0 cost, silently). Move
pricing to config/env (the budget plugin already takes env-configured
pricing — one mechanism should win), and treat `supported_models` as
display-only fallback (it already is when DB models exist).

### ☑ R36. Providers translate tools *into* requests but drop them from responses

`anthropic/provider.rs:139-184`, `gemini/provider.rs:144-191`, and Bedrock
always emit `tool_calls: None`, discarding `tool_use` / `functionCall`
blocks the model returns — a client that sends tools gets back a bare text
(often empty) response. Either map the blocks into OpenAI-style `tool_calls`
(Anthropic: `content[].type == "tool_use"` → id/name/input; Gemini:
`parts[].functionCall`) or reject tool-bearing requests for these providers
until implemented. Silent drop is the worst option. Add round-trip tests
mirroring the existing request-side `tool_tests`.

### ☑ R37. Docs index & changelog hygiene

`docs/REFACTORING.md` was removed with this review (its content is
superseded; §4 lives on as R33). `README.md` and `docs/README.md` now point
here — keep this file the single active review checklist and mark items
☑ in place as they land (the CHANGELOG references the old file historically;
leave those entries as-is).

---

## Suggested order of attack

1. **R1+R18 together** (correctness + biggest single refactor, one test run).
2. R3, R5, R2 (small correctness fixes).
3. P1 deletions R9 → R14 → R15 → R13 → R8 → R10 → R7 → R11/R12 decisions
   (each is an independent, safely-revertible commit).
4. R21 (Default derives) — makes every later diff smaller.
5. R16/R17, R22, R23, R19/R20, then the rest of P2 in any order.
6. P3 as capacity allows; R36 before advertising tool support.

After each item: `cargo test --workspace` and, for provider-touching items,
the wiremock-backed `crates/himadri-provider/src/tests.rs` +
`crates/himadri/tests/feature_tests.rs` suites.
