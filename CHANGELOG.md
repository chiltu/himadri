# Changelog

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
