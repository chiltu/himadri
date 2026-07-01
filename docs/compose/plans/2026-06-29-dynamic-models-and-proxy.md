# Dynamic /v1/models and Proxy Pass-Through

> **For agentic workers:** REQUIRED SUB-SKILL: Use compose:subagent (recommended) or compose:execute to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace hardcoded model lists with database-backed dynamic models, and add transparent proxy pass-through for unmatched `/v1/*` endpoints.

**Architecture:** Two independent changes:
1. The `/v1/models` handler queries the admin `ProviderStore`/`ModelStore` (SQLite/Postgres) to return only enabled models registered via the admin API, falling back to provider `supported_models()` when no DB models exist.
2. The `/v1/*` fallback handler proxies unmatched requests to the first configured target provider, forwarding headers, body, and streaming the response back.

**Tech Stack:** Rust, Axum 0.8, reqwest (already in workspace), SQLite (via sqlx, already used)

## Global Constraints

- SQLite is the default persistence backend (`default = ["sqlite"]`)
- Follow existing code conventions: no new dependencies unless necessary
- Tests must pass: `cargo test` (workspace-wide)
- No comments in code unless explicitly asked

---

### Task 1: Dynamic /v1/models from database

**Covers:** Replace hardcoded model list with DB-backed query

**Files:**
- Modify: `crates/himadri/src/main.rs` (list_models handler)
- Modify: `crates/himadri-admin/src/handlers.rs` (add list_enabled_models_for_api)
- Test: existing `test_list_models` in `crates/himadri/src/lib.rs` E2E tests

**Interfaces:**
- Consumes: `AdminHandlers::list_enabled_models()` → `Vec<Model>` (each has `name`, `provider_id`, `enabled`, `display_name`)
- Produces: `ModelListResponse` with models from DB, or fallback to provider `supported_models()`

- [ ] **Step 1: Add `list_enabled_models_for_api` to AdminHandlers**

In `crates/himadri-admin/src/handlers.rs`, add a method that returns models suitable for the `/v1/models` endpoint — only enabled models, joined with provider name:

```rust
pub async fn list_enabled_models_for_api(&self) -> Vec<himadri_core::ModelObject> {
    let models = self.list_enabled_models().await;
    let providers = self.list_providers().await;
    let provider_map: std::collections::HashMap<String, String> = providers
        .iter()
        .map(|p| (p.id.clone(), p.name.clone()))
        .collect();

    models
        .into_iter()
        .filter_map(|m| {
            let owned_by = provider_map.get(&m.provider_id)?.clone();
            Some(himadri_core::ModelObject {
                id: m.name.clone(),
                object: "model".to_string(),
                created: m.created_at.timestamp() as u64,
                owned_by,
            })
        })
        .collect()
}
```

- [ ] **Step 2: Update list_models handler in main.rs**

Replace the hardcoded `list_models` function in `crates/himadri/src/main.rs` to query the admin store first, falling back to hardcoded lists when no DB models exist:

```rust
async fn list_models(State(state): State<AppState>) -> Json<ModelListResponse> {
    // Try database-backed models first
    let admin_models = state.admin.list_enabled_models_for_api().await;
    if !admin_models.is_empty() {
        return Json(ModelListResponse {
            object: "list".to_string(),
            data: admin_models,
        });
    }

    // Fallback to provider supported_models() for env-var-only deployments
    let providers = state.gateway.list_providers();
    let mut models = Vec::new();
    for provider_name in &providers {
        if let Some(provider) = state.gateway.get_provider(provider_name) {
            for model_id in provider.supported_models() {
                models.push(ModelObject {
                    id: model_id,
                    object: "model".to_string(),
                    created: 0,
                    owned_by: provider_name.clone(),
                });
            }
        }
    }

    Json(ModelListResponse {
        object: "list".to_string(),
        data: models,
    })
}
```

- [ ] **Step 3: Expose get_provider on Gateway**

The fallback path needs `Gateway::get_provider()`. Add to `crates/himadri/src/gateway.rs`:

```rust
pub fn get_provider(&self, name: &str) -> Option<Arc<dyn Provider>> {
    self.providers.get(name).map(|r| r.value().clone())
}
```

- [ ] **Step 4: Update the /v1/models handler in handlers.rs to match**

The `Routes::list_models` in `crates/himadri/src/handlers.rs` also has a hardcoded version. Update it to delegate to the gateway's provider list:

```rust
pub async fn list_models(State(routes): State<Arc<Self>>) -> Json<ModelListResponse> {
    let providers = routes.gateway.list_providers();
    let mut models = Vec::new();

    for provider_name in &providers {
        if let Some(provider) = routes.gateway.get_provider(provider_name) {
            for model_id in provider.supported_models() {
                models.push(ModelObject {
                    id: model_id,
                    object: "model".to_string(),
                    created: 0,
                    owned_by: provider_name.clone(),
                });
            }
        }
    }

    Json(ModelListResponse {
        object: "list".to_string(),
        data: models,
    })
}
```

- [ ] **Step 5: Build and test**

```bash
cargo test 2>&1 | grep "test result"
cargo build 2>&1 | tail -3
```

- [ ] **Step 6: Commit**

```bash
git add crates/himadri/src/main.rs crates/himadri/src/gateway.rs crates/himadri/src/handlers.rs crates/himadri-admin/src/handlers.rs
git commit -m "feat: dynamic /v1/models from database with provider fallback"
```

---

### Task 2: Proxy pass-through for /v1/*

