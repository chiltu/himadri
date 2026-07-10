## Assistant

# Code Quality Suggestions — himadri

Review based on the current workspace (~21k LOC Rust across 11 crates, Next.js admin in `web/`, ~296 tests). The project already has strong documentation, a solid crate split, and recent refactor work (`CODE_REVIEW.md` — 36/37 items). Below is a prioritized quality report grounded in the **current** code, including open issues from `docs/CODE_REVIEW_2026-07-10.md` that are still present.

> **Status update (2026-07-10):** all P0 items and every P1 correctness/availability item are now fixed and covered by regression tests (workspace: all tests pass, clippy clean). Quick structural wins (shared crypto, vendor registry + `/admin/known-providers`, metrics expects, Dockerfile, docs drift) are also done. Per-item status is annotated below; the remaining OPEN items are the large refactors (gateway module split, store unification, typed admin errors, frontend auth storage) that should each be their own change.


---

## Architecture snapshot (what’s solid)

| Area | Assessment |
|------|------------|
| Crate boundaries | Clear: core types, providers, plugins, admin, auth, observability |
| Provider abstraction | Good: one `OpenAiCompatibleProvider` + bespoke Anthropic/Gemini |
| Lock discipline | Documented lock order (`strategy → config → targets`) |
| Auth layering | API key + OIDC/JWT via `combined_auth` (AGENTS.md is slightly stale here) |
| Observability | Metrics, audit redaction, SSRF guard for base URLs |
| Tests | Broad unit/e2e (~296); clippy mostly clean (2 style warnings) |

---

## P0 — Security & data integrity (fix first)

### 1. API keys leaked on write endpoints

> **Status (2026-07-10):** FIXED — `created`/`updated` responses now pass through `redact_endpoint()`

**Where:** `handlers.rs` — `create_model_endpoint` / `update_model_endpoint` / `toggle_model_endpoint` go through `created()` / `updated()` without `redact_endpoint()`.

GET/list paths redact correctly; POST/PUT responses still return the full `ModelEndpoint` including decrypted `api_key`.

**Suggestion:** Apply `redact_endpoint()` in `created`/`updated` (or only on endpoint handlers). Add a regression test that write responses never include `api_key`.

---

### 2. Decrypt failure can permanently wipe stored credentials

> **Status (2026-07-10):** FIXED — `update()` treats `api_key: None` as "leave the stored column alone"

**Where:** `provider_store.rs` / `postgres_provider_store.rs` `update()`:

```249:251:crates/himadri-admin/src/provider_store.rs
        // `current.api_key` is already decrypted; re-encrypt whatever we store.
        let api_key = request.api_key.unwrap_or(current.api_key);
        let stored_api_key = self.encrypt_api_key(api_key);
```

On decrypt failure, `decrypt_endpoint` sets `api_key = None`. An update that omits `api_key` then writes `NULL` over the ciphertext.

**Suggestion:** On decrypt failure, either fail the update, or preserve the original encrypted column when `request.api_key` is absent. Never treat “decrypt failed” as “no key.”

---

### 3. Admin master key in `localStorage`

> **Status (2026-07-10):** LARGELY FIXED — the dashboard no longer handles the master key at all: the login form is username+password only (`DEV_ADMIN_PASSWORD` → `POST /auth/admin/login`), and the browser holds a short-lived (12h default), per-boot-revocable JWT in `sessionStorage`. The master key stays server-side / CLI-only. Residual: any JS-readable token remains XSS-sensitive for its lifetime — httpOnly session cookies would close that last gap.

**Where:** `web/lib/api.ts` stores `himadri_master_key` in `localStorage`.

Any XSS in the dashboard exfiltrates full admin credentials.

**Suggestion:** Prefer httpOnly session cookies (or short-lived tokens) for the UI; keep bearer keys out of JS-readable storage for production deployments.

---

### 4. Auth fully disabled without `MASTER_KEY` / `JWT_ISSUER`

> **Status (2026-07-10):** FIXED — startup fails when `REQUIRE_AUTH=1` or `RUST_ENV`/`HIMADRI_ENV` is production/staging without `MASTER_KEY`

Documented as “dev only,” but easy to misconfigure in staging/prod.

**Suggestion:** Fail startup in non-dev mode when neither is set (`RUST_ENV=production` / explicit `REQUIRE_AUTH=1`), or log a loud continuous warning.

