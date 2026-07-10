# Code Review Findings — 2026-07-10

High-effort code review of working-tree changes: 6235 lines across 29 files (Rust backends + Next.js web).

**Status:** 10 findings verified (6 CONFIRMED, 4 PLAUSIBLE). **Priority:** 2 CRITICAL (security/data loss), 4 HIGH (availability/correctness), 4 MEDIUM (maintainability/consistency).

**Resolution (2026-07-10):** 9 of 10 findings fixed with regression tests; workspace tests pass and clippy is clean.

| Finding | Resolution |
|---|---|
| API key leak in POST handlers | Fixed — write responses go through `redact_endpoint()` |
| Decrypt failure destroys credentials | Fixed — `update()` never rewrites the key column from a decrypted read; `api_key: None` leaves it alone |
| Config reload wipes targets | Fixed — guarded by `Gateway::db_has_active_targets` (also at startup in `main.rs`) |
| `dedup_targets` collapses endpoints | Fixed — `target.id` added to the dedup key |
| Rebuild race sends empty API keys | Fixed — insert-then-retain repopulation of `provider_keys`; no empty window |
| `/v1/models` advertises unreachable models | Fixed — shared `himadri_core::endpoint_is_routable` predicate + drift-guard test against `build_provider_client` |
| `delete_endpoint` swallows DB errors | Fixed — returns `Result`, HTTP layer maps store errors instead of 404 |
| `delete_model_endpoint` inconsistency | Fixed — uses the same error-aware `deleted()` helper as `delete_model` |
| Encryption duplicated across stores | Fixed — shared helpers in `crypto.rs`, both stores delegate |
| Vendor registry in 3 places | Fixed — `himadri_core::KNOWN_PROVIDER_TYPES` + `GET /admin/known-providers`; web UI fetches it |
| Postgres/SQLite cascade divergence | Fixed — dual-backend contract suite in `himadri-admin/src/store_parity_tests.rs` asserts the cascade outcome on both backends and that the Postgres `ON DELETE CASCADE` FK exists |

---

## P0 — Critical Security & Data Loss Issues

### CONFIRMED: API Key Leak in POST Endpoint Handlers
**File:** `crates/himadri/src/handlers.rs:791`  
**Severity:** CRITICAL (credential exposure)

The `toggle_model_endpoint`, `update_model_endpoint`, and `create_model_endpoint` handlers return the full `ModelEndpoint` struct without calling `redact_endpoint()`, which leaks the decrypted provider API key to the client.

**Failure scenario:**  
```
POST /admin/endpoints/{id}/toggle
Body: {"enabled": false}

Response includes:
{
  "id": "ep-123",
  "api_key": "sk-top-secret-key-here",  // ← LEAKED
  "provider_type": "openai"
}
```

The GET/list handlers correctly redact this field, but write operations do not.

**Fix:** Call `redact_endpoint()` on the response before returning, matching GET behavior. Verify with a test that POST responses never contain `api_key` field.

---

### CONFIRMED: Decrypt Failure Destroys Stored Credentials Permanently
**File:** `crates/himadri-admin/src/provider_store.rs:250`  
**Severity:** CRITICAL (data loss)

The `update()` method retrieves the current endpoint via `get()`, whose `decrypt_endpoint()` sets `api_key=None` when decryption fails (line 158). If an update omits the `api_key` field, the method uses `current.api_key = None`, then encrypts `None` and writes `NULL` over the original ciphertext in the database.

**Failure scenario:**
1. `PROVIDER_ENCRYPTION_KEY` is misconfigured or rotated
2. `decrypt_endpoint()` fails (logs error, returns endpoint with `api_key=None`)
3. Operator calls `PUT /admin/endpoints/{id}` with `{"weight": 2}` (no `api_key` field)
4. Update logic: `api_key = current.api_key = None`
5. Credentials permanently destroyed; even restoring the correct encryption key cannot recover them

