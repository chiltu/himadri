# End-to-End: Running & Manually Verifying All Features

This document shows how to run the himadri gateway and manually verify every
feature, using the `load-test-sink` crate as a mock LLM provider and (optionally)
a real **Zitadel** container for JWT/OIDC authentication.

There are two paths:

- **Quick path** (Section 1–3): no Docker, master-key auth — covers chat,
  streaming, cache, embeddings, tool calling, admin config history/rollback,
  metrics, and provider failover.
- **Full path** (Section 4): adds real Zitadel JWT/OIDC validation.

---

## 0. Prerequisites & build

```bash
cd /path/to/ai-gateway-rust
cargo build -p himadri -p load-test-sink   # -> target/debug/himadri, target/debug/load_test_sink
```

Notes:
- Binary names use underscores: `himadri`, `load_test_sink`.
- The gateway binary accepts `--port <PORT>` (overrides the `PORT` env var used
  in the examples below), `--migrate` (migrate `DATABASE_URL` to the latest
  schema before starting), and `--help`.
- The default gateway config has a single `openai` target with
  `api_key_env: OPENAI_API_KEY`, so **`OPENAI_API_KEY` must be set** (any value;
  the sink ignores it) or requests fail with `ServiceUnavailable`.
- The Zitadel section requires `docker`, `curl`, and `jq`.

> Sandbox caveat: the examples use `&` to background the servers for brevity. In
> some sandboxed shells a backgrounded process is killed when the shell returns;
> if so, run each server in its own terminal (or via a process supervisor).
> Likewise, Docker here may require `--network host` (used in Section 4).

---

## 1. Quick path — start sink + gateway (master-key auth)

```bash
# Mock LLM provider: chat + embeddings + tool-call echo.
# SINK_STREAM=false => non-streaming responses unless the request sets "stream":true.
SINK_PORT=8081 SINK_STREAM=false SINK_RESPONSE="Bonjour from the sink" \
  ./target/debug/load_test_sink &

# Gateway: openai provider -> sink, response cache on, admin master key.
OPENAI_BASE_URL=http://localhost:8081/v1 \
OPENAI_API_KEY=dummy-key \
CACHE_TTL_SECS=60 \
MASTER_KEY=e2e-master-key \
PORT=9000 \
./target/debug/himadri &

curl -s localhost:9000/health    # -> {"status":"ok"}
```

Shared shell vars for the commands below:

```bash
AUTH='Authorization: Bearer e2e-master-key'
J='Content-Type: application/json'
```

---

## 2. Per-feature manual verification

### Chat completion
```bash
curl -s -X POST localhost:9000/v1/chat/completions -H "$AUTH" -H "$J" \
  -d '{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}' \
  | jq '.choices[0].message.content'
# -> "Bonjour from the sink"
```

### Streaming (SSE)
```bash
curl -sN -X POST localhost:9000/v1/chat/completions -H "$AUTH" -H "$J" \
  -d '{"model":"gpt-4o","stream":true,"messages":[{"role":"user","content":"hi"}]}'
# -> a series of `data: {...delta...}` chunks ending with "finish_reason":"stop"
```

### Response cache
Two identical non-streaming requests return the **same `id`** (2nd is served from
cache); a different prompt produces a fresh `id`.
```bash
REQ='{"model":"gpt-4o","temperature":0.1,"messages":[{"role":"user","content":"cache me"}]}'
curl -s -X POST localhost:9000/v1/chat/completions -H "$AUTH" -H "$J" -d "$REQ" | jq -r .id
curl -s -X POST localhost:9000/v1/chat/completions -H "$AUTH" -H "$J" -d "$REQ" | jq -r .id   # same id
curl -s localhost:9000/metrics | grep himadri_cache_hits_total                                 # counter > 0
```

### Embeddings
```bash
curl -s -X POST localhost:9000/v1/embeddings -H "$AUTH" -H "$J" \
  -d '{"model":"text-embedding-3-small","input":["a","b","c"]}' | jq '.data | length'   # -> 3
```