---

## P1 — Correctness & availability

### 5. Config reload can wipe all routing targets

> **Status (2026-07-10):** FIXED — reassert + startup now guard on `Gateway::db_has_active_targets` (enabled model with enabled endpoint); regression test added

**Where:** `reassert_db_targets_after_config`:

```579:588:crates/himadri/src/handlers.rs
    async fn reassert_db_targets_after_config(&self) {
        let endpoints = self.admin.list_endpoints().await;
        if endpoints.is_empty() {
            return;
        }
        // ...
        self.gateway.rebuild_targets_from_db(&models, &endpoints).await;
    }
```

Guard is “any endpoints,” but rebuild only keeps **enabled** ones. All-disabled DB endpoints → empty target list, overwriting config/env targets → full outage on chat.

**Suggestion:** Guard on *enabled* endpoints (and preferably merge DB + config targets instead of full replace).

---

### 6. `dedup_targets` drops legitimate multi-endpoint failover

> **Status (2026-07-10):** FIXED — `target.id` is part of the dedup key; regression test added

```300:311:crates/himadri/src/strategy.rs
    fn dedup_targets(targets: Vec<Target>) -> Vec<Target> {
        // keys only on (provider, api_key_env, base_url) — not target.id
```

Two OpenAI DB endpoints with different keys and `base_url=None` collapse to one → broken failover/weighted multi-key setups.

**Suggestion:** Include `target.id` in the dedup key (or skip dedup when `id` is set).

---

### 7. Race: `provider_keys.clear()` during live traffic

> **Status (2026-07-10):** FIXED — rebuild inserts new keys first, then retains live ones (no empty window); regression test added

```836:836:crates/himadri/src/gateway.rs
        self.provider_keys.clear();
```

Then keys are re-inserted; concurrent `get_api_key` can get `Ok("")` and send empty Bearer tokens → 401s and false circuit-breaker trips.

**Suggestion:** Build a new map, then swap atomically (or hold a write guard over clear+repopulate). Related smell: `get_api_key` returns empty string instead of error when a DB key is missing — prefer explicit `ServiceUnavailable` / `Unauthorized` for empty keys.

---

### 8. `/v1/models` vs routing disagree

> **Status (2026-07-10):** FIXED — shared `himadri_core::endpoint_is_routable` used by `/v1/models`; drift-guard test ties it to `build_provider_client`; regression test added

`list_enabled_models_for_api` treats any enabled endpoint as active; `rebuild_targets_from_db` skips unknown `provider_type` without `base_url`. Models can appear in the catalog and then 404 on completion.

**Suggestion:** Share one “is this endpoint routable?” helper for listing and rebuild.

---

### 9. Deletes swallow DB errors as “not found”

> **Status (2026-07-10):** FIXED — `delete_endpoint`/`delete_key` return `Result`; endpoint delete uses the error-aware `deleted()` helper, key delete maps store errors to 500

```246:250:crates/himadri-admin/src/handlers.rs
    pub async fn delete_endpoint(&self, id: &str) -> bool {
        match &self.model_endpoint_store {
            Some(s) => s.delete(id).await.unwrap_or(false),
```

Same pattern for keys/models in places. Transient DB failures become HTTP 404.

**Suggestion:** Propagate `Result` for mutations (at least deletes/updates); map store errors to 500. Align `delete_model_endpoint` with the error-aware `deleted()` helper used by `delete_model`.

---

### 10. Full CB/rate-limit reset on every model/endpoint mutation

> **Status (2026-07-10):** FIXED — rebuild retains breakers for surviving endpoints and no longer touches the rate limiter (its buckets are keyed by org/key, not endpoint); regression test added

`rebuild_targets_from_db` ends with:

```887:889:crates/himadri/src/gateway.rs
        self.rate_limiter.clear();
        self.circuit_breakers.clear();
```

Toggling one endpoint resets global breaker/limiter state under load.

**Suggestion:** Remove only stale keys for deleted endpoints; leave healthy breaker state intact.

---

## P2 — Maintainability & structure

### 11. `gateway.rs` is a ~2k-line god module

> **Status (2026-07-10):** FIXED — split into `gateway/` with `mod.rs` as a thin facade (struct, construction, registration, accessors, key resolution) plus `route.rs`, `stream.rs`, `policy.rs`, `audit.rs`, `config.rs`, `rebuild.rs`, `providers.rs`, `proxy.rs`; largest file is now ~460 lines and each test module lives with its subject. Public API unchanged (`himadri::gateway::Gateway`).

