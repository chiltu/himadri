# SPEC: Guardrails — Inline PII Redaction via `redact-core`

| | |
|---|---|
| **Status** | Implemented — Phases 1–3 (2026-07-14). Bench: 4 KiB dense-PII redact ≈ 0.18 ms (budget was <5 ms); adversarial input scales linearly. Follow-ups tracked in SPEC_GUARDRAILS_V2.md |
| **Date** | 2026-07-13 |
| **Owner** | — |
| **Tracking** | GAP_ANALYSIS.md rows 5 (multi-tenant guardrails) / "Governance" |
| **Follow-up** | [SPEC_GUARDRAILS_V2.md](SPEC_GUARDRAILS_V2.md) — v2 plan (NER, de-redaction, mid-stream scanning, embeddings, per-key overrides) |

## 1. Summary

Add a first-class **guardrails** feature to himadri that detects and redacts
PII (emails, phone numbers, SSNs, credit cards, API keys, …) in request
content **before it is forwarded to any LLM provider**, using the
[`redact-core`](https://crates.io/crates/redact-core) crate (v0.8.3,
Apache-2.0) as the detection/anonymization engine. The same engine optionally
scans provider **responses** through the existing `ResponseGuardrail` hook.

Three enforcement modes per deployment/org: **redact** (rewrite content
inline and forward), **block** (reject the request with 400), and
**observe** (detect, count, log — forward unchanged).

## 2. Motivation

- Prompts routinely carry PII and credentials that today leave the gateway
  verbatim and land in third-party provider logs. The gateway is the single
  choke point where an organization can enforce "no raw PII leaves our
  perimeter".
- The config schema already *promises* this feature but doesn't deliver it:
  `OrgGuardrailConfig.content_filter` has a `block_pii: bool` field
  (`crates/himadri-core/src/config.rs:202`) that **no code reads** —
  `check_org_guardrails` (`gateway/policy.rs:73`) only enforces
  `blocked_words` and `max_tokens_per_request`.
- The only redaction that exists today is the audit-log `Redactor`
  (`himadri-observability/src/redact.rs`) — 5 hardcoded regexes applied
  *after* the provider call, protecting only our own logs, not the upstream.
- GAP_ANALYSIS.md flags guardrails/governance as partial (🟠) vs.
  competitors.

## 3. Current state (what this builds on)

| Existing piece | Location | Relevance |
|---|---|---|
| `Plugin` trait, `Stage::BeforeRequest`, `PluginType::Guardrail` | `himadri-plugin/src/traits.rs` | The PII plugin is a `Guardrail`-type before-request plugin |
| `ResponseGuardrail` trait + `ResponseAction::{Allow,Reject,Redact}` | `himadri-plugin/src/traits.rs:42` | Output-side scanning slot; `run_response_guardrails` already called in `route()` and (buffered, end-of-stream) in `stream.rs` |
| `WordFilterPlugin` | `himadri-plugins/src/word_filter.rs` | Closest analog: env-configured, before-request, iterates `ctx.request.messages` |
| `check_org_guardrails` | `himadri/src/gateway/policy.rs:73` | Per-org/team blocked-words + token caps; PII enforcement slots in beside it conceptually |
| `OrgGuardrailConfig` / `ContentFilterConfig` | `himadri-core/src/config.rs:178–209` | Config vocabulary to extend (its `block_pii`/`custom_patterns` are currently dead) |
| Audit `Redactor` | `himadri-observability/src/redact.rs` | Secret patterns (JWT, `sk-…`, AKIA, bearer) worth replicating as custom recognizers; stays independent (§ 6.7) |
| `redact_response_text` | `gateway/audit.rs:90` | Applies a `ResponseAction::Redact` to a response body |
| `guardrail_actions` on `AuditEvent` | `himadri-observability/src/audit.rs:34` | Where guardrail outcomes are recorded |

### 3.1 Critical integration gap: plugin request mutations are dropped

