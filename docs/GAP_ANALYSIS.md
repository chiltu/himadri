# himadri — Gap Analysis (Corrected Status)

> Replaces the former `FEATURE_PARITY.md`, which overstated completeness in several places.
> Every status below was verified against the source, not the claim.
> Baseline for comparison is **Bifrost** (himadri is a Rust port of it).

> **⚠️ Historical snapshot (2026-06-26).** This analysis drove a series of
> sprints that have since **closed most of the gaps below**. Now implemented
> and wired (see [`CHANGELOG.md`](../CHANGELOG.md) and the current docs):
>
> - **Fallback retry loop** — `route`/`route_stream` fail over across targets.
> - **Conditional / ContentBased / ABTest strategies** — reachable from config
>   (all 8 strategy modes constructible).
> - **Response cache plugin** — wired, env-gated by `CACHE_TTL_SECS`, with
>   hit/miss metrics.
> - **Request-log persistence** — Postgres request-log store wired (with
>   in-memory fallback); filtered queries fixed.
> - **`himadri-auth` compiled and wired** — JWT/OIDC on `/v1` via
>   `CombinedAuth` (JWKS discovery + refresh); auth-failure auditing.
> - **RBAC** — per-role model/provider allow-lists with wildcards and
>   `default_role` ([configuration.md](./configuration.md#rbac-tiered-access)).
> - **Budgets** — per-principal USD caps enforced and cost accumulated,
>   including for streamed responses.
> - **Embeddings** — `POST /v1/embeddings` with provider fallback.
> - **Tool calling** — `tools`/`tool_choice` modeled in core and translated
>   per provider (OpenAI-compatible, Anthropic, Gemini, Bedrock).
> - **`/v1/*` passthrough proxy** — functional (auth-gated, 10 MiB body cap).
> - **Config history & rollback** — `GET /admin/config/history` and
>   `POST /admin/config/rollback/{version}` implemented (in-memory history).
>
> Still open, as of 2026-07-05: the ~17 additional Bifrost providers, inbound
> Anthropic/GenAI API schemas, image/audio endpoints, MCP gateway, semantic
> (non-exact) caching, cluster mode/HA beyond the `redis` feature, and vault
> secret backends. Statuses in the body below are **not** updated.

## Status legend

- ✅ **Feature-complete** — implemented *and* wired into the running binary, usable end-to-end
- 🟡 **Dev-complete (not wired)** — code exists but is unreachable: not in the workspace, not registered, or stubbed at the call site
- 🟠 **Partial** — works for some cases only
- ❌ **Missing**

---

## A. Providers

| Requirement | Status | Evidence |
|---|---|---|
| OpenAI, Anthropic, Gemini, Azure, Bedrock (dedicated impls) | ✅ | `crates/himadri-provider/src/{openai,anthropic,gemini,azure,bedrock}/provider.rs`, 367–413 LOC each |
| OpenAI-compatible (OpenRouter, Together, Groq, Fireworks, DeepInfra, Cerebras, Novita) | ✅ | `compatible/provider.rs`, env-gated registration in `main.rs` |
| Remaining ~17 Bifrost providers (mistral, cohere, deepseek, ollama, vertex, xai, perplexity, qwen, …) | ❌ | absent |
| Embeddings | ❌ | `Provider` trait exposes only `complete` / `complete_stream` |
| Image generation | ❌ | absent |
| Audio / speech / transcription | ❌ | absent |
| Model discovery | ❌ | `/v1/models` is hard-coded lists in `main.rs::list_models` |

## B. Routing strategies

> `FEATURE_PARITY.md` claims "8/8 FULL". This is incorrect.

| Strategy | Status | Evidence |
|---|---|---|
| Single, LoadBalance, LeastLatency, CostOptimized | ✅ | `StrategyMode` variants exist and `strategy.rs::select` implements them |
| Fallback | 🟠 | `select()` returns `targets.first()` only — **no retry-on-failure loop** in `gateway.rs::route`. Effectively non-functional. |
| Conditional, ContentBased, ABTest | 🟡 | implemented in `strategy.rs::select` but `StrategyMode` (core `config.rs`) has only 5 variants — these 3 can never be constructed from config. Dead code. |

## C. Plugins

> `FEATURE_PARITY.md` claims "6/6 FULL". Misleading — one is unwired.

| Plugin | Status | Evidence |
|---|---|---|
| word_filter, max_token, logger | ✅ | registered in `main.rs` |
| budget, rate_limit | ✅ | registered (env-gated) |
| cache | 🟡 | `ResponseCachePlugin` exists (moka, SHA-256 exact-match) but **never registered in `main.rs` and never referenced in `gateway.rs`** |

## D. Admin API

| Endpoint | Status | Evidence |
|---|---|---|
| Key CRUD + revoke/rotate | ✅ | `main.rs` admin routes |
| Provider/Model CRUD + toggle (live target rebuild) | ✅ | `rebuild_targets_from_db` |
| Dashboard, usage stats | ✅ | `usage_store` |
| `GET/PUT /admin/config` | ✅ | wired |
| `GET /admin/config/history` | 🟠 | returns `data: []` placeholder |
| `POST /admin/config/rollback/{v}` | 🟡 | returns 501 Not Implemented |
| `GET/DELETE /admin/logs` | 🟠 | backed by in-memory store only |

## E. Request logs

| Requirement | Status | Evidence |
|---|---|---|
| Entry/query structs, in-memory store, wired into request flow | ✅ | `gateway.rs::route` writes entries |
| SQL persistence | 🟡 | Postgres backend exists in `himadri-admin` but `Gateway::new` hard-codes `InMemoryRequestLogStore` — **logs lost on restart** |

## F. Middleware

| Requirement | Status |
|---|---|
| CORS, per-key rate limit | ✅ |
| Per-IP rate limit | ✅ (env-gated plugin) |
| Proxy auth | ❌ |

## G. Proxy / MCP

| Requirement | Status | Evidence |
|---|---|---|
| Proxy passthrough | 🟡 | `passthrough` handler returns 404 stub |
| MCP (tool gateway / agentic loop) | ❌ | absent |

## H. Cross-cutting infrastructure

| Requirement | Status | Evidence |
|---|---|---|
| Circuit breaker (in-memory + Redis) | ✅ | wired per-provider in `gateway.rs` |
| Rate limiter (token bucket + sliding window + Redis) | ✅ | `himadri-ratelimit` |
| Latency tracking (in-memory + Redis) | ✅ | used by LeastLatency |
| Prometheus metrics | ✅ | `/metrics` |
| OTel tracing | ✅ | `init_tracing` honors endpoint + sample ratio |
| Tool calling | 🟠 | response/message types model `tool_calls`, but `ChatCompletionRequest` has no `tools`/`tool_choice` field — relies on `#[serde(flatten)] extra` passthrough; per-provider forwarding unverified |
| Multimodal input | 🟠 | `ContentPart::ImageUrl` exists; no image/audio output |

---

## I. Enterprise auth (`auth-requirements.md`)

> **Headline gap:** the entire `himadri-auth` crate is NOT listed in `Cargo.toml` workspace members and is NOT imported by the binary. `main.rs` uses `himadri_admin::AuthMiddleware` (API-key + master-key only). All of the below exist as source but are **not compiled or reachable**. The parity doc's "Auth: SUPERIOR" claim is inverted.

| Phase | Requirement | Status | Files |
|---|---|---|---|
| 1 | JWT validation + OIDC discovery (JWKS refresh) | 🟡 | `jwt.rs`, `oidc.rs` |
| 2 | OAuth2 client-credentials + token introspection | 🟡 | `oauth2_client.rs`, `introspect.rs` |
| 3 | Zitadel integration (resolver, webhooks) | 🟡 | `zitadel.rs` |
| 4 | Claim-based rate limits / budgets | 🟡 | partial in `middleware.rs` |
| 5 | Multi-tenant isolation + RBAC + audit | 🟠 | org/team guardrails live in `gateway.rs`; no RBAC enum |
| 6 | Auth-order strategy + migration docs | ❌ | not implemented |

---

## J. Comparison with Bifrost

| Capability | Bifrost | himadri | Gap |
|---|---|---|---|
| Providers | 12+ providers, 1000+ models | 5 native + 7 OpenAI-compatible | Large |
| Drop-in OpenAI API | ✅ | ✅ | — |
| Drop-in Anthropic / GenAI inbound APIs | ✅ | ❌ (OpenAI schema in only) | Yes |
| Streaming | ✅ | ✅ (SSE) | — |
| Embeddings / image / audio / transcription | ✅ | ❌ | Yes |
| Tool calling | ✅ first-class | 🟠 passthrough via `extra` | Partial |
| MCP gateway | ✅ | ❌ | Yes |
| Fallbacks + load balancing | ✅ across providers and keys | 🟠 LB works; fallback is a stub; no multi-key weighting | Yes |
| Semantic caching | ✅ | 🟡 exact-hash cache, unwired | Yes |
| Governance (virtual keys, budgets, hierarchy) | ✅ teams/customers/orgs | 🟠 org/team guardrails + budget plugin; no virtual-key hierarchy | Partial |
| Observability (Prometheus + OTel + tracing) | ✅ | ✅ | — |
| Web UI | ✅ | 🟠 Next.js dashboard, backend-complete only | Partial |
| Plugins / custom middleware | ✅ | ✅ trait system (subset wired) | Partial |
| Cluster mode / HA | ✅ | ❌ single process | Yes |
| Vault / secret backends | ✅ | ❌ `get_api_key` reads env vars only | Yes |
| Config hot-reload | ✅ | ✅ | — |
| Persistent storage | ✅ | 🟠 keys/providers in Postgres/SQLite; request logs in-memory only | Partial |
| Enterprise auth (JWT/OIDC/OAuth2) | governance | 🟡 written, not compiled | Yes |

---

## K. Biggest real gaps (priority order)

1. **`himadri-auth` is dead code** — not in the workspace, not wired. Largest "looks done but isn't" item.
2. **Fallback strategy doesn't fall back** — no failure-retry loop; the headline reliability feature is non-functional.
3. **3 of 8 strategies unreachable** — `Conditional`/`ContentBased`/`ABTest` lack `StrategyMode` variants and config plumbing.
4. **Cache plugin unwired** — implemented but never registered or invoked.
5. **Request logs are volatile** — Postgres backend exists but gateway hard-codes the in-memory store.
6. **No embeddings/image/audio, no MCP, no inbound Anthropic API** — major Bifrost surface absent.
7. **Config history/rollback stubbed** (`data: []`, 501).
8. **Tool calling is passthrough-by-flatten**, not modeled — fragile, per-provider unverified.
</content>