Routing, failover, streaming, proxy, config history, rebuild, rate limits, audit, and caching live in one file (~74 `.clone()` call sites).

**Suggestion:** Split into modules, e.g.:

- `gateway/route.rs` — non-stream path  
- `gateway/stream.rs` — stream + usage recorder  
- `gateway/rebuild.rs` — DB target rebuild  
- `gateway/proxy.rs` — catch-all proxy  
- `gateway/providers.rs` — `build_provider_client` + vendor registry  

Keep `Gateway` as a thin facade.

---

### 12. Duplicated encryption across SQLite and Postgres

> **Status (2026-07-10):** FIXED — policy moved to `crypto.rs` (`encrypt_endpoint_api_key` / `decrypt_endpoint`); both stores delegate

`encrypt_api_key` / `decrypt_endpoint` are copy-pasted in both stores even though `crypto.rs` already exists for AES-GCM.

**Suggestion:** Move encrypt/decrypt-or-clear policy into `crypto.rs` (or a small `endpoint_crypto` helper) and call it from both backends.

---

### 13. Vendor preset registry in three places

> **Status (2026-07-10):** FIXED — single source in `himadri_core::provider_registry`, served on `GET /admin/known-providers`; web UI fetches it (hardcoded list kept only as fallback)

Hardcoded in:

1. `gateway.rs` `build_provider_client`  
2. `main.rs` `register_providers_from_env`  
3. `web/.../models/page.tsx` `KNOWN_PROVIDER_TYPES`

**Suggestion:** Single Rust source of truth + `GET /admin/known-providers` for the UI.

---

### 14. SQLite ↔ Postgres store pairs still parallel implementations

> **Status (2026-07-10):** ADDRESSED via contract tests — `himadri-admin/src/store_parity_tests.rs` runs one shared behavior contract against both backends (SQLite always; Postgres when `TEST_POSTGRES_URL` is set — see AGENTS.md). Covers CRUD lifecycles, the enabled-model delete `Conflict`, delete-cascade outcome (plus a structural check that Postgres keeps its `ON DELETE CASCADE` FK), key encryption round-trip / partial-update preservation, and missing/malformed-id semantics. Divergences found and fixed while writing it: Postgres surfaced malformed UUIDs as errors (409/500) where SQLite returned not-found; both backends misclassified "endpoint created under nonexistent model" as a 500 `Protocol` error (now `NotFound` → 404). The implementations intentionally remain parallel (see the sea-query/SeaORM discussion) — the contract suite is what pins them together.

`provider_store.rs` (~344) + `postgres_provider_store.rs` (~364), plus similar patterns for keys/usage. Dispatch macros help API surface; SQL and business rules still diverge (e.g. model-delete cascade: app-level vs `ON DELETE CASCADE`).

**Suggestion:** Shared query builders / integration tests that run the same suite against both backends; align cascade policy explicitly.

---

### 15. Error-type inconsistency

> **Status (2026-07-10):** FIXED — `himadri_admin::AdminError` (`NotFound`/`Validation`/`Conflict`/`Store`) now flows from the concrete stores through the backends and `AdminHandlers` facade; one `From<AdminError> for ApiError` maps to HTTP (404/400/409/500, store detail logged but not echoed). Also fixed along the way: the enabled-model delete guard was smuggled through `sqlx::Error::Protocol` (now a real `Conflict`); malformed UUIDs on Postgres surfaced as 409 instead of 404; SSRF-rejected endpoint URLs returned a generic 500 instead of 400 with the reason; and `rebuild_targets` no longer wipes live routing targets when the store read fails mid-mutation.

| Layer | Style |
|-------|--------|
| `GatewayError` / `ProviderError` | Typed `thiserror` |
| Admin stores | `Result<T, String>` |
| Admin handlers facade | `Option` / `bool` (errors logged, then discarded) |

**Suggestion:** Introduce `AdminError` / `StoreError` with variants (`NotFound`, `Conflict`, `Db`, `Validation`) and map to HTTP once. Keep logging, but stop collapsing all failures into empty/`false`.

---

### 16. Stringly-typed provider / model identity

