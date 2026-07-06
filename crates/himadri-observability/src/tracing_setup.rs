use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Initialize tracing/logging to the console.
///
/// `service_name`, `endpoint` and `sample_ratio` are accepted so the
/// signature stays stable for a future OTLP/exporter integration, but are
/// not currently used (the previous OTLP path never compiled and was removed).
pub fn init_tracing(_service_name: &str, _endpoint: Option<&str>, _sample_ratio: f64) {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "himadri=info,tower_http=info".into());

    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer())
        .init();

    tracing::info!("Tracing initialized (console output only)");
}

pub fn shutdown_tracing() {}
