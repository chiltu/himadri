# AGENTS.md

**himadri** — an OpenAI-compatible AI gateway in Rust that proxies many LLM providers, adding auth, routing strategies, rate limiting, plugins/guardrails, circuit breaking, and observability. A Next.js admin dashboard lives in `web/`.

## Layout

Cargo workspace (`crates/`):

- `himadri` — the binary: axum server (`main.rs` wiring + `handlers.rs` HTTP handlers) + `Gateway` orchestrator (`gateway.rs`, `strategy.rs`)
- `himadri-core` — shared types, config, errors
- `himadri-provider` — `Provider` trait; OpenAI-shaped vendors (openai, azure, openrouter, groq, …) are config presets of `compatible::OpenAiCompatibleProvider`; bespoke impls only for anthropic, gemini, bedrock; shared SSE decoding in `sse.rs`
- `himadri-plugin` / `himadri-plugins` — plugin traits + impls (budget, cache, logger, max_token, rate_limit, word_filter)
- `himadri-admin` — key/provider/model CRUD, usage & request-log stores (in-memory / Postgres / SQLite), auth middleware
- `himadri-auth` — JWT/OIDC/OAuth2 (not currently wired into the binary)
- `himadri-ratelimit`, `himadri-circuitbreaker`, `himadri-observability`

## Commands

```bash
cargo build
cargo test                 # workspace tests
cargo clippy --all-targets
cargo fmt
cargo run -p himadri       # starts server on PORT (default 8080)
```

Web (in `web/`): `npm run dev` / `npm run build` / `npm run lint` / `npm run typecheck`.

## Notes

- `MASTER_KEY` gates auth; if unset, all auth is bypassed (dev only).
- Providers register from env vars in `crates/himadri/src/main.rs`; routing targets also rebuild live via `/admin/*` endpoints.
- `web/` is Next.js 16 / React 19 with breaking changes from older versions — see `web/AGENTS.md` and read `node_modules/next/dist/docs/` before editing frontend code.
</content>
</invoke>
