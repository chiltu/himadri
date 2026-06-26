#!/bin/bash
set -e

# E2E Test Script for Himadri Gateway
# Tests: Auth, API Keys, SQLite migration, idempotent startup
# Usage: ./tests/e2e_test.sh

GATEWAY_PORT=8081
DATABASE_URL="sqlite:./test_e2e.db"
MASTER_KEY="test-master-key-$$"
BASE_URL="http://localhost:$GATEWAY_PORT"

cleanup() {
    kill $(pgrep -f "himadri.*$GATEWAY_PORT") 2>/dev/null || true
    rm -f test_e2e.db
}
trap cleanup EXIT

echo "=== Building gateway ==="
cargo build --release 2>/dev/null

echo "=== Starting gateway ==="
DATABASE_URL="$DATABASE_URL" MASTER_KEY="$MASTER_KEY" ./target/release/himadri --port $GATEWAY_PORT &
sleep 2

PASS=0
FAIL=0

check() {
    local desc="$1" expected="$2" actual="$3"
    if [ "$expected" = "$actual" ]; then
        echo "  ✓ $desc"
        PASS=$((PASS + 1))
    else
        echo "  ✗ $desc (expected $expected, got $actual)"
        FAIL=$((FAIL + 1))
    fi
}

echo ""
echo "=== Test 1: SQLite database auto-creation ==="
if [ -f test_e2e.db ]; then
    check "Database file created" "yes" "yes"
else
    check "Database file created" "yes" "no"
fi

TABLES=$(sqlite3 test_e2e.db ".tables" 2>/dev/null | tr -d ' ')
check "api_keys table exists" "1" "$(echo $TABLES | grep -c api_keys)"
check "config_history table exists" "1" "$(echo $TABLES | grep -c config_history)"
check "request_logs table exists" "1" "$(echo $TABLES | grep -c request_logs)"
check "usage_records table exists" "1" "$(echo $TABLES | grep -c usage_records)"
check "_sqlx_migrations table exists" "1" "$(echo $TABLES | grep -c _sqlx_migrations)"

MIGRATION_VERSION=$(sqlite3 test_e2e.db "SELECT version FROM _sqlx_migrations;" 2>/dev/null)
check "Migration version is 1" "1" "$MIGRATION_VERSION"

echo ""
echo "=== Test 2: Authentication ==="
HTTP_CODE=$(curl -s -o /dev/null -w '%{http_code}' "$BASE_URL/admin/dashboard")
check "No auth returns 401" "401" "$HTTP_CODE"

HTTP_CODE=$(curl -s -o /dev/null -w '%{http_code}' -H "Authorization: Bearer wrong-key" "$BASE_URL/admin/dashboard")
check "Wrong key returns 401" "401" "$HTTP_CODE"