**Fix:** 
- On decryption failure, either return an error to the client or preserve the encrypted value unchanged
- Add a test: misconfigure `PROVIDER_ENCRYPTION_KEY`, attempt an update that omits `api_key`, verify the encrypted credential is not modified

---

## P1 — High-Priority Availability & Correctness Issues

### CONFIRMED: Config Reload Wipes All Routing Targets When DB Endpoints Disabled
**File:** `crates/himadri/src/handlers.rs:581`  
**Severity:** HIGH (availability outage)

The `reassert_db_targets_after_config` function guards on `endpoints.is_empty()`, but `list_endpoints()` returns all endpoints including disabled ones. When `rebuild_targets_from_db()` is called, it processes only enabled endpoints. If all endpoints happen to be disabled, the rebuilt target list is empty, overwriting config-file targets that were explicitly set to preserve them.

**Failure scenario:**
1. Deployment routes traffic via config-file targets: `targets: [openai]`
2. Administrator creates a DB endpoint but then disables it (or was testing)
3. Admin saves the gateway config via the dashboard
4. `reassert_db_targets_after_config` guards on `endpoints.is_empty()` → false (disabled endpoint exists)
5. Calls `rebuild_targets_from_db()` which skips disabled endpoints → produces zero targets
6. Config targets `[openai]` are overwritten with `[]`
7. Every `/v1/chat/completions` request returns 404 "No provider is configured"

Same outage occurs on gateway restart via `main.rs:123`.

**Fix:** Guard should check `endpoints.iter().filter(|e| e.enabled).count() > 0`, or better: merge DB targets with config targets instead of replacing them entirely.

---

### CONFIRMED: dedup_targets Collapses Multiple Endpoints, Breaking Failover
**File:** `crates/himadri/src/strategy.rs:305`  
**Severity:** HIGH (failover broken)

The `dedup_targets()` function keys on `(provider, api_key_env, base_url)`, missing the new `target.id` field. Two DB endpoints of the same provider type with both `base_url=None` (preset) but different credentials and weights are collapsed into one, dropping the sibling from the failover order.

**Failure scenario:**
1. Model `gpt-4o` has two enabled OpenAI endpoints:
   - `ep1`: `api_key: "sk-proj-key1"`, `base_url: null`, weight 5.0
   - `ep2`: `api_key: "sk-proj-key2"`, `base_url: null`, weight 2.0
2. `rebuild_targets_from_db()` creates two targets with different `id` and credentials
3. `select_ordered("gpt-4o")` returns both targets
4. `select_ordered()` calls `dedup_targets()` which keys on:
   - `(provider="openai", api_key_env=None, base_url=None)` for both
5. Dedup sees them as identical and filters out `ep2`
6. When `ep1`'s API key hits rate limit or fails, no failover to `ep2` is attempted
7. Requests fail despite a healthy backup endpoint existing

**Fix:** Use `target.id` as part of the dedup key: `(id, provider, api_key_env, base_url)`. Verify with a test: two endpoints of the same provider with same base_url but different api_keys must both be present in the deduplicated list.

---

### PLAUSIBLE: Rebuild Targets Race Condition Sends Empty API Keys
**File:** `crates/himadri/src/gateway.rs:836`  
**Severity:** HIGH (transient failures, false circuit-breaker trips)

The `rebuild_targets_from_db()` function clears the entire `provider_keys` map before repopulating it, and before swapping the targets. Concurrent in-flight requests that call `get_api_key()` between `clear()` and the re-insert window get `Ok("")` (empty string), sending requests upstream with an empty Bearer token.

**Failure scenario:**
1. Admin toggles an endpoint → `rebuild_targets_from_db()` runs
2. Concurrent chat request is already past `select_targets()`, holding a still-valid target
3. Request calls `get_api_key()` between `provider_keys.clear()` and re-insert
4. `get_api_key()` returns `Ok("")` (empty string)
5. Request sent with `Authorization: Bearer ` (empty)
6. Provider returns 401 Unauthorized
7. Circuit breaker records failure against a healthy endpoint, reducing availability