> **Status (2026-07-10):** OPEN (deferred) — mitigated by the shared registry + drift-guard test

`provider_type: String` everywhere with `match` arms. Typos (“open-ai”) silently become generic clients or skips.

**Suggestion:** `ProviderKind` enum + `Unknown(String)` for custom base-URL vendors; validate at API boundary.

---

## P3 — Frontend (`web/`) quality

| Issue | Suggestion |
|-------|------------|
| Large client pages (e.g. models) mix layout, forms, tables | Extract hooks (`useModels`) + presentational components |
| Master key in `localStorage` | Session cookie / short-lived token (see P0) |
| Hardcoded provider list | **FIXED (2026-07-10)** — fetched from `GET /admin/known-providers`, hardcoded list kept as fallback |
| `prompt()` for log retention UI | Proper dialog with validation |
| Error handling is raw `Error` strings | Typed API errors + toast/error boundary consistency |
| No obvious E2E for dashboard | Playwright smoke tests for login → models → keys |

---

## P4 — Ops, tooling, polish

### 17. Dockerfile uses `rust:1.75-slim`

> **Status (2026-07-10):** FIXED — pinned `rust:1.96-slim` with BuildKit cache mounts for deps

May lag workspace edition/features and security patches. Pin a current stable (or `rust-toolchain.toml`) and multi-stage cache deps for faster CI builds.

### 18. Clippy findings (current)

> **Status (2026-07-10):** FIXED — workspace clippy is clean (0 warnings)

Two `clippy::field_reassign_with_default` warnings in `gateway.rs` (test helpers ~1788, ~1871). Easy cleanup; keep CI with `-D warnings` once clean.

### 19. SSRF guard is hostname/IP-literal only

> **Status (2026-07-10):** OPEN (documented limitation in `net_guard.rs`)

Documented in `net_guard.rs`: DNS rebinding to private IPs is not blocked. For high-trust multi-tenant admin, resolve and re-check, or use a pinned egress proxy.

### 20. Metrics registry uses `.unwrap()`

> **Status (2026-07-10):** FIXED — messaged `expect`s; registry is per-instance so duplicate registration cannot occur

Acceptable at startup if metrics are fixed, but panics on duplicate registration during hot-reload tests. Prefer `expect` with a message or handle “already registered.”

### 21. Docs drift

> **Status (2026-07-10):** FIXED — AGENTS.md auth note corrected (also removed stray markup at EOF), Bedrock references removed from ARCHITECTURE.md/AGENTS.md

- `AGENTS.md`: “himadri-auth not currently wired” — it **is** wired via `combined_auth` + `JWT_ISSUER`.  
- `ARCHITECTURE.md` still mentions Bedrock in places while Bedrock was removed in R11.

Keep agent/docs in sync with code to avoid wrong “quality” assumptions.

### 22. Streaming vs non-streaming asymmetry

> **Status (2026-07-10):** OPEN (deferred) — documentation/alignment task

Non-stream path: cache, after-plugins, full audit with usage. Stream path: audit “start only,” usage via `StreamUsageRecorder`, limited after-plugin story. Document and/or align so operators don’t assume identical accounting/guardrail behavior.

---

## Suggested prioritization

| Priority | Work | Impact |
|----------|------|--------|
| **Now** | Redact keys on write; fix decrypt-on-update wipe | Credential safety |
| **Now** | Fix `dedup_targets` + config reassert enabled-only | Failover & uptime |
| **Soon** | Atomic `provider_keys` rebuild; don’t clear empty keys silently | Transient 401s |
| **Soon** | Surface store errors as 500; align `/v1/models` with routing | Operability |
| **Next** | Split `gateway.rs`; shared crypto + vendor registry | Maintainability |
| **Next** | Typed admin errors; dual-backend parity tests | Long-term quality |
| **Ongoing** | Frontend auth storage; Dockerfile/toolchain; docs accuracy | Security & DX |

---

## Positive patterns to keep

- Explicit lock-order comments and dual-lock acquisition in `apply_config`
- Separation of secrets from `Target` (keys in `provider_keys`, not config JSON)
- Plugin pipeline + guardrail stages as extension points
- Shared SSE decoding and OpenAI-compatible preset model for vendors
- Prior large-scale cleanup (R1–R36) already removed many silent bugs (tool_calls drop, Gemini key-in-URL, unbounded in-memory stores, etc.)