### Tool calling
The gateway forwards `tools`/`tool_choice`; the sink echoes a tool call; the
gateway surfaces it in the response.
```bash
curl -s -X POST localhost:9000/v1/chat/completions -H "$AUTH" -H "$J" \
  -d '{"model":"gpt-4o","messages":[{"role":"user","content":"weather?"}],
       "tools":[{"type":"function","function":{"name":"get_weather","parameters":{"type":"object"}}}],
       "tool_choice":"auto"}' \
  | jq '.choices[0].message.tool_calls[0].function.name'
# -> "get_weather"
```

### Admin: config history + rollback
```bash
curl -s localhost:9000/admin/config/history -H "$AUTH" | jq '.summary.total_versions'   # initial: 1

# Update config (bump strategy timeout) -> creates a new version
CUR=$(curl -s localhost:9000/admin/config -H "$AUTH")
echo "$CUR" | jq '.strategy.fallback_timeout_ms=4242' \
  | curl -s -X PUT localhost:9000/admin/config -H "$AUTH" -H "$J" -d @-
curl -s localhost:9000/admin/config -H "$AUTH" | jq '.strategy.fallback_timeout_ms'      # -> 4242

# Roll back to version 1
curl -s -X POST localhost:9000/admin/config/rollback/1 -H "$AUTH"
curl -s localhost:9000/admin/config -H "$AUTH" | jq '.strategy.fallback_timeout_ms'      # -> 30000
```

### Auth enforcement
```bash
curl -s -o /dev/null -w '%{http_code}\n' localhost:9000/admin/config                      # -> 401 (no key)
curl -s -o /dev/null -w '%{http_code}\n' -X POST localhost:9000/v1/chat/completions \
  -H "$J" -d '{"model":"gpt-4o","messages":[]}'                                            # -> 401
```

### Metrics
```bash
curl -s localhost:9000/metrics | grep -E 'himadri_(requests_total|cache_hits_total|cache_misses_total)'
```

---

## 3. Provider failover (two sinks)

Failover triggers only on **retryable** upstream errors: `429` and `502/503/529`.
Non-retryable errors (auth failures, `404`, connection refused, etc.) are
returned without falling back.

```bash
# Healthy sink + an always-503 sink
SINK_PORT=8081 SINK_STREAM=false SINK_RESPONSE="from HEALTHY" ./target/debug/load_test_sink &
SINK_PORT=8082 SINK_STATUS=503                                ./target/debug/load_test_sink &

# Two targets, fallback strategy
cat > /tmp/fb.json <<'JSON'
{ "strategy": { "mode": "fallback" },
  "targets": [ { "provider": "openai" }, { "provider": "openai-secondary" } ] }
JSON

# Primary "openai" -> 503 sink ; secondary "openai-secondary" -> healthy sink
GATEWAY_CONFIG=/tmp/fb.json \
OPENAI_BASE_URL=http://localhost:8082/v1 \
OPENAI_SECONDARY_BASE_URL=http://localhost:8081/v1 \
MASTER_KEY=e2e-master-key PORT=9000 \
RUST_LOG=info,himadri::gateway=debug \
./target/debug/himadri &

curl -s -X POST localhost:9000/v1/chat/completions \
  -H 'Authorization: Bearer e2e-master-key' -H 'Content-Type: application/json' \
  -d '{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}' \
  | jq -r '.choices[0].message.content'
# -> "from HEALTHY"  (503 was retryable -> fell back)
# Gateway log shows: "Provider openai failed with retryable error, falling back: provider error (503): ..."
```

Negative case: start the first sink with `SINK_STATUS=401` instead — the request
returns the error (HTTP 500 with the upstream message) and does **not** fall back.

---

## 4. Full path — real Zitadel JWT/OIDC

### 4.1 Start Postgres + Zitadel