**Fix:** Hold the old `provider_keys` reference until new one is fully populated, then swap atomically. Or use read-write lock and hold write lock only during the critical swap, allowing readers to proceed with old keys during repopulation.

---

### CONFIRMED: /v1/models Advertises Unreachable Models
**File:** `crates/himadri-admin/src/handlers.rs:162`  
**Severity:** HIGH (API contract violation)

The `list_enabled_models_for_api` treats any enabled endpoint as making a model active in `/v1/models`. However, `rebuild_targets_from_db()` skips endpoints whose `provider_type` is unknown and `base_url` is empty. This causes a model to advertise in the list but fail every completion request.

**Failure scenario:**
1. Create model `my-llm` with endpoint `{provider_type: "my-vendor", base_url: null}`
2. GET `/v1/models` lists `my-llm` (endpoint is enabled)
3. POST `/v1/chat/completions` with `model: "my-llm"` → 404 "No provider is configured to serve model"
4. Root cause: rebuild_targets_from_db skipped the endpoint because `my-vendor` is unknown and no base_url provided

**Fix:** Sync the two checks: either both skip unknown-type + empty-base_url endpoints, or both include them. Recommend: require `provider_type` to be in a whitelist of known vendors, or require `base_url` when using an unknown vendor.

---

## P2 — High-Priority Error Handling & Consistency Issues

### CONFIRMED: delete_endpoint Silently Swallows Database Errors
**File:** `crates/himadri-admin/src/handlers.rs:246`  
**Severity:** HIGH (silent failures, error reporting)

The `delete_endpoint` handler calls `.unwrap_or(false)`, mapping both successful "not found" and database errors to false, which returns HTTP 404 in both cases.

**Failure scenario:**
1. Database corruption or connection error occurs during `DELETE FROM model_endpoints`
2. `state.admin.delete_endpoint()` returns an error
3. Handler converts error to `false` via `unwrap_or(false)`
4. HTTP response: 404 "not found"
5. Client believes deletion succeeded when it actually failed silently

**Fix:** Return 500 on database error. Use the error-aware `deleted()` helper (already used by `delete_model`) to surface actual errors instead of silently converting them to 404.

---

### CONFIRMED: delete_model_endpoint Inconsistent with delete_model Error Handling
**File:** `crates/himadri/src/handlers.rs:784`  
**Severity:** MEDIUM (inconsistent error API)

The `delete_model_endpoint` handler calls `.unwrap_or(false)` (line 784), while `delete_model` (line 766) properly uses the error-aware `deleted()` helper. This inconsistency means:
- `delete_model` returns `Result<bool, String>` with error details
- `delete_model_endpoint` returns just `bool`

Future validation guards added to `ModelEndpointStore.delete()` would surface in `delete_model` but be silently lost in `delete_model_endpoint`.

**Fix:** Use the same `deleted()` error wrapper in both handlers to ensure consistent behavior.

---

## P3 — Structural Deduplication & Maintainability Issues

### CONFIRMED: Encryption Logic Duplicated Across Postgres and SQLite Stores
**File:** `crates/himadri-admin/src/postgres_provider_store.rs:152`  
**File:** `crates/himadri-admin/src/provider_store.rs:143`  
**Severity:** MEDIUM (maintainability, divergence risk)

The `encrypt_api_key()` and `decrypt_endpoint()` methods are verbatim copies between `PgModelEndpointStore` and `ModelEndpointStore`. The encrypt-if-nonempty rule and the decrypt-failure policy (log + set `api_key=None`) now live in two files that must be edited in lockstep.

**Failure scenario:**
1. Bug found and fixed in `provider_store.rs` decrypt logic
2. `postgres_provider_store.rs` version is forgotten
3. Postgres and SQLite endpoints diverge in behavior
4. Operator debugging intermittent failures doesn't realize backend-specific behavior is the cause

