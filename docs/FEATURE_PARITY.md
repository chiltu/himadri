# Feature Parity Matrix: Go vs Rust (himadri)

## Summary

| Area | Status | Go Features | Rust Features |
|------|--------|-------------|---------------|
| **Providers** | PARTIAL | 29 providers + Embedding/Image/Discovery/Proxy | 10 providers via generic OpenAI-compatible |
| **Strategies** | ✅ FULL | 8 strategies | 8 strategies |
| **Plugins** | ✅ FULL | 6 plugins | 6 plugins |
| **Admin API** | PARTIAL | Full CRUD + history + rollback + logs | Key CRUD + reload |
| **Auth** | SUPERIOR | API key + master key + bootstrap | + JWT/OIDC/OAuth2/Zitadel |
| **Config** | PARTIAL | Full API + history + rollback | Manager coded, API partial |
| **Request Logs** | PARTIAL | Full with SQL persistence | Data structures only |
| **Middleware** | PARTIAL | CORS + rate limit + proxy auth | CORS + auth (key-based) |
| **Proxy** | MISSING | Full pass-through | Stub (404) |
| **MCP** | MISSING | Full implementation | None |
| **Circuit Breaker** | ✅ FULL+ | In-memory | + Redis-backed |
| **Rate Limiter** | ✅ FULL+ | Token bucket | + Sliding window + Redis |
| **Metrics** | PARTIAL | Prometheus format | Basic metrics |
| **Observability** | PARTIAL | Full OTel pipeline | Basic tracing |
| **Latency Tracking** | ✅ FULL+ | In-memory | + Redis-backed |
| **CLI** | MISSING | Cobra subcommands | None |
| **Tests** | ⚠️ | 1,031 tests | 156 tests |

## Detailed Feature Breakdown

### 1. Providers (PARTIAL — 10/29)

| Provider | Go | Rust | Notes |
|----------|-----|------|-------|
| openai | ✅ | ✅ | Dedicated impl |
| anthropic | ✅ | ✅ | Dedicated impl |
| gemini | ✅ | ✅ | Dedicated impl |
| azure_openai | ✅ | ✅ | OpenAI-compatible with Azure config |
| bedrock | ✅ | ✅ | Dedicated impl (REST API) |
| openrouter | ✅ | ✅ | OpenAI-compatible |
| together | ✅ | ✅ | OpenAI-compatible |
| groq | ✅ | ✅ | OpenAI-compatible |
| fireworks | ✅ | ✅ | OpenAI-compatible |
| deepinfra | ✅ | ✅ | OpenAI-compatible |
| cerebras | ✅ | ✅ | OpenAI-compatible |
| novita | ✅ | ✅ | OpenAI-compatible |
| ai21 | ✅ | ❌ | Missing |
| azure_foundry | ✅ | ❌ | Missing |
| cloudflare | ✅ | ❌ | Missing |
| cohere | ✅ | ❌ | Missing |
| databricks | ✅ | ❌ | Missing |
| deepseek | ✅ | ❌ | Missing |
| hugging_face | ✅ | ❌ | Missing |
| mistral | ✅ | ❌ | Missing |
| moonshot | ✅ | ❌ | Missing |
| nvidia_nim | ✅ | ❌ | Missing |
| ollama | ✅ | ❌ | Missing |
| ollama_cloud | ✅ | ❌ | Missing |
| perplexity | ✅ | ❌ | Missing |
| qwen | ✅ | ❌ | Missing |
| replicate | ✅ | ❌ | Missing |
| sambanova | ✅ | ❌ | Missing |
| vertex_ai | ✅ | ❌ | Missing |
| xai | ✅ | ❌ | Missing |

**Missing interfaces:**
- `EmbeddingProvider` — No embedding support in Rust
- `ImageProvider` — No image generation in Rust
- `DiscoveryProvider` — No model discovery in Rust
- `ProxiableProvider` — Proxy stub only

### 2. Strategies (✅ FULL)

| Strategy | Go | Rust |
|----------|-----|------|
| Single | ✅ | ✅ |
| Fallback | ✅ | ✅ |
| LoadBalance | ✅ | ✅ |
| LeastLatency | ✅ | ✅ |
| CostOptimized | ✅ | ✅ |
| Conditional | ✅ | ✅ |
| ContentBased | ✅ | ✅ |
| ABTest | ✅ | ✅ |