`Gateway::route` builds the plugin context from the request
(`prepare_request` clones it into `ctx.request`), runs before-request
plugins, **then forwards the original `request`, not `ctx.request`, to the
provider** (`gateway/route.rs:301–305`; same pattern in `stream.rs:42–45`).
A plugin that rewrites `ctx.request.messages` today has no effect on what
the upstream sees.

Inline redaction is impossible without closing this gap, so this spec
includes the fix (§ 6.4): after the before-request pipeline, **the
pipeline's view of the request (`ctx.request`) becomes the request of
record** for provider dispatch, response caching, and audit logging.

## 4. The `redact-core` crate

Surveyed at v0.8.3 (published 2026-04-19, Apache-2.0, repo
`github.com/censgate/redact`, docs at docs.rs/redact-core). Positioned as a
Rust replacement for Microsoft Presidio.

**API shape** (all synchronous; `AnalyzerEngine` is `Send + Sync + Clone +
Default`):

```rust
use redact_core::{AnalyzerEngine, AnonymizerConfig, AnonymizationStrategy};

let engine = AnalyzerEngine::new();                     // default recognizers
let analysis = engine.analyze(text, None)?;             // -> AnalysisResult { detected_entities, .. }
let analysis = engine.analyze_with_entities(text, &[EntityType::EmailAddress, ..], None)?;
let out = engine.anonymize(text, None, &AnonymizerConfig {
    strategy: AnonymizationStrategy::Replace,           // Replace | Mask | Hash | Encrypt
    ..Default::default()
})?;                                                     // -> AnonymizedResult { text, .. }
```

- **Detection**: 36 pattern-based entity types — contact
  (`EMAIL_ADDRESS`, `PHONE_NUMBER`, `IP_ADDRESS`, `URL`), financial
  (`CREDIT_CARD`, `IBAN_CODE`, `US_BANK_NUMBER`), government IDs (US SSN /
  passport / driver license, UK NHS / NINO), healthcare, crypto wallets,
  technical (`GUID`, `MAC_ADDRESS`, hashes), plus checksum validators
  (Luhn for cards, IBAN, NHS, SSN). NER-based `PERSON` / `ORGANIZATION` /
  `LOCATION` detection exists in the sibling `redact-ner` crate (ONNX) — see
  non-goals.
- **Anonymization strategies**: `Replace` (`[EMAIL_ADDRESS]`), `Mask`
  (`jo**@****le.com`, configurable `mask_char` / start / end chars), `Hash`
  (salted, `[EMAIL_ADDRESS_a1b2c3d4]`), `Encrypt` (reversible
  `<TOKEN_uuid>` via AES-GCM, keyed by `encryption_key`).
  `AnonymizerConfig` also carries `hash_salt` and `preserve_format`.
- **Extensibility**: `Recognizer` trait + `PatternRecognizer` for custom
  regex recognizers (used in § 6.7 for gateway-secret patterns);
  `Anonymizer` trait for custom strategies; a `policy` module
  (`Policy` / `PatternRule` / `RedactionConfig`) for rule bundles.
- **Dependencies** it brings in: `regex`, `aes-gcm`, `blake3`, `sha2`,
  `pbkdf2`, `rayon`, `serde`, `thiserror`, `tracing`, `uuid` — all
  mainstream. No network access, no async runtime, no C deps.
- **Performance claims**: sub-millisecond regex inference; published
  benchmark p50 0.196 ms vs Presidio 6.25 ms. We validate ourselves (§ 10).

**Maturity caveats** (drives design decisions below): pre-1.0 (0.8.x),
~940 total downloads, single publisher, first release 2026-02. Therefore:

1. **Pin exactly**: `redact-core = "=0.8.3"`; upgrades are deliberate.
2. **Isolate behind an internal trait** (`PiiEngine`, § 6.2) so the
   dependency is swappable without touching plugin/gateway code.
3. **Feature-gate** the dependency (`guardrails` cargo feature) so builds
   that don't want the extra dependency tree can drop it.