```bash
WD=/tmp/e2e; mkdir -p $WD; chmod 777 $WD

# Postgres (Zitadel's datastore)
sudo docker run -d --name e2e-pg --network host \
  -e POSTGRES_USER=postgres -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=zitadel \
  postgres:16-alpine

# Zitadel init steps: org + machine user 'e2e-admin' + a PAT written to /e2e/pat.txt
cat > $WD/steps.yaml <<'YAML'
FirstInstance:
  PatPath: /e2e/pat.txt
  Org:
    Name: e2e-org
    Human: { UserName: zitadel-admin, Password: Password1! }
    Machine:
      Machine: { Username: e2e-admin, Name: e2e-admin }
      Pat: { ExpirationDate: 2099-01-01T00:00:00Z }
YAML

sudo docker run -d --name e2e-zitadel --network host -v $WD:/e2e \
  -e ZITADEL_DATABASE_POSTGRES_HOST=localhost -e ZITADEL_DATABASE_POSTGRES_PORT=5432 \
  -e ZITADEL_DATABASE_POSTGRES_DATABASE=zitadel \
  -e ZITADEL_DATABASE_POSTGRES_USER_USERNAME=postgres -e ZITADEL_DATABASE_POSTGRES_USER_PASSWORD=postgres -e ZITADEL_DATABASE_POSTGRES_USER_SSL_MODE=disable \
  -e ZITADEL_DATABASE_POSTGRES_ADMIN_USERNAME=postgres -e ZITADEL_DATABASE_POSTGRES_ADMIN_PASSWORD=postgres -e ZITADEL_DATABASE_POSTGRES_ADMIN_SSL_MODE=disable \
  -e ZITADEL_EXTERNALSECURE=false -e ZITADEL_EXTERNALDOMAIN=localhost -e ZITADEL_EXTERNALPORT=8080 -e ZITADEL_TLS_ENABLED=false \
  ghcr.io/zitadel/zitadel:latest \
  start-from-init --masterkey "MasterkeyNeedsToHave32Characters" --tlsMode disabled --steps /e2e/steps.yaml

# Wait until ready
until [ "$(curl -s -o /dev/null -w '%{http_code}' localhost:8080/debug/healthz)" = 200 ]; do sleep 2; done
```

### 4.2 Mint an RS256 JWT

Zitadel **PATs are JWE** (encrypted; introspection-only) and cannot be validated
by the gateway's JWKS path. Create a machine user with
`accessTokenType: ACCESS_TOKEN_TYPE_JWT`, give it a client secret, and use the
`client_credentials` grant to obtain a signed **RS256 JWT** whose `aud` is the
project id.

```bash
PAT=$(cat $WD/pat.txt)
H_PAT="Authorization: Bearer $PAT"; H_J="Content-Type: application/json"

PROJECT_ID=$(curl -s -H "$H_PAT" -H "$H_J" -d '{"name":"e2e-project"}' \
  localhost:8080/management/v1/projects | jq -r .id)

SVC=$(curl -s -H "$H_PAT" -H "$H_J" \
  -d '{"userName":"e2e-svc","name":"e2e-svc","accessTokenType":"ACCESS_TOKEN_TYPE_JWT"}' \
  localhost:8080/management/v1/users/machine | jq -r .userId)

SEC=$(curl -s -X PUT -H "$H_PAT" -H "$H_J" localhost:8080/management/v1/users/$SVC/secret -d '{}')
CID=$(echo "$SEC" | jq -r .clientId); CSECRET=$(echo "$SEC" | jq -r .clientSecret)

JWT=$(curl -s -X POST localhost:8080/oauth/v2/token \
  --data-urlencode grant_type=client_credentials \
  --data-urlencode client_id=$CID --data-urlencode client_secret=$CSECRET \
  --data-urlencode "scope=openid urn:zitadel:iam:org:project:id:${PROJECT_ID}:aud" \
  | jq -r .access_token)

# Sanity: should be a 3-segment RS256 JWS, with iss=http://localhost:8080 and aud=[PROJECT_ID]
echo "$JWT" | cut -d. -f1 | base64 -d 2>/dev/null | jq .       # {"alg":"RS256",...}
echo "$JWT" | cut -d. -f2 | base64 -d 2>/dev/null | jq '{iss,aud}'
echo "PROJECT_ID (use as JWT_AUDIENCE) = $PROJECT_ID"
```

### 4.3 Run the gateway with JWT validation

