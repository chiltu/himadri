//! Tracing/logging setup with an optional OpenTelemetry OTLP span exporter.
//!
//! Behavior is entirely runtime-driven by [`TracingConfig`]:
//!
//! * `enabled == false` — console (`fmt`) output only; no OTLP pipeline is
//!   constructed. This is the default.
//! * `enabled == true` — spans are additionally exported over **OTLP/gRPC** to
//!   an OpenTelemetry Collector, using a batch processor. If the exporter
//!   cannot be constructed we log a `WARN` and fall back to console-only rather
//!   than refusing to boot — observability must never take down the data plane.
//!
//! Only the **traces** signal is implemented here. Metrics are served
//! separately via Prometheus (see [`crate::metrics`]); OTLP metrics/logs are
//! intentionally out of scope.
//!
//! ## Endpoint precedence
//! `config.endpoint` (when `Some`) → `OTEL_EXPORTER_OTLP_ENDPOINT` →
//! `http://localhost:4317` (the OTLP/gRPC default).
//!
//! ## Transport security
//! Scheme-driven: an `http://` endpoint is plaintext; an `https://` endpoint
//! uses TLS with system/webpki roots (rustls — never OpenSSL). Client-cert
//! mTLS is not configured.
//!
//! ## Distributed propagation
//! Only local spans are exported today, but the global W3C `TraceContext`
//! propagator is installed and the sampler is parent-aware, so inbound-extract
//! / outbound-inject middleware can be added later without revisiting init.
//! See the `propagation seam:` comments in `himadri` (the axum layer stack and
//! the provider HTTP client builder).

use himadri_core::config::TracingConfig;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig as _;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};
use opentelemetry_sdk::Resource;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Held by `main` for the lifetime of the process. Dropping it (or calling
/// [`TracingGuard::shutdown`]) force-flushes and shuts down the batch span
/// processor so buffered spans are exported on graceful shutdown. Inert when
/// tracing is disabled or fell back to console-only (nothing to flush).
#[must_use = "hold the guard for the process lifetime; dropping it flushes spans"]
pub struct TracingGuard {
    provider: Option<SdkTracerProvider>,
}

impl TracingGuard {
    fn inert() -> Self {
        Self { provider: None }
    }

    /// True when an OTLP exporter pipeline is installed; false when tracing is
    /// disabled or fell back to console-only.
    pub fn is_active(&self) -> bool {
        self.provider.is_some()
    }

    /// Force-flush and shut down the tracer provider. Idempotent and safe to
    /// call on an inert guard. The batch processor honors the standard
    /// `OTEL_BSP_EXPORT_TIMEOUT` bound, so a slow/hung collector cannot stall
    /// process exit indefinitely.
    pub fn shutdown(mut self) {
        self.shutdown_inner();
    }

    fn shutdown_inner(&mut self) {
        if let Some(provider) = self.provider.take() {
            if let Err(e) = provider.shutdown() {
                tracing::warn!("OpenTelemetry tracer shutdown error (spans may be dropped): {e}");
            }
        }
    }
}

impl Drop for TracingGuard {
    fn drop(&mut self) {
        // Safety net if `main` never calls `shutdown()` explicitly.
        self.shutdown_inner();
    }
}

/// Initialize tracing/logging. Installs a console `fmt` layer always, plus an
/// OTLP/gRPC exporter layer when `cfg.enabled` is true. Returns a
/// [`TracingGuard`] the caller must hold for the process lifetime.
pub fn init_tracing(cfg: &TracingConfig) -> TracingGuard {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "himadri=info,tower_http=info".into());
    let fmt_layer = tracing_subscriber::fmt::layer();

    if !cfg.enabled {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(fmt_layer)
            .init();
        tracing::info!("Tracing initialized (console output only; OTLP disabled)");
        return TracingGuard::inert();
    }

    match build_otlp_provider(cfg) {
        Ok(provider) => {
            // Install the global W3C TraceContext propagator now so that, when
            // inbound-extract / outbound-inject middleware is added later, the
            // parent-aware sampler already does the right thing.
            opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());

            let tracer = provider.tracer("himadri");
            let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);

            tracing_subscriber::registry()
                .with(env_filter)
                .with(fmt_layer)
                .with(otel_layer)
                .init();

            // Register globally so any direct OpenTelemetry API usage shares the
            // same provider; the guard still owns flush/shutdown.
            opentelemetry::global::set_tracer_provider(provider.clone());

            tracing::info!(
                service.name = %cfg.service_name,
                sample_ratio = clamp_ratio(cfg.sample_ratio),
                "Tracing initialized with OTLP/gRPC exporter"
            );
            TracingGuard {
                provider: Some(provider),
            }
        }
        Err(e) => {
            // Log-and-continue: fall back to console-only so a misconfigured
            // collector endpoint cannot prevent the gateway from serving.
            tracing_subscriber::registry()
                .with(env_filter)
                .with(fmt_layer)
                .init();
            tracing::warn!(
                "OTLP tracing exporter setup failed ({e}); falling back to console-only tracing"
            );
            TracingGuard::inert()
        }
    }
}