4. Vendor-review the crate source before first ship (it's small, ~52 KB).

## 5. Goals / non-goals

### Goals (v1)

- G1. Detect + redact/block/observe PII in **chat-completion request
  messages** (both `/v1/chat/completions` and `/v1/completions` paths that
  flow through `Gateway::route` / `route_stream`), *before* provider
  dispatch, including failover retries (redact once, all attempts see the
  redacted request).
- G2. Global (env/config-file) configuration **and** per-org/per-team
  overrides riding the existing `OrgGuardrailConfig`, live-reloadable via
  `/admin/config` like everything else.
- G3. Configurable entity set, confidence threshold, strategy, and mode.
- G4. Response-side PII scanning via `ResponseGuardrail` (redact or block
  model output), reusing the existing buffered end-of-stream hook for
  streams.
- G5. Redacted request becomes the request of record: provider dispatch,
  response-cache key/value, audit log, and request log all see the
  redacted content — raw PII does not persist anywhere in the gateway.
- G6. Observability: Prometheus counters/histograms + `guardrail_actions`
  audit entries. Entity *types and counts* are recorded; entity *values*
  never are.
- G7. Custom secret patterns (JWTs, `sk-…` keys, AWS keys, bearer tokens)
  registered as recognizers so credentials are covered, not just classic
  PII.

### Non-goals (v1)

- NER-based PERSON/ORG/LOCATION detection (`redact-ner`, ONNX runtime —
  large binary + model distribution story; revisit as v2 opt-in).
- Mid-stream response redaction. Streaming keeps today's semantics: chunks
  flow through, guardrails run on the buffered full text at stream end
  (`stream.rs:211` — the existing comment documents windowed scanning as
  future work). A streamed response can therefore emit PII before the
  end-of-stream check; document this loudly.
- The `/v1/*` catch-all proxy (`gateway/proxy.rs`) — raw pass-through
  bodies are not parsed today and won't be scanned. Documented limitation.
- `/v1/embeddings` input redaction — `embed()` bypasses the plugin
  pipeline entirely (`route.rs:231`); scanning it is v1.1 (§ 12).
- De-redaction (reversible `Encrypt` tokens re-inflated in responses) — v2;
  the config schema reserves room for it.
- Toxicity/jailbreak detection (`block_toxicity` stays dead in v1; § 13).
- Image content (`ContentPart::ImageUrl`) — text parts only.

## 6. Design

### 6.1 Placement: new module in `himadri-plugins`, feature-gated

New file `crates/himadri-plugins/src/pii_guardrail.rs` (plus
`pii_engine.rs` for the trait + redact-core adapter), behind a cargo
feature:

```toml
# himadri-plugins/Cargo.toml
[features]
default = ["guardrails"]
guardrails = ["dep:redact-core"]

[dependencies]
redact-core = { version = "=0.8.3", optional = true }
```

`himadri` (the binary crate) re-exports the feature. Rationale for a module
rather than a new crate: it is a plugin like the other six; it needs
nothing the plugins crate doesn't already have (`himadri-plugin`,
`himadri-core`, `async-trait`). If the engine grows (NER, policies), it can
graduate to `himadri-guardrails` later without changing any public shape.

### 6.2 Internal engine abstraction

```rust
/// What the gateway needs from a PII engine. Implemented by the
/// redact-core adapter; swappable if the upstream crate stalls.
pub trait PiiEngine: Send + Sync {
    /// Scan text; return detected entities (type, span, confidence).
    fn scan(&self, text: &str) -> Result<Vec<PiiEntity>, PiiError>;
    /// Rewrite text according to `opts`, returning the new text and what
    /// was replaced (types + counts only — never original values).
    fn redact(&self, text: &str, opts: &RedactOptions) -> Result<RedactOutcome, PiiError>;
}

pub struct PiiEntity {
    pub entity_type: String,   // stable string form, e.g. "EMAIL_ADDRESS"
    pub start: usize,
    pub end: usize,
    pub confidence: f32,
}

pub struct RedactOptions {
    pub strategy: RedactStrategy,        // Replace | Mask | Hash | Encrypt
    pub entities: Option<Vec<String>>,   // None = all supported
    pub min_confidence: f32,             // default 0.6
}

pub struct RedactOutcome {
    pub text: String,
    pub replaced: Vec<(String /*entity_type*/, u32 /*count*/)>,
}
```