**Fix:** Extract into a shared `crates/himadri-admin/src/crypto.rs` module with public helpers:
```rust
pub fn encrypt_api_key(key: &CipherKey, api_key: Option<&str>) -> Result<Option<String>>;
pub fn decrypt_api_key(key: &CipherKey, encrypted: Option<&str>) -> Result<Option<String>>;
```

Both stores call these shared functions.

---

### PLAUSIBLE: Vendor Preset Registry Duplicated in 3 Places
**File:** `crates/himadri/src/gateway.rs:72`  
**File:** `crates/himadri/src/main.rs:266-298`  
**File:** `web/app/dashboard/models/page.tsx:51`  
**Severity:** MEDIUM (divergence risk)

The vendor preset list (openai, openrouter, together, groq, fireworks, deepinfra, cerebras, novita) is hardcoded in three independent locations:
- `build_provider_client()` (gateway.rs)
- `register_providers_from_env()` (main.rs)
- `KNOWN_PROVIDER_TYPES` (web UI)

No shared source of truth. Adding or renaming a vendor requires editing three places.

**Failure scenario:**
1. New vendor added to gateway.rs and main.rs
2. Web UI KNOWN_PROVIDER_TYPES forgotten
3. User cannot select new vendor from UI autocomplete
4. Or user selects vendor, told "no base URL required", but gateway skips it as unknown
5. Model routing fails with no clear error message

**Fix:** Expose `/admin/known-providers` endpoint that returns the authoritative list. Web UI and Rust CLI fetch it at runtime instead of hardcoding.

---

### PLAUSIBLE: Postgres/SQLite Model Cascade Strategies Diverge
**File:** `crates/himadri-admin/src/postgres_provider_store.rs:692`  
**File:** `crates/himadri-admin/src/provider_store.rs:109`  
**Severity:** MEDIUM (asymmetric behavior risk)

Model deletion uses different cascade strategies:
- Postgres: Relies on `ON DELETE CASCADE` foreign key (database enforces it)
- SQLite: Explicitly deletes from `model_endpoints` before deleting from `models` (application enforces it)

**Failure scenario:**
1. Postgres FK constraint is accidentally dropped or misconfigured
2. Model deleted → `model_endpoints` rows left orphaned
3. SQLite behavior still cascades correctly (code logic works)
4. Operator debugging only sees the problem in Postgres, not SQLite
5. Data inconsistency between backends undetected

**Fix:** Both backends should explicitly cascade in application code, or both should rely on database FK. Recommend: ensure both use `ON DELETE CASCADE` FK and add a migration validator that confirms the constraint exists.

---

## Summary & Recommended Fix Order

1. **Immediate (security):**
   - API key leak in POST handlers (handlers.rs:791)
   - Decrypt destroying credentials (provider_store.rs:250)

2. **Urgent (availability):**
   - Config reload wiping targets (handlers.rs:581)
   - dedup_targets collision (strategy.rs:305)
   - Rebuild race condition (gateway.rs:836)
   - /v1/models advertising unavailable models (handlers.rs:162)

3. **High (error handling):**
   - delete_endpoint swallowing errors (handlers.rs:246)
   - delete_model_endpoint inconsistency (handlers.rs:784)

4. **Medium (maintainability):**
   - Encryption logic duplication (crypto.rs extraction)
   - Vendor registry duplication (expose /admin/known-providers)
   - Cascade strategy divergence (align or test)

---

## Testing Notes

Add regression tests for each finding:
- POST endpoint handlers: verify no `api_key` in response
- Decrypt on key rotation: update endpoint while key misconfigured, verify ciphertext unchanged
- Config reload: disable all endpoints, save config, verify targets preserved
- Dedup: two same-provider endpoints with different credentials both present after dedup
- Rebuild race: concurrent get_api_key calls during rebuild return valid keys (not empty)
- /v1/models consistency: enabled endpoint without valid provider_type is not listed
- Error codes: delete errors return 500, not 404