### 3. Plugins (✅ FULL)

| Plugin | Go | Rust |
|--------|-----|------|
| budget | ✅ | ✅ |
| cache | ✅ | ✅ |
| logger | ✅ | ✅ |
| maxtoken | ✅ | ✅ |
| ratelimit | ✅ | ✅ |
| wordfilter | ✅ | ✅ |

### 4. Admin API (PARTIAL)

| Endpoint | Go | Rust |
|----------|-----|------|
| GET /admin/keys | ✅ | ✅ |
| POST /admin/keys | ✅ | ✅ |
| GET /admin/keys/{id} | ✅ | ✅ |
| PUT /admin/keys/{id} | ✅ | ✅ |
| DELETE /admin/keys/{id} | ✅ | ✅ |
| POST /admin/keys/{id}/revoke | ✅ | ✅ |
| POST /admin/keys/{id}/rotate | ✅ | ✅ |
| GET /admin/keys/usage | ✅ | ❌ |
| GET /admin/config | ✅ | ❌ |
| GET /admin/config/history | ✅ | ❌ |
| POST /admin/config | ✅ | ❌ |
| PUT /admin/config | ✅ | ❌ |
| DELETE /admin/config | ✅ | ❌ |
| POST /admin/config/rollback/{v} | ✅ | ❌ |
| GET /admin/dashboard | ✅ | ❌ |
| GET /admin/logs | ✅ | ❌ |
| DELETE /admin/logs | ✅ | ❌ |
| GET /admin/logs/stats | ✅ | ❌ |
| GET /admin/providers | ✅ | ❌ |
| GET /admin/plugins | ✅ | ❌ |
| POST /admin/reload | ❌ | ✅ |

### 5. Auth (SUPERIOR — Rust has more)

| Feature | Go | Rust |
|---------|-----|------|
| API key auth | ✅ | ✅ |
| Master key | ✅ | ✅ |
| Bootstrap keys | ✅ | ❌ |
| JWT validation | ❌ | ✅ |
| OIDC discovery | ❌ | ✅ |
| OAuth2 introspection | ❌ | ✅ |
| Zitadel integration | ❌ | ✅ |
| Rate limit from claims | ❌ | ✅ |

### 6. Config Management (PARTIAL)

| Feature | Go | Rust |
|---------|-----|------|
| GatewayConfigManager | ✅ | ✅ |
| Versioned history | ✅ | ✅ |
| Rollback to version | ✅ | ✅ |
| Reset to initial | ✅ | ✅ |
| Validation on reload | ✅ | ✅ |
| GET /admin/config | ✅ | ❌ |
| POST /admin/config | ✅ | ❌ |
| PUT /admin/config | ✅ | ❌ |
| DELETE /admin/config | ✅ | ❌ |
| GET /admin/config/history | ✅ | ❌ |
| POST /admin/config/rollback/{v} | ✅ | ❌ |

### 7. Request Logs (PARTIAL)

| Feature | Go | Rust |
|---------|-----|------|
| Entry struct | ✅ | ✅ |
| Query struct | ✅ | ✅ |
| InMemoryStore | ✅ | ✅ |
| SQL persistence | ✅ | ❌ |
| Wire into request flow | ✅ | ❌ |
| GET /admin/logs | ✅ | ❌ |
| DELETE /admin/logs | ✅ | ❌ |
| GET /admin/logs/stats | ✅ | ❌ |

### 8. Middleware (PARTIAL)

| Feature | Go | Rust |
|---------|-----|------|
| CORS | ✅ | ✅ |
| Rate limit (per-key) | ✅ | ✅ |
| Rate limit (per-IP) | ✅ | ❌ |
| Proxy auth | ✅ | ❌ |

### 9. Proxy (MISSING)

| Feature | Go | Rust |
|---------|-----|------|
| Pass-through forwarding | ✅ | ❌ (stub returns 404) |
| Provider resolution | ✅ | ❌ |
| Header injection | ✅ | ❌ |

### 10. MCP (MISSING)

| Feature | Go | Rust |
|---------|-----|------|
| MCP client | ✅ | ❌ |
| Tool registry | ✅ | ❌ |
| Agentic loop | ✅ | ❌ |
| Gateway integration | ✅ | ❌ |