The redact-core adapter (`RedactCoreEngine`) holds one
`redact_core::AnalyzerEngine` built at startup:

- `AnalyzerEngine::new()` for the default 36 recognizers, then
  `recognizer_registry_mut()` extended with `PatternRecognizer`s for the
  gateway-secret patterns (§ 6.7).
- `scan` → `analyze` / `analyze_with_entities`, filtered by
  `min_confidence` client-side.
- `redact` → `anonymize` with an `AnonymizerConfig` mapped from
  `RedactOptions` (`strategy`, `hash_salt`, `encryption_key`,
  `preserve_format: true`).
- Engine construction is config-independent (entity subset, threshold, and
  strategy are per-call), so **live config reload never rebuilds the
  engine**.

Keys/salts come from env only (`GUARDRAILS_HASH_SALT`,
`GUARDRAILS_ENCRYPTION_KEY`) and live in the adapter, **not** in `Config` —
`Config` is served verbatim by `GET /admin/config` (see the `master_key`
CWE-522 comment at `config.rs:411`); secrets must never enter it.

### 6.3 The plugin

```rust
pub struct PiiGuardrailPlugin {
    engine: Arc<dyn PiiEngine>,
    /// Same handle Gateway holds; per-org overrides resolve per request.
    config: Arc<tokio::sync::RwLock<himadri_core::Config>>,
    /// Global defaults (from env / top-level config), used when no org
    /// override applies.
    defaults: PiiGuardrailSettings,
    metrics: Arc<Metrics>,
}
```

- `plugin_type()` → `PluginType::Guardrail`; `stage()` →
  `Stage::BeforeRequest`. Registered **first** in `wire_plugins` order so
  downstream before-request plugins (word filter, logger, budget) and the
  response cache see redacted content.
- `execute(ctx)`:
  1. Resolve effective settings: global defaults, overridden by
     `config.orgs[ctx.org_id()].guardrails.pii`, then by the team's
     (`most-specific wins`, mirroring `check_org_guardrails` precedence).
     Disabled → return `Ok(())`.
  2. For each `ctx.request.messages[i]` whose `role` is in `apply_to`
     (default: `user`, `system`, `tool` — not `assistant` history, which
     already round-tripped through a provider) and whose `content` is
     `Some`:
     - `MessageContent::Text(s)` → scan/redact `s`, replace in place.
     - `MessageContent::Parts(parts)` → each `ContentPart::Text` part
       individually; `ImageUrl` untouched.
  3. If `scan_tool_arguments` (default **false**): also scan each
     `message.tool_calls[].function.arguments` and, when redacting, replace
     the argument string. (JSON-structure-blind string scan — acceptable
     because arguments are opaque strings already.)
  4. Mode handling:
     - **observe** — `scan` only; record metrics + `ctx.set_metadata`.
     - **redact** — `redact`; write text back into `ctx.request`; record
       what was replaced in `ctx.metadata["guardrails.pii"]` as
       `{"action":"redact","entities":{"EMAIL_ADDRESS":2,…}}` (types and
       counts only).
     - **block** — `scan`; on any hit return
       `PluginError::Rejected { kind: RejectKind::BadRequest, reason:
       "PII detected: EMAIL_ADDRESS, US_SSN" }` (types only, never
       values) → maps to HTTP 400 via `map_plugin_error`.
  5. **Fail-closed by default**: an engine error (`PiiError`) becomes
     `PluginError::Internal` → 500, request not forwarded. A
     `fail_open: bool` setting (default `false`) downgrades engine errors
     to a logged warning + unscanned forward, for availability-first
     deployments.

**Blocking-call discipline**: redact-core is synchronous and CPU-bound
(regex + rayon). For typical prompt sizes it's sub-millisecond, so calls
run inline on the async worker when the total scanned length is under
`GUARDRAILS_INLINE_LIMIT_BYTES` (default 16 KiB); above that, the plugin
moves the scan to `tokio::task::spawn_blocking` to avoid stalling the
runtime. The § 10 benchmark validates the threshold.