**Covers:** Transparent proxy for unmatched /v1/* endpoints

**Files:**
- Modify: `crates/himadri/src/main.rs` (passthrough handler)
- Modify: `crates/himadri/src/gateway.rs` (add proxy method)
- Test: add E2E test for proxy

**Interfaces:**
- Consumes: first configured target's `base_url` and `api_key`
- Proxies: HTTP method, path, headers, body to upstream provider
- Returns: upstream response status, headers, body (streaming-aware)

- [ ] **Step 1: Add proxy method to Gateway**

In `crates/himadri/src/gateway.rs`, add a method that proxies a raw HTTP request to the first configured target:

```rust
pub async fn proxy(
    &self,
    method: &str,
    path: &str,
    headers: &axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Result<(axum::http::StatusCode, axum::http::HeaderMap, axum::body::Bytes), GatewayError> {
    let targets = self.targets.read().await;
    let target = targets.first().ok_or_else(|| {
        GatewayError::Internal("No targets configured for proxy".to_string())
    })?;

    let provider = self.providers.get(&target.provider).ok_or_else(|| {
        GatewayError::ProviderNotFound(target.provider.clone())
    })?;

    let base_url = target.base_url.clone().unwrap_or_else(|| {
        match provider.name() {
            "openai" => "https://api.openai.com/v1".to_string(),
            "anthropic" => "https://api.anthropic.com".to_string(),
            "gemini" => "https://generativelanguage.googleapis.com".to_string(),
            _ => "https://api.openai.com/v1".to_string(),
        }
    });

    let api_key = self.get_api_key(target)?;
    let url = format!("{}{}", base_url.trim_end_matches('/'), path);

    let client = &*crate::gateway::PROXY_CLIENT;
    let mut req_builder = match method {
        "GET" => client.get(&url),
        "POST" => client.post(&url),
        "PUT" => client.put(&url),
        "DELETE" => client.delete(&url),
        "PATCH" => client.patch(&url),
        _ => client.request(method.parse().unwrap(), &url),
    };

    for (key, value) in headers.iter() {
        if key == "authorization" || key == "host" || key == "content-length" {
            continue;
        }
        req_builder = req_builder.header(key, value);
    }

    if !api_key.is_empty() {
        req_builder = req_builder.header("authorization", format!("Bearer {}", api_key));
    }

    req_builder = req_builder.body(body);

    let resp = req_builder.send().await.map_err(|e| {
        GatewayError::Provider(format!("Proxy request failed: {}", e))
    })?;

    let status = axum::http::StatusCode::from_u16(resp.status().as_u16())
        .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR);

    let mut resp_headers = axum::http::HeaderMap::new();
    for (key, value) in resp.headers().iter() {
        if key == "transfer-encoding" || key == "connection" {
            continue;
        }
        if let (Ok(name), Ok(val)) = (
            axum::http::HeaderName::from_bytes(key.as_str().as_bytes()),
            axum::http::HeaderValue::from_bytes(value.as_bytes()),
        ) {
            resp_headers.insert(name, val);
        }
    }

    let resp_body = resp.bytes().await.map_err(|e| {
        GatewayError::Provider(format!("Failed to read proxy response: {}", e))
    })?;

    Ok((status, resp_headers, resp_body))
}
```

- [ ] **Step 2: Add static PROXY_CLIENT**

In `crates/himadri/src/gateway.rs`, add a reqwest client pool at module level:

```rust
lazy_static::lazy_static! {
    static ref PROXY_CLIENT: reqwest::Client = reqwest::Client::builder()
        .pool_max_idle_per_host(32)
        .pool_idle_timeout(std::time::Duration::from_secs(90))
        .build()
        .expect("Failed to create proxy HTTP client");
}
```

Wait — `lazy_static` is not in dependencies. Use `once_cell` which is already in `Cargo.toml`:

```rust
use once_cell::sync::Lazy;

static PROXY_CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .pool_max_idle_per_host(32)
        .pool_idle_timeout(std::time::Duration::from_secs(90))
        .build()
        .expect("Failed to create proxy HTTP client")
});
```

Add `reqwest` to `crates/himadri/Cargo.toml` dependencies:

```toml
reqwest = { workspace = true }
```

And in workspace `Cargo.toml`, reqwest is already defined.

- [ ] **Step 3: Update passthrough handler in main.rs**

Replace the stub `passthrough` handler in `crates/himadri/src/main.rs`:

```rust
async fn passthrough(
    State(state): State<AppState>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    axum::extract::Extension(auth): axum::extract::Extension<Option<AuthContext>>,
    req: axum::extract::Request,
) -> Response {
    let remote_ip = resolve_remote_ip(peer, &headers);
    let (parts, body) = req.into_parts();
    let method = parts.method.as_str().to_string();
    let uri = parts.uri.path().to_string();

    let body_bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap_or_default();

    match state.gateway.proxy(&method, &uri, &parts.headers, body_bytes).await {
        Ok((status, resp_headers, resp_body)) => {
            let mut response = axum::response::Response::builder().status(status);
            for (key, value) in &resp_headers {
                if let Some(name) = key {
                    response = response.header(name, value);
                }
            }
            response.body(axum::body::Body::from(resp_body)).unwrap_or_else(|e| {
                error_to_response(GatewayError::Internal(e.to_string()))
            })
        }
        Err(e) => error_to_response(e),
    }
}
```

- [ ] **Step 4: Build and run tests**

```bash
cargo build 2>&1 | tail -5
cargo test 2>&1 | grep "test result"
```

- [ ] **Step 5: Commit**

```bash
git add crates/himadri/src/main.rs crates/himadri/src/gateway.rs crates/himadri/Cargo.toml
git commit -m "feat: proxy pass-through for unmatched /v1/* endpoints"
```
