use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

pub fn init_tracing(_service_name: &str, _endpoint: Option<&str>, _sample_ratio: f64) {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "himadri=info,tower_http=info".into());

    #[cfg(feature = "otlp")]
    {
        if let Some(ep) = endpoint {
            match init_otlp(service_name, ep, sample_ratio) {
                Ok(telemetry) => {
                    tracing_subscriber::registry()
                        .with(env_filter)
                        .with(tracing_subscriber::fmt::layer())
                        .with(telemetry)
                        .init();
                    tracing::info!("Tracing initialized with OTLP exporter at {}", ep);
                    return;
                }
                Err(e) => {
                    tracing::warn!("Failed to init OTLP: {}, falling back to console", e);
                }
            }
        }
    }

    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer())
        .init();

    tracing::info!("Tracing initialized (console output only)");
}

#[cfg(feature = "otlp")]
fn init_otlp(
    service_name: &str,
    endpoint: &str,
    sample_ratio: f64,
) -> Result<
    tracing_opentelemetry::OpenTelemetryLayer<
        tracing_subscriber::Registry,
        opentelemetry_sdk::trace::Tracer,
    >,
    Box<dyn std::error::Error>,
> {
    use opentelemetry::global;
    use opentelemetry_otlp::WithExportConfig;
    use opentelemetry_sdk::trace::TracerProvider;

    let exporter = opentelemetry_otlp::new_exporter()
        .tonic()
        .with_endpoint(endpoint);

    let provider = TracerProvider::builder()
        .with_batch_exporter(exporter, opentelemetry_sdk::runtime::Tokio)
        .with_resource(opentelemetry_sdk::Resource::new(vec![
            opentelemetry::KeyValue::new("service.name", service_name.to_string()),
        ]))
        .with_config(opentelemetry_sdk::trace::Config::default().with_sampler(
            opentelemetry_sdk::trace::Sampler::ParentBased(
                opentelemetry_sdk::trace::Sampler::TraceIdRatioBased(sample_ratio),
            ),
        ))
        .build();

    let tracer = provider.tracer(service_name);
    global::set_tracer_provider(provider);

    Ok(tracing_opentelemetry::layer().with_tracer(tracer))
}

pub fn shutdown_tracing() {
    #[cfg(feature = "otlp")]
    {
        opentelemetry::global::shutdown_tracer_provider();
    }
}