### 6.4 Gateway change: `ctx.request` becomes the request of record

In `gateway/route.rs::route` and `gateway/stream.rs::route_stream`, after
`prepare_request` returns:

```rust
let mut ctx = self.prepare_request(&request, auth, remote_ip).await?;
let request = ctx.request.clone();   // pipeline-visible request wins
```

All subsequent uses of `request` — response-cache `get`/`insert`, target
selection, `provider.complete`/`complete_stream`, `log_audit`,
`audit_messages` — operate on the (possibly redacted) pipeline copy. This
is one line per path plus a doc comment on `prepare_request` making the
contract explicit: *before-request plugins may rewrite `ctx.request`; the
gateway forwards what the pipeline produced*.

Consequences, all desirable:

- The response cache is keyed and populated with redacted content: two
  requests differing only in redacted PII share a cache entry, and raw PII
  never sits in the cache.
- Audit events and request logs record the redacted messages — consistent
  with what the provider actually received (this is what an auditor needs)
  and with the "raw PII does not persist" goal (G5). The audit event's
  `guardrail_actions` gains a `pii_redact: EMAIL_ADDRESS×2` style entry
  from `ctx.metadata`.
- Existing plugins are unaffected — none of them currently mutate
  `ctx.request`.

Model-selection note: strategy selection (`select_targets` /
`content_based` rules) also moves to the redacted request. Content-routing
rules that matched on PII-shaped text would change behavior; called out in
release notes, considered correct.

### 6.5 Response-side guardrail

`PiiResponseGuardrail` implements the existing `ResponseGuardrail` trait:

```rust
async fn check_response(&self, ctx: &PluginContext, response: &str)
    -> Result<ResponseAction, PluginError>
{
    // resolve settings exactly as the request-side plugin does
    match settings.response_mode {
        Off      => Ok(ResponseAction::Allow),
        Observe  => { scan, count metrics; Ok(Allow) }
        Redact   => Ok(ResponseAction::Redact(engine.redact(response, &opts)?.text)),
        Block    => Ok(ResponseAction::Reject("PII detected in model output".into())),
    }
}
```

No gateway changes needed: `route()` already applies
`Redact`/`Reject` (`route.rs:349–402`) and the streaming path already runs
guardrails on the buffered stream at end (`stream.rs:211` — with the
documented "chunks already sent" caveat). Registered via the existing
`PluginManager::register_response_guardrail` (currently never called in
production wiring — this is its first user).

### 6.6 Configuration

#### Config-file schema (himadri-core)

New `PiiGuardrailConfig`, referenced from **both** the top-level `Config`
(global default) and `OrgGuardrailConfig`/`TeamConfig.guardrails`
(overrides):

```rust
// himadri-core/src/config.rs
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PiiGuardrailConfig {
    #[serde(default)] pub enabled: bool,
    #[serde(default)] pub mode: PiiMode,            // Redact (default) | Block | Observe
    #[serde(default)] pub strategy: PiiStrategy,    // Replace (default) | Mask | Hash | Encrypt
    /// None = all supported entity types.
    #[serde(default)] pub entities: Option<Vec<String>>,
    #[serde(default = "default_min_confidence")] pub min_confidence: f32, // 0.6
    /// Roles scanned. Default: ["user", "system", "tool"].
    #[serde(default = "default_apply_to")] pub apply_to: Vec<String>,
    #[serde(default)] pub scan_tool_arguments: bool,
    #[serde(default)] pub fail_open: bool,
    /// Response-side scanning. Off by default.
    #[serde(default)] pub response_mode: PiiResponseMode, // Off | Observe | Redact | Block
}
```

Wired as:

- `Config.guardrails: GuardrailsConfig { pii: PiiGuardrailConfig }` — new
  top-level section (global default, admin-reloadable).
- `OrgGuardrailConfig.pii: Option<PiiGuardrailConfig>` — org override
  (present = replaces the global settings wholesale for that org; field
  merge is deliberately avoided to keep resolution predictable).