/// Build the OTLP/gRPC-backed tracer provider (batch processor).
fn build_otlp_provider(
    cfg: &TracingConfig,
) -> Result<SdkTracerProvider, Box<dyn std::error::Error>> {
    // Endpoint precedence: explicit config wins; otherwise let the SDK read
    // OTEL_EXPORTER_OTLP_ENDPOINT and fall back to its localhost:4317 default.
    let mut exporter_builder = opentelemetry_otlp::SpanExporter::builder().with_tonic();
    if let Some(endpoint) = cfg.endpoint.as_deref().filter(|e| !e.is_empty()) {
        // TLS is scheme-driven by tonic (https => rustls/webpki roots).
        exporter_builder = exporter_builder.with_endpoint(endpoint);
    }
    let exporter = exporter_builder.build()?;

    Ok(SdkTracerProvider::builder()
        .with_resource(build_resource(cfg))
        .with_sampler(build_sampler(cfg))
        .with_batch_exporter(exporter)
        .build())
}

/// Build the OTel `Resource`. `Resource::builder()` folds in
/// `OTEL_RESOURCE_ATTRIBUTES` and default detectors; we then stamp
/// `service.name` (from config) and `service.version` (from the crate).
fn build_resource(cfg: &TracingConfig) -> Resource {
    Resource::builder()
        .with_service_name(cfg.service_name.clone())
        .with_attribute(opentelemetry::KeyValue::new(
            "service.version",
            env!("CARGO_PKG_VERSION"),
        ))
        .build()
}

/// Parent-aware ratio sampler: defer to an extracted parent's decision when
/// present (see the propagation seam), otherwise sample the configured fraction
/// of root traces.
fn build_sampler(cfg: &TracingConfig) -> Sampler {
    Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(clamp_ratio(
        cfg.sample_ratio,
    ))))
}

/// Clamp a configured sample ratio into `[0.0, 1.0]`; NaN is treated as fully
/// sampled so a bad value never silently drops all traces.
fn clamp_ratio(ratio: f64) -> f64 {
    if ratio.is_nan() {
        1.0
    } else {
        ratio.clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_sdk::trace::InMemorySpanExporter;
    use tracing::subscriber::with_default;
    use tracing_subscriber::layer::SubscriberExt;

    fn cfg(enabled: bool, sample_ratio: f64) -> TracingConfig {
        TracingConfig {
            enabled,
            service_name: "himadri-test".to_string(),
            endpoint: None,
            sample_ratio,
        }
    }

    #[test]
    fn clamp_ratio_bounds() {
        assert_eq!(clamp_ratio(1.5), 1.0);
        assert_eq!(clamp_ratio(-0.1), 0.0);
        assert_eq!(clamp_ratio(0.25), 0.25);
        assert_eq!(clamp_ratio(f64::NAN), 1.0);
    }

    #[test]
    fn resource_carries_service_name_and_version() {
        let resource = build_resource(&cfg(true, 1.0));
        let attrs: std::collections::HashMap<String, String> = resource
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        assert_eq!(attrs.get("service.name").map(String::as_str), Some("himadri-test"));
        assert_eq!(
            attrs.get("service.version").map(String::as_str),
            Some(env!("CARGO_PKG_VERSION"))
        );
    }

    /// Drive an instrumented unit of work under a scoped (non-global)
    /// subscriber wired to an in-memory exporter, then return the captured
    /// spans. Uses `with_default` so tests never touch the global subscriber.
    fn capture_spans(sampler: Sampler, work: impl FnOnce()) -> Vec<String> {
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_resource(build_resource(&cfg(true, 1.0)))
            .with_sampler(sampler)
            .with_simple_exporter(exporter.clone())
            .build();
        let tracer = provider.tracer("himadri-test");
        let subscriber =
            tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer));

        with_default(subscriber, work);

        provider.force_flush().expect("flush");
        exporter
            .get_finished_spans()
            .expect("finished spans")
            .into_iter()
            .map(|s| {
                // Confirm the instrumented field rode along on captured spans.
                if s.name == "unit_of_work" {
                    let has_model = s
                        .attributes
                        .iter()
                        .any(|kv| kv.key.as_str() == "model" && kv.value.as_str() == "test-model");
                    assert!(has_model, "span should carry the `model` field");
                }
                s.name.into_owned()
            })
            .collect()
    }

    #[test]
    fn full_ratio_keeps_span_with_fields() {
        let names = capture_spans(build_sampler(&cfg(true, 1.0)), || {
            let span = tracing::info_span!("unit_of_work", model = "test-model");
            let _e = span.enter();
        });
        assert!(
            names.iter().any(|n| n == "unit_of_work"),
            "ratio 1.0 must export the root span, got {names:?}"
        );
    }

    #[test]
    fn zero_ratio_drops_root_span() {
        let names = capture_spans(build_sampler(&cfg(true, 0.0)), || {
            let span = tracing::info_span!("unit_of_work", model = "test-model");
            let _e = span.enter();
        });
        assert!(
            names.is_empty(),
            "ratio 0.0 must drop root spans, got {names:?}"
        );
    }

    #[test]
    fn disabled_config_returns_inert_guard() {
        // This is the only test that installs the global subscriber; keep it so.
        let guard = init_tracing(&cfg(false, 1.0));
        assert!(!guard.is_active(), "disabled tracing must not build an OTLP pipeline");
        guard.shutdown(); // must not panic on an inert guard
    }
}