HTTP_CODE=$(curl -s -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $MASTER_KEY" "$BASE_URL/admin/dashboard")
check "Correct key returns 200" "200" "$HTTP_CODE"

echo ""
echo "=== Test 3: API Key CRUD ==="
# Create
KEY_RESPONSE=$(curl -s -X POST "$BASE_URL/admin/keys" \
    -H "Authorization: Bearer $MASTER_KEY" \
    -H "Content-Type: application/json" \
    -d '{"name":"e2e-test-key","scopes":["api","admin"]}')
KEY_ID=$(echo $KEY_RESPONSE | jq -r '.id')
KEY_VALUE=$(echo $KEY_RESPONSE | jq -r '.key')
check "Create key returns id" "true" "$([ -n "$KEY_ID" ] && [ "$KEY_ID" != "null" ] && echo true || echo false)"
check "Create key name matches" "e2e-test-key" "$(echo $KEY_RESPONSE | jq -r '.name')"
check "Create key has scopes" "2" "$(echo $KEY_RESPONSE | jq '.scopes | length')"
check "Create key enabled" "true" "$(echo $KEY_RESPONSE | jq -r '.enabled')"

# List
KEYS_COUNT=$(curl -s "$BASE_URL/admin/keys" -H "Authorization: Bearer $MASTER_KEY" | jq 'length')
check "List returns 1 key" "1" "$KEYS_COUNT"

# Get
GET_RESPONSE=$(curl -s "$BASE_URL/admin/keys/$KEY_ID" -H "Authorization: Bearer $MASTER_KEY")
check "Get key returns correct id" "$KEY_ID" "$(echo $GET_RESPONSE | jq -r '.id')"

# Rotate
ROTATE_RESPONSE=$(curl -s -X POST "$BASE_URL/admin/keys/$KEY_ID/rotate" -H "Authorization: Bearer $MASTER_KEY")
NEW_KEY=$(echo $ROTATE_RESPONSE | jq -r '.key')
check "Rotate generates new key" "true" "$([ "$KEY_VALUE" != "$NEW_KEY" ] && echo true || echo false)"

# Revoke
REVOKE_RESPONSE=$(curl -s -X POST "$BASE_URL/admin/keys/$KEY_ID/revoke" -H "Authorization: Bearer $MASTER_KEY")
check "Revoke disables key" "false" "$(echo $REVOKE_RESPONSE | jq -r '.enabled')"

# Delete
HTTP_CODE=$(curl -s -o /dev/null -w '%{http_code}' -X DELETE "$BASE_URL/admin/keys/$KEY_ID" -H "Authorization: Bearer $MASTER_KEY")
check "Delete returns 200" "200" "$HTTP_CODE"

KEYS_AFTER=$(curl -s "$BASE_URL/admin/keys" -H "Authorization: Bearer $MASTER_KEY" | jq 'length')
check "List shows 0 keys after delete" "0" "$KEYS_AFTER"

echo ""
echo "=== Test 4: Dashboard ==="
DASHBOARD=$(curl -s "$BASE_URL/admin/dashboard" -H "Authorization: Bearer $MASTER_KEY")
check "Dashboard has total_requests" "0" "$(echo $DASHBOARD | jq '.total_requests')"
check "Dashboard has total_tokens" "0" "$(echo $DASHBOARD | jq '.total_tokens')"

echo ""
echo "=== Test 5: Config ==="
CONFIG=$(curl -s "$BASE_URL/admin/config" -H "Authorization: Bearer $MASTER_KEY")
check "Config has strategy mode" "single" "$(echo $CONFIG | jq -r '.strategy.mode')"

# Update config
HTTP_CODE=$(curl -s -o /dev/null -w '%{http_code}' -X PUT "$BASE_URL/admin/config" \
    -H "Authorization: Bearer $MASTER_KEY" \
    -H "Content-Type: application/json" \
    -d '{"strategy":{"mode":"fallback","fallback_timeout_ms":30000},"targets":[],"rate_limit":{"enabled":false,"requests_per_second":100,"burst_size":200},"plugins":[],"observability":{"tracing":{"enabled":false,"endpoint":"","sample_ratio":1.0}},"admin":{}}')
check "Update config returns 200" "200" "$HTTP_CODE"

NEW_CONFIG=$(curl -s "$BASE_URL/admin/config" -H "Authorization: Bearer $MASTER_KEY")
check "Config updated to fallback" "fallback" "$(echo $NEW_CONFIG | jq -r '.strategy.mode')"

echo ""
echo "=== Test 6: Reload ==="
RELOAD=$(curl -s -X POST "$BASE_URL/admin/reload" -H "Authorization: Bearer $MASTER_KEY")
check "Reload returns success" "reloaded" "$(echo $RELOAD | jq -r '.status')"

echo ""
echo "=== Test 7: Request Logs ==="
LOGS=$(curl -s "$BASE_URL/admin/logs" -H "Authorization: Bearer $MASTER_KEY")
check "Logs returns data field" "0" "$(echo $LOGS | jq '.total')"

echo ""
echo "=== Test 8: Usage ==="
USAGE=$(curl -s "$BASE_URL/admin/usage" -H "Authorization: Bearer $MASTER_KEY")
check "Usage returns total_requests" "0" "$(echo $USAGE | jq '.total_requests')"

echo ""
echo "=== Test 9: Health ==="
HEALTH=$(curl -s "$BASE_URL/health")
check "Health status ok" "ok" "$(echo $HEALTH | jq -r '.status')"

echo ""
echo "=== Test 10: Idempotent startup ==="
# Kill and restart with same database
kill $(pgrep -f "himadri.*$GATEWAY_PORT") 2>/dev/null || true
sleep 1

DATABASE_URL="$DATABASE_URL" MASTER_KEY="$MASTER_KEY" ./target/release/himadri --port $GATEWAY_PORT &
sleep 2

# Verify migration still version 1 (not re-run)
MIGRATION_COUNT=$(sqlite3 test_e2e.db "SELECT COUNT(*) FROM _sqlx_migrations;" 2>/dev/null)
check "Migration not re-run" "1" "$MIGRATION_COUNT"

# Verify data persisted
HTTP_CODE=$(curl -s -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $MASTER_KEY" "$BASE_URL/admin/dashboard")
check "Data persisted after restart" "200" "$HTTP_CODE"

echo ""
echo "================================"
echo "Results: $PASS passed, $FAIL failed"
echo "================================"

[ $FAIL -eq 0 ] && exit 0 || exit 1