- `TeamConfig.guardrails.pii` — team override, same rule, wins over org.
- `ContentFilterConfig.block_pii` is **deprecated**: config load maps
  `content_filter.block_pii == true` (with no explicit `pii` section) to
  `pii: { enabled: true, mode: block }` and logs a deprecation warning.
  The field is removed in the release after next.

#### Env vars (parity with the other plugins' wiring in `wire_plugins`)

| Var | Meaning | Default |
|---|---|---|
| `GUARDRAILS_PII_MODE` | `redact` / `block` / `observe`; presence enables the plugin globally | unset (disabled) |
| `GUARDRAILS_PII_STRATEGY` | `replace` / `mask` / `hash` / `encrypt` | `replace` |
| `GUARDRAILS_PII_ENTITIES` | comma-separated entity types | all |
| `GUARDRAILS_PII_MIN_CONFIDENCE` | float | `0.6` |
| `GUARDRAILS_PII_RESPONSE_MODE` | `off` / `observe` / `redact` / `block` | `off` |
| `GUARDRAILS_PII_FAIL_OPEN` | `true`/`false` | `false` |
| `GUARDRAILS_HASH_SALT` | salt for `hash` strategy (secret; env-only) | random per boot |
| `GUARDRAILS_ENCRYPTION_KEY` | key for `encrypt` strategy (secret; env-only) | unset (strategy rejected if chosen without it) |
| `GUARDRAILS_INLINE_LIMIT_BYTES` | inline-vs-spawn_blocking threshold | `16384` |

Env sets the global defaults; the config file / admin API can still add
org/team overrides on top, and the config-file global section (when
enabled) wins over the env defaults.

*Implementation note (deviation from the original draft):* the plugin is
**always registered** when the `guardrails` feature is compiled in, and
resolves settings per request against the gateway's live config handle.
This is what lets an admin enable guardrails for an org via
`/admin/config` reload without a restart. With nothing configured the
per-request cost is one config read-lock and an early return.

#### Admin API / dashboard

No new endpoints: the new config fields ride `GET/PUT /admin/config`,
`reload_config`, `rollback_config`, and `config_history` for free. Web
work: extend `OrgGuardrailConfig`/new `PiiGuardrailConfig` types in
`web/lib/api.ts` and add a "Guardrails" card to
`web/app/dashboard/config/page.tsx` (mode/strategy dropdowns, entity
multi-select, confidence slider, response-mode toggle).

### 6.7 Secret patterns and the audit `Redactor`

The five patterns in `himadri-observability/src/redact.rs` (JWT, bearer,
`sk-…`, email, AKIA) are registered into the engine as custom
`PatternRecognizer`s with gateway-namespaced entity types (`GW_JWT`,
`GW_API_KEY`, `GW_AWS_KEY`, `GW_BEARER_TOKEN`), confidence 0.9, so inline
redaction covers credentials, not just classic PII.

The audit `Redactor` itself **stays as-is**: it protects audit logs in
deployments where inline guardrails are off, it has no config surface, and
`himadri-observability` should not grow a redact-core dependency.
Consolidation is possible later; explicitly out of scope.

## 7. Request flow (after this change)

```
client ──▶ auth ──▶ prepare_request
                      ├─ rate limits / budgets / org guardrails / RBAC   (unchanged)
                      └─ before-request plugins
                           1. PiiGuardrailPlugin      ◀── NEW  (mutates ctx.request)
                           2. word filter · max-token · logger · budget  (see redacted text)
            request-of-record := ctx.request           ◀── NEW  (§ 6.4)
        ──▶ response cache get (redacted key)
        ──▶ select_targets ──▶ with_failover ──▶ provider (receives redacted request)
        ──▶ after-request plugins
        ──▶ response guardrails
              PiiResponseGuardrail                     ◀── NEW  (optional)
        ──▶ audit log (redacted messages + guardrail_actions) · request log · metrics · cache insert
```

## 8. Observability

New metrics (registered in `himadri-observability::Metrics`, following
existing naming):

