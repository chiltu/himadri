# SPEC: Guardrails v2 — Implementation Plan

| | |
|---|---|
| **Status** | Draft |
| **Date** | 2026-07-13 |
| **Depends on** | [SPEC_GUARDRAILS.md](SPEC_GUARDRAILS.md) (v1) |

## 1. Context

Guardrails v1 Phase 1 is implemented: the `PiiEngine` abstraction and
redact-core adapter (`himadri-plugins/src/pii_engine.rs`), the
request-side `PiiGuardrailPlugin` (`pii_guardrail.rs`), the
request-of-record fix in `Gateway::route`/`route_stream`, env-var wiring
(`GUARDRAILS_PII_*` in `wire_plugins`), four Prometheus metrics, and unit +
e2e coverage (`crates/himadri/tests/guardrails_e2e_tests.rs`).

Still open from the **v1 spec** (not v2 work, listed for orientation):

- **Phase 2** — config-file schema (`Config.guardrails` global +
  `OrgConfig`/`TeamConfig` overrides), admin-reload support,
  `ContentFilterConfig.block_pii` deprecation shim, dashboard UI, docs.
- **Phase 3** — `PiiResponseGuardrail` (response side, buffered
  end-of-stream for streams).

This document plans what v1 explicitly deferred (v1 spec §5 non-goals and
§12 "later / v2"), in recommended build order.

## 2. Implementation learnings from Phase 1 that shape v2

These facts about redact-core 0.8.3, discovered while building Phase 1,
constrain the v2 designs below:

1. **One `PatternRecognizer` per engine.** `PatternRecognizer::new()` (and
   `with_name`) always loads the 36 default patterns, and
   `AnalyzerEngine::new()` installs its own instance — a second recognizer
   double-detects everything. Custom patterns must be added to a single
   recognizer installed via `AnalyzerEngine::builder()`. Any v2 feature
   adding recognizers (NER, user patterns) must funnel through the same
   builder path in `RedactCoreEngine::new`.
2. **Entity/confidence filtering is ours, not the engine's.** The engine's
   `anonymize()` re-analyzes unfiltered; the adapter calls `analyze()`,
   filters, then drives `AnonymizerRegistry::anonymize` directly with the
   filtered entity list. v2 features that add detection sources feed the
   same filtered pipeline.
3. **`AnonymizedResult.tokens`** already carries reversible-token metadata
   (`Token { token_id, original_value (encrypted), entity_type, span,
   expires_at }`) for the `Encrypt` strategy — de-redaction (§4) does not
   need upstream changes.
4. **The overlap resolution is in the recognizer registry**, so filtered
   results are already non-overlapping; v2 detection sources that bypass
   the registry (e.g. a separate NER pass) must merge through it, not
   append to its output.

## 3. V2.1 — NER-based detection (`redact-ner`, opt-in)

**Goal**: detect `PERSON` / `ORGANIZATION` / `LOCATION` — the classes regex
cannot express — via the sibling `redact-ner` crate (ONNX runtime,
BERT-style token classification).

**Design**:

- New cargo feature `guardrails-ner` on `himadri-plugins` (implies
  `guardrails`), **off by default**: `ort`/ONNX adds tens of MB to the
  binary and needs a model file at runtime.
- `RedactCoreEngine::new` grows an optional `NerConfig`-shaped parameter:
  `GUARDRAILS_NER_MODEL_PATH` + `GUARDRAILS_NER_TOKENIZER_PATH` +
  `GUARDRAILS_NER_MIN_CONFIDENCE` (default 0.7). When set (and the feature
  is compiled in), a `NerRecognizer` is added to the same
  `RecognizerRegistry` as the pattern recognizer (learning #4: the registry
  merges and overlap-resolves both sources).
- Model distribution: document a `models/` volume-mount convention in the
  Dockerfile/compose files; the gateway never downloads models itself
  (no runtime network fetch — supply-chain and startup-latency reasons).
- **Blocking discipline changes**: NER inference is 2–10 ms per text, an
  order of magnitude above the regex path. When NER is active the plugin
  should *always* `spawn_blocking`, not only above the 16 KiB threshold —
  make the threshold `0` when a NER model is configured.
- Bench gate: extend `benches/guardrails.rs` (once added in v1) with a
  NER variant; abort threshold p99 < 25 ms for a 4 KiB prompt.

**Risks**: `redact-ner` is even younger than redact-core; same pinning +
internal-trait isolation applies. ONNX runtime pulls native code — build it
in CI for both glibc targets before committing to the feature.

## 4. V2.2 — Reversible de-redaction (Encrypt round-trip)

**Goal**: with `strategy: encrypt`, the provider sees `<TOKEN_uuid>`
placeholders; when the model's *response* quotes a token back, the gateway
re-inflates it to the original value before the client sees it. PII then
never reaches the provider yet the client experience is lossless.

**Design**:

