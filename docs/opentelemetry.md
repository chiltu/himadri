# OpenTelemetry Tracing

himadri can export **distributed tracing spans** over **OTLP/gRPC** to an
[OpenTelemetry Collector](https://opentelemetry.io/docs/collector/). This is the
`traces` signal only — metrics remain on the Prometheus endpoint (see
[Configuration → metrics](configuration.md#guardrails--observability)); OTLP
metrics and logs are intentionally out of scope.

Tracing is **off by default**. When enabled, the spans already instrumented
across the request path (routing, provider calls, plugin/guardrail execution)
are batched and shipped to the collector; when disabled, only console log
output is produced.

- [Quick start](#quick-start)
- [Configuration](#configuration)
- [Endpoint resolution](#endpoint-resolution)
- [Transport security (TLS)](#transport-security-tls)
- [Sampling](#sampling)
- [Resource attributes](#resource-attributes)
- [Collector example](#collector-example)
- [Verifying it works](#verifying-it-works)
- [Distributed propagation (not yet wired)](#distributed-propagation-not-yet-wired)
- [Design notes](#design-notes)

## Quick start

Enable tracing in the JSON config file and point it at a collector:

```json
{
  "observability": {
    "tracing": {
      "enabled": true,
      "service_name": "himadri",
      "endpoint": "http://localhost:4317",
      "sample_ratio": 1.0
    },
    "metrics": {
      "enabled": true,
      "path": "/metrics"
    }
  }
}
```

Start an OTLP-capable collector on `:4317`, boot the gateway, send a request,
and traces appear in whatever backend the collector forwards to (Jaeger, Tempo,
Honeycomb, Datadog, …).

## Configuration

The tracing block lives under `observability.tracing` in the JSON config file:

| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `false` | Master switch. When `false`, no OTLP pipeline is built and only console output is emitted. |
| `service_name` | string | `"himadri"` | Value of the `service.name` resource attribute on every span. |
| `endpoint` | string \| null | `null` | OTLP/gRPC collector endpoint. `null` defers to env/default — see below. |
| `sample_ratio` | float | `1.0` | Fraction of root traces to sample, `[0.0, 1.0]`. Out-of-range values are clamped; `NaN` is treated as `1.0`. |

Log verbosity is controlled independently by the standard `RUST_LOG` /
`EnvFilter` mechanism (default `himadri=info,tower_http=info`).

> **Failure is non-fatal.** If `enabled: true` but the exporter cannot be
> constructed (bad endpoint, TLS misconfig), the gateway logs a `WARN`, falls
> back to console-only tracing, and boots normally. Observability never takes
> down the data plane. A collector that is simply *down* is also non-fatal —
> the batch exporter drops spans and logs export errors until the collector
> returns.

## Endpoint resolution

The collector endpoint is resolved with this precedence:

1. **`observability.tracing.endpoint`** from the config file, when set.
2. **`OTEL_EXPORTER_OTLP_ENDPOINT`** (or `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`)
   environment variable, read natively by the OpenTelemetry SDK.
3. **`http://localhost:4317`** — the OTLP/gRPC default.

This keeps the config file authoritative when it declares an endpoint, while
still supporting the standard env-var story used by collector sidecars.

## Transport security (TLS)

TLS is **scheme-driven** by the endpoint URL:

- `http://…:4317` → plaintext (typical for a local/sidecar collector).
- `https://…:4317` → TLS using the system / webpki root certificates.

The TLS backend is **rustls** — himadri never links OpenSSL. Client-certificate
mTLS is not currently configurable.

## Sampling

The sampler is `ParentBased(TraceIdRatioBased(sample_ratio))`:

- For **root** traces (no incoming parent), a deterministic `sample_ratio`
  fraction is kept (`1.0` = everything, `0.0` = nothing).
- When a parent context is present, its sampling decision is respected. This
  makes the sampler already correct for the day inbound trace propagation is
  wired (see below).

`sample_ratio` is clamped to `[0.0, 1.0]`; `NaN` falls back to `1.0` so a bad
value never silently drops all traces.

## Resource attributes

Every span carries a `Resource` with:

- `service.name` — from `observability.tracing.service_name`.
- `service.version` — the himadri crate version (`CARGO_PKG_VERSION`).
- anything supplied via the standard **`OTEL_RESOURCE_ATTRIBUTES`** environment
  variable, e.g. `OTEL_RESOURCE_ATTRIBUTES=deployment.environment=prod,service.instance.id=pod-7`.

Stamping host/environment attributes is best done at the collector (via its
`resource` processor) or through `OTEL_RESOURCE_ATTRIBUTES`, rather than adding
config fields here.

## Collector example

A minimal OpenTelemetry Collector config that accepts OTLP/gRPC and logs traces
(swap the `debug` exporter for your real backend):

```yaml
receivers:
  otlp:
    protocols:
      grpc:
        endpoint: 0.0.0.0:4317

exporters:
  debug:
    verbosity: detailed

service:
  pipelines:
    traces:
      receivers: [otlp]
      exporters: [debug]
```

## Verifying it works

1. Run the collector above (e.g. `otelcol --config collector.yaml`).
2. Boot himadri with `observability.tracing.enabled = true` and
   `endpoint = "http://localhost:4317"`.
3. On startup the gateway logs
   `Tracing initialized with OTLP/gRPC exporter`.
4. Send a request; within a few seconds spans appear in the collector's `debug`
   output (or your trace backend). Look for the per-request span, plus routing
   and upstream-provider child spans on `/v1` inference calls.

If instead you see
`OTLP tracing exporter setup failed (…); falling back to console-only tracing`,
the endpoint or TLS settings are wrong — the gateway keeps serving regardless.

On graceful shutdown the gateway force-flushes buffered spans (bounded by the
standard `OTEL_BSP_EXPORT_TIMEOUT`), so the final batch is not lost.

## What spans you'll see (RUST_LOG interaction)

Exported traces are exactly the `tracing` spans that pass the active
`EnvFilter` (`RUST_LOG`, default `himadri=info,tower_http=info`) — the OTLP
layer never sees a span the filter suppressed. This has one operator-visible
consequence:

| Span | Emitted at | Seen with default filter? |
|---|---|---|
| himadri routing / provider-call spans (`#[instrument]`) | `INFO` | ✅ yes |
| `tower_http` per-request span (method, uri, status, latency) | `DEBUG` | ❌ no |

So with the default filter you get the gateway's own routing/provider spans on
`/v1` inference requests, but **not** the generic per-request HTTP span. To
capture the per-request span (and its `status`/`latency` fields) for every
endpoint, raise the filter for that target, e.g.:

```
RUST_LOG=himadri=info,tower_http=debug
```

Raising `tower_http` to `debug` only affects which spans/events are *created*;
sampling (`sample_ratio`) still governs what is ultimately exported.

## Distributed propagation (not yet wired)

Only **local spans** are exported today. The global **W3C `TraceContext`**
propagator is installed and the sampler is parent-aware, so two future
additions turn on end-to-end propagation without revisiting init:

- **Inbound extract** — a layer in the axum stack that reads the `traceparent`
  header from the incoming request and sets it as the span's parent.
- **Outbound inject** — middleware on the provider HTTP client that writes
  `traceparent` into upstream calls.

Both touchpoints are marked with `propagation seam:` comments in the source
(`crates/himadri/src/main.rs` and
`crates/himadri-provider/src/http_client.rs`).

## Design notes

- The OTLP stack is **always compiled**; the `enabled` flag is a pure runtime
  switch, so there is only one binary and no build that "can't emit traces".
- Spans are shipped with a **batch** processor on the Tokio runtime; tunable
  via the standard `OTEL_BSP_*` environment variables.
- Batch-processor internals and export errors are surfaced through the normal
  `tracing` log output.