| Metric | Type | Labels |
|---|---|---|
| `himadri_guardrails_pii_detections_total` | counter | `entity_type`, `direction` (`request`/`response`), `action` (`redact`/`block`/`observe`) |
| `himadri_guardrails_requests_blocked_total` | counter | `direction` |
| `himadri_guardrails_scan_duration_seconds` | histogram | `direction` |
| `himadri_guardrails_engine_errors_total` | counter | — |

Audit: `guardrail_actions` entries of the form `pii_redact:
EMAIL_ADDRESS×2,US_SSN×1`, `pii_block: CREDIT_CARD`, `pii_observe: …`.
Tracing: `debug!`-level spans only; **no log line at any level ever
contains a detected value** (the `RedactOutcome` type makes this
structural — original spans/values aren't returned to the plugin).

## 9. Security considerations

- **Secrets handling**: `hash_salt` / `encryption_key` are env-only and
  never enter `Config` (which `GET /admin/config` serializes verbatim —
  same reasoning as `AdminConfig.master_key`, CWE-522).
- **Encrypt strategy** produces reversible tokens ⇒ the key is
  PII-equivalent. Startup refuses `strategy: encrypt` without
  `GUARDRAILS_ENCRYPTION_KEY` and logs the key's fingerprint (blake3
  prefix), never the key.
- **Block-mode error messages** name entity *types* only.
- **Cache/log/audit hygiene** is structural after § 6.4: everything
  downstream of the pipeline sees redacted content.
- **Streaming response caveat** (non-goal N2) must be documented in
  README/configuration.md: response-side guardrails on streams act only at
  stream end; chunks already sent are not recalled.
- **Supply chain**: pinned `=0.8.3`; source-review the crate (~52 KB) and
  its non-workspace deps before enabling `guardrails` in the default
  feature set; re-review on every bump.
- **ReDoS**: recognizers are the crate's own regexes; the § 10 bench
  includes adversarial long-token inputs to check for pathological
  backtracking before ship.

## 10. Performance

- Engine built once at startup (`Arc`); no per-request allocation beyond
  the scan itself. Clean text (no matches) should not reallocate message
  strings — the adapter compares and skips writes when nothing changed.
- Budget: **p99 added latency < 5 ms** for a 4 KiB prompt with the full
  entity set; abort/redesign threshold at 15 ms.
- New Criterion bench `benches/guardrails.rs`: clean 1 KiB / 16 KiB /
  128 KiB prompts, PII-dense prompts, adversarial near-miss inputs;
  measures scan and redact separately. Validates the 16 KiB
  `spawn_blocking` threshold.
- The engine may use rayon internally; if profiling shows worker-pool
  contention under load, cap via `RAYON_NUM_THREADS` guidance in
  DEVELOPMENT.md.

## 11. Testing

- **Unit** (`himadri-plugins`): per-mode behavior (redact/block/observe),
  `MessageContent::Text` vs `Parts`, role filtering, tool-argument scan,
  min-confidence filtering, entity subset, fail-open vs fail-closed,
  metadata/metrics recording, mask/hash/replace output shapes, engine-trait
  mock for error paths.
- **Unit** (`himadri-core`): config (de)serialization round-trip, org/team
  override resolution precedence, `block_pii` deprecation mapping.
- **Gateway** (`crates/himadri/tests`, following `usecase_e2e_tests.rs`
  patterns with the mock provider):
  - request with an email + SSN in `redact` mode → provider-received body
    contains `[EMAIL_ADDRESS]`, not the raw values; response OK.
  - `block` mode → 400 with entity types in the message; audit event has
    `pii_block` action and `GuardrailBlocked` status.
  - `observe` mode → forwarded verbatim; metrics incremented.
  - org override beats global; team beats org.
  - cache: identical prompts differing only in PII hit the same entry
    post-redaction; cached value contains no raw PII.
  - audit/request log assertions: stored messages are the redacted ones.
  - streaming: request-side redaction applies; response-side end-of-stream
    redact/reject behavior on the buffered text.
  - failover: second target receives the same redacted request.
  - plugin-order test: word filter runs on redacted text.
- **Bench**: § 10.

