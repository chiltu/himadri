# himadri — Domain Glossary

Names for the concepts the code is organized around. Architecture reviews and
refactors should use these terms; if a change coins a new load-bearing concept,
add it here.

## Routing

- **Target** — one routable (provider, credential, model-list) binding. The
  live target list is what the strategy selects from. Config/env deployments
  supply targets in the config document; DB deployments derive them from
  models × endpoints.
- **Model** — the first-party entity a client requests by name. Owns one or
  more **Model Endpoints** (provider routes). A model with no enabled endpoint
  is inactive.
- **Model Endpoint** — one provider route for a model: `provider_type`,
  optional `base_url`, optional encrypted `api_key`, weight. Each enabled
  endpoint of an enabled model becomes one target, keyed by endpoint id.

## Provider construction

- **Provider Registry** — the single seam from a `provider_type` name to a
  live provider client (`himadri-provider/src/registry.rs`, trait
  `ProviderRegistry`, map-backed `MapProviderRegistry`). Provider modules
  self-register via their `register()` functions; the wire-up
  (`himadri/src/wire/providers.rs`) decides which are included. The admin API
  validates endpoints against it at creation; the rebuild builds clients
  through it. `KNOWN_PROVIDER_TYPES` (himadri-core) is the *advertised* list
  (UI picker, `GET /admin/known-providers`) and is drift-guarded in both
  directions against the registry.
- **Preset vendor** — an OpenAI-compatible vendor with a built-in config
  (`compatible::PRESETS`): one table row enables it in both ENV and DB modes.
  Unregistered types are still routable with an explicit `base_url` via a
  generic Bearer client.

## Rebuild

- **Rebuild** — recomputing the live targets, provider clients, keys, and
  breaker set from the DB's models/endpoints
  (`Gateway::rebuild_targets_from_db`). Computes everything before mutating
  any live state; the key repopulation is insert-then-retain so no in-flight
  request ever sees a missing key.
- **Rebuild Outcome** — what a rebuild reports: `targets_built`, the
  `skipped` endpoints with reasons, and whether it `applied`. Callers decide
  from this fact; there is deliberately no separate predicate predicting it
  (the old `db_has_active_targets` guard could disagree with the rebuild and
  wipe routing).
- **On-Empty policy** — who is the authority when a rebuild computes zero
  targets. After an admin mutation the DB is (`OnEmpty::Apply`: empty means
  empty); at startup and after a config apply the DB only takes over when it
  produces targets (`OnEmpty::KeepPrevious`: env/file targets stand).

## Composition

- **Wire module** (`himadri/src/wire/`) — the composition roots: the places
  that decide *what* the gateway is assembled from, separated from the code
  that does the assembling. `wire::providers` decides which provider types
  register; `wire::plugins` decides what env-driven processing runs on every
  request (plugin pipeline + response cache), as pure composition over a
  [`PluginSettings`] struct — `PluginSettings::from_env()` is the single
  place plugin env vars are read, and the one inventory of them.
- **Fail-closed guardrail boot** (SPEC §6.3) — if PII guardrails are
  configured (env defaults or any config `pii` section, *even a disabled
  one*) and the engine fails to build, startup panics rather than silently
  running unguarded. Lives in `wire::plugins::build`.

## Scoped policy

- **Scope** — one level of the org hierarchy a request-policy rule can live
  at: org or team (teams exist only under their org). A request's **scope
  chain** is resolved once by `Config::scopes(org_id, team_id)`
  (himadri-core `scope.rs`), least→most specific; all scoped checks consume
  it rather than walking `config.orgs` themselves.
- **Cumulative rule** — a rule every scope in the chain enforces
  independently (model allow/block lists, token budgets, blocked words,
  max_tokens). A team can only narrow the org. Each scope's
  `guardrails.enabled` gates only that scope's own contribution.
- **Wholesale override** — the PII exception: the most specific scope with a
  `guardrails.pii` section decides entirely (including `enabled: false` to
  opt out of a global policy); resolution falls back global → env defaults
  at the plugin.

## Provider-routing source

- **Routing source** — which side provides routing targets, decided from the
  startup **Rebuild Outcome** (facts, not prediction) and announced with one
  `Provider routing: …` log line every boot. `DATABASE_URL` is a *storage*
  decision (API keys, usage, logs) and never by itself decides routing —
  the documented quick start sets it alongside `OPENAI_API_KEY`.
- **Auto** (default) — env/config targets route until the database produces
  targets; then the DB owns routing, the boot warns naming any provider env
  vars that are set but inert, and those vars remain the routing *fallback*
  if the DB stops producing targets (`OnEmpty::KeepPrevious`). The empty-DB
  state is the onboarding path, logged as such.
- **Strict db** (`HIMADRI_PROVIDER_SOURCE=db`) — asserts "never route with
  env-configured providers": env provider registration is skipped wholesale.
  Boot fails fast without `DATABASE_URL` or on an unrecognized value (a typo
  must not silently mean auto). Decision lives in `himadri/src/wire/mode.rs`.
