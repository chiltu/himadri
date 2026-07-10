FROM rust:1.96-slim AS builder

WORKDIR /app
COPY . .
# BuildKit cache mounts keep the registry and build artifacts across builds,
# so a source-only change doesn't recompile every dependency. The binary is
# copied out because the target dir lives in the (non-exported) cache mount.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release && cp target/release/himadri /usr/local/bin/himadri

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/local/bin/himadri /usr/local/bin/himadri

EXPOSE 8080
ENTRYPOINT ["himadri"]