## 12. Rollout plan

| Phase | Contents | Flag state |
|---|---|---|
| **1** | `PiiEngine` trait + redact-core adapter; `PiiGuardrailPlugin` (request side); § 6.4 request-of-record fix; env-var wiring; metrics; unit + e2e tests; bench | `guardrails` feature on by default, plugin **off** unless configured |
| **2** | Config-file schema (global + org/team), admin-reload support, `block_pii` deprecation shim, dashboard UI, configuration.md + ARCHITECTURE.md docs | same |
| **3** | `PiiResponseGuardrail` (response side incl. streaming end-of-stream), secret-pattern recognizers (§ 6.7) | same |
| **later / v2** | embeddings input scanning; reversible de-redaction of responses (Encrypt round-trip); `redact-ner` opt-in; mid-stream windowed scanning; toxicity — planned in detail in [SPEC_GUARDRAILS_V2.md](SPEC_GUARDRAILS_V2.md) | — |

Phase 1 is shippable alone: env-configured global redaction is the core
value. Each phase lands with its tests; the § 6.4 change ships in Phase 1
even though it's a behavior change for hypothetical mutating plugins
(there are none today).

## 13. Open questions

1. **Default `apply_to`** — should `assistant` history messages be scanned
   too? They already transited a provider once, but multi-provider fanout
   means provider B sees PII that only provider A had. Current draft: not
   scanned by default, opt-in via `apply_to`.
2. **`Hash` salt default** — random per boot means hashes aren't stable
   across restarts (fine for redaction, useless for cross-log correlation).
   Require explicit salt when `strategy: hash`? Current draft: random +
   startup warning.
3. **Per-key (API-key-level) overrides** — org/team granularity may not be
   enough for gateway-as-a-product deployments; `AuthContext` has `key_id`
   so plumbing exists. Deferred until a concrete need.
4. **`custom_patterns`** in the existing `ContentFilterConfig` — wire them
   in as user-defined `PatternRecognizer`s in Phase 2, or drop the field
   with `block_pii`? Leaning: wire them in (cheap, and the field is already
   public schema).
5. **Should `observe` mode also annotate the forwarded request** (e.g. an
   `x-himadri-pii-detected` response header) so callers can self-serve
   audit? Not in draft.

## 14. File-level touch list

| File | Change |
|---|---|
| `crates/himadri-plugins/Cargo.toml` | `guardrails` feature, optional pinned `redact-core` |
| `crates/himadri-plugins/src/pii_engine.rs` | **new** — `PiiEngine` trait, types, redact-core adapter, secret-pattern recognizers |
| `crates/himadri-plugins/src/pii_guardrail.rs` | **new** — `PiiGuardrailPlugin`, `PiiResponseGuardrail` |
| `crates/himadri-plugins/src/lib.rs` | module + re-exports (feature-gated) |
| `crates/himadri-core/src/config.rs` | `GuardrailsConfig`/`PiiGuardrailConfig` + enums; `OrgGuardrailConfig.pii`; deprecation shim for `block_pii` |
| `crates/himadri/src/main.rs` | `wire_plugins` gains env-driven registration (needs the gateway's config handle — signature becomes `wire_plugins(config: Arc<RwLock<Config>>, metrics: Arc<Metrics>)`) |
| `crates/himadri/src/gateway/route.rs` | request-of-record fix (§ 6.4) |
| `crates/himadri/src/gateway/stream.rs` | request-of-record fix (§ 6.4) |
| `crates/himadri-plugin/src/traits.rs` | doc comment: before-request plugins may mutate `ctx.request` and the gateway forwards it |
| `crates/himadri-observability/src/lib.rs` (metrics) | four new metrics (§ 8) |
| `crates/himadri/tests/guardrails_e2e_tests.rs` | **new** — § 11 gateway tests |
| `benches/guardrails.rs` | **new** — § 10 |
| `web/lib/api.ts`, `web/app/dashboard/config/page.tsx` | Phase 2 UI |
| `docs/configuration.md`, `ARCHITECTURE.md`, `README.md` | docs incl. streaming caveat |