- `RedactOutcome` gains `tokens: Option<Vec<PiiToken>>` (mirroring
  redact-core's `Token`, minus `original_value` in plaintext — keep it
  encrypted; learning #3).
- The plugin stores the request's token list in
  `ctx.metadata["guardrails.pii.tokens"]` (encrypted values only — metadata
  flows into logs via the logger plugin, so plaintext is forbidden there).
- A new response-side step (piggybacking on the v1 Phase 3
  `PiiResponseGuardrail`) scans the response for `<TOKEN_[0-9a-f-]{36}>`
  placeholders, decrypts via the engine (`GUARDRAILS_ENCRYPTION_KEY`), and
  substitutes. Unknown tokens pass through untouched.
- **Streaming**: token placeholders can split across chunk boundaries. The
  stream wrapper needs a small holdback buffer (longest-placeholder length,
  ~45 bytes) — cheap, unlike full mid-stream PII scanning; this is
  independent of §5 and can ship without it.
- **Key management**: single symmetric key per deployment in v2.2; key
  rotation = old tokens stop resolving (documented). Per-org keys deferred.
- **Threat note**: de-redaction re-introduces PII into the response path —
  audit logs must record the *redacted* response (order: audit before
  re-inflation).

## 5. V2.3 — Mid-stream response scanning (windowed)

**Goal**: today response guardrails run only on the buffered full text at
stream end (`stream.rs` — chunks already sent cannot be recalled). Provide
a best-effort inline mode that scans a sliding window and withholds
not-yet-emitted content.

**Design sketch**:

- The stream wrapper keeps a holdback window of N bytes (default 256 —
  longer than any pattern the engine matches). Chunks are released only
  once they leave the window; the tail is scanned each flush.
- On detection: in `redact` mode, rewrite within the withheld window and
  continue; in `block` mode, terminate the stream with an error chunk.
- Latency cost: one window of added time-to-last-token; time-to-first-token
  unchanged unless the first chunk is smaller than the window. Config:
  `guardrails.pii.stream_window_bytes` (0 = keep today's end-of-stream
  behavior, the default).
- **Honest limits** (document loudly): entities spanning beyond the window
  are missed; NER (§3) is too slow per-flush and stays end-of-stream-only.
- This is the most complex v2 item (stateful stream rewriting under
  backpressure) — build last, behind its own config flag.

## 6. V2.4 — Embeddings input scanning

**Goal**: `Gateway::embed` bypasses the plugin pipeline entirely
(`route.rs`), so embedding inputs leave unscanned today.

**Design**: embeddings inputs are not chat messages, so rather than force
them through `PluginContext`, call the engine directly in `embed()` between
the RBAC check and target iteration: resolve settings, scan/redact
`request.input` (string or array-of-strings forms), and honor
block/observe/redact identically. Metrics label `direction="embedding"`.
Small, self-contained; good first v2 item if prioritized by demand.

## 7. V2.5 — Governance extensions

Bundled smaller items, each mostly config plumbing on top of the v1 Phase 2
schema:

1. **Per-API-key overrides** — `AuthContext` already carries `key_id`;
   add `pii: Option<PiiGuardrailConfig>` to the API-key record
   (himadri-admin store + admin API + UI). Resolution order becomes
   key > team > org > global.
2. **User-defined custom patterns** — wire the existing (currently dead)
   `ContentFilterConfig.custom_patterns` as `Custom(<name>)` entities added
   to the single pattern recognizer (learning #1). Because the engine is
   built once at startup, org-level *runtime-added* patterns require an
   engine rebuild on config reload — rebuild the `Arc<RedactCoreEngine>`
   on `reload_config` when (and only when) the pattern set changed.
   Validate regexes at config-accept time and reject the reload on error
   (mirrors the fail-closed posture).
3. **Toxicity / content classification** — out of scope for redact-core
   (it detects entities, not semantics). If demanded, it is a separate
   `PiiEngine`-style trait with an ONNX classifier backend; do not overload
   the PII pipeline. Keep `block_toxicity` deprecated until then.

## 8. Explicit non-goals (still)

- `/v1/*` catch-all proxy scanning — raw pass-through bodies are not
  parsed; scanning would require understanding every upstream's schema.
  Remains a documented limitation.
- Image/audio content scanning (`ContentPart::ImageUrl`).
- Per-org encryption keys and token vaults (revisit with v2.2 feedback).

## 9. Suggested sequencing

| Order | Item | Why this order |
|---|---|---|
| 1 | v1 Phase 2 (config schema + org overrides + UI) | Prerequisite for every per-scope v2 feature |
| 2 | v1 Phase 3 (`PiiResponseGuardrail`) | Prerequisite for de-redaction and mid-stream work |
| 3 | §6 embeddings scanning | Small, closes a real leak |
| 4 | §7.1 per-key overrides + §7.2 custom patterns | Config plumbing while schema work is fresh |
| 5 | §4 de-redaction | Highest user value of the big items; bounded complexity |
| 6 | §3 NER | Binary/model logistics; value depends on PERSON-type demand |
| 7 | §5 mid-stream scanning | Most complex, weakest guarantees — last |

Each item ships behind config that defaults to current behavior; nothing in
v2 changes v1 semantics for existing deployments.