```bash
SINK_PORT=8081 SINK_STREAM=false SINK_RESPONSE="Bonjour" ./target/debug/load_test_sink &

OPENAI_BASE_URL=http://localhost:8081/v1 OPENAI_API_KEY=dummy CACHE_TTL_SECS=60 \
MASTER_KEY=e2e-master-key \
JWT_ISSUER=http://localhost:8080 JWT_AUDIENCE=$PROJECT_ID \
PORT=9000 ./target/debug/himadri &

# Valid Zitadel JWT -> 200
curl -s -o /dev/null -w '%{http_code}\n' -X POST localhost:9000/v1/chat/completions \
  -H "Authorization: Bearer $JWT" -H 'Content-Type: application/json' \
  -d '{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}'        # -> 200

# Missing / garbage token -> 401
curl -s -o /dev/null -w '%{http_code}\n' -X POST localhost:9000/v1/chat/completions \
  -H 'Authorization: Bearer not.a.jwt' -H 'Content-Type: application/json' \
  -d '{"model":"gpt-4o","messages":[]}'                                       # -> 401
```

Both a Zitadel JWT and the master key work on `/v1` (CombinedAuth tries JWT
first, then falls back to API-key/master-key). The JWT's `sub` flows into the
audit log as `user_id`/`key_id`.

---

## 5. Automated tests & cleanup

```bash
# Unit + integration tests (incl. failover, cache, embeddings, tools, auth, config history)
cargo test --workspace

# Stop servers and containers
pkill -x himadri; pkill -x load_test_sink
sudo docker rm -f e2e-zitadel e2e-pg
```

### Live OpenRouter integration test

`crates/himadri-provider/tests/openrouter_live.rs` exercises the real
`https://openrouter.ai/api/v1` endpoint (chat completion + streaming). It is
**gated on `OPENROUTER_API_KEY`** — without the key it skips and passes, so the
default `cargo test --workspace` run and CI stay green. Upstream `429`s on free
models are treated as a skip, not a failure.

```bash
# Skipped by default:
cargo test -p himadri-provider --test openrouter_live -- --nocapture

# Run live (free model by default; override with OPENROUTER_TEST_MODEL):
OPENROUTER_API_KEY=sk-or-... \
  cargo test -p himadri-provider --test openrouter_live -- --nocapture --test-threads=1
```

---

## Environment variable reference

| Variable | Purpose |
|---|---|
| `OPENAI_BASE_URL` | Base URL for the primary `openai` provider |
| `OPENAI_SECONDARY_BASE_URL` | Base URL for a secondary `openai-secondary` provider (multi-endpoint / failover) |
| `OPENAI_API_KEY` | Required by the default config's `openai` target |
| `CACHE_TTL_SECS`, `CACHE_MAX_ENTRIES` | Enable/size the response cache |
| `MASTER_KEY` | Admin auth and `/v1` bearer |
| `JWT_ISSUER` | OIDC issuer (e.g. `http://localhost:8080`) |
| `JWT_AUDIENCE` | Expected `aud` (the Zitadel project id) |
| `JWT_JWKS_URI` | Explicit JWKS URL (bypasses OIDC discovery) |
| `JWT_JWKS_REFRESH_SECS` | JWKS refresh interval (default 3600) |
| `GATEWAY_CONFIG` | Path to a JSON config (targets + strategy) |
| `PORT` | Gateway listen port |
| `DATABASE_URL` | When it starts with `postgres`, persists request logs (requires the `postgres` build feature) |
| `SINK_PORT`, `SINK_STREAM`, `SINK_STATUS`, `SINK_RESPONSE`, `SINK_LATENCY_MS` | Mock sink behavior (`SINK_STATUS` simulates an unhealthy upstream, e.g. `503`/`401`) |

## Endpoint reference

| Method & path | Auth | Purpose |
|---|---|---|
| `GET /health` | none | Liveness |
| `GET /metrics` | none | Prometheus metrics |
| `GET /v1/models` | none | Model list |
| `POST /v1/chat/completions` | JWT or API key | Chat (supports `stream`, `tools`) |
| `POST /v1/completions` | JWT or API key | Chat-style completion |
| `POST /v1/embeddings` | JWT or API key | Embeddings |
| `GET/PUT /admin/config` | master/admin key | Get / replace config |
| `GET /admin/config/history` | master/admin key | Config version history |
| `POST /admin/config/rollback/{version}` | master/admin key | Roll back to a version |
| `GET/DELETE /admin/logs`, `/admin/keys`, `/admin/providers`, `/admin/models`, `/admin/dashboard`, `/admin/usage` | master/admin key | Admin management |
