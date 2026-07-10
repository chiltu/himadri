use prometheus::{
    Encoder, Histogram, HistogramOpts, IntCounter, IntCounterVec, IntGauge, Opts, Registry,
    TextEncoder,
};

#[derive(Clone)]
pub struct Metrics {
    pub registry: Registry,
    pub requests_total: IntCounterVec,
    pub request_duration: Histogram,
    pub tokens_input_total: IntCounterVec,
    pub tokens_output_total: IntCounterVec,
    pub provider_errors: IntCounterVec,
    pub cost_usd_total: IntCounterVec,
    pub rate_limit_rejections: IntCounter,
    pub circuit_breaker_state: IntGauge,
    pub active_connections: IntGauge,
    pub cache_hits_total: IntCounter,
    pub cache_misses_total: IntCounter,
}

impl Metrics {
    pub fn new() -> Self {
        let registry = Registry::new();

        let requests_total = IntCounterVec::new(
            Opts::new("himadri_requests_total", "Total number of requests"),
            &["provider", "model"],
        )
        .expect("static metric definition is valid");

        let request_duration = Histogram::with_opts(
            HistogramOpts::new(
                "himadri_request_duration_seconds",
                "Request duration in seconds",
            )
            .buckets(vec![
                0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
            ]),
        )
        .expect("static metric definition is valid");

        let tokens_input_total = IntCounterVec::new(
            Opts::new("himadri_tokens_input_total", "Total input tokens"),
            &["provider", "model"],
        )
        .expect("static metric definition is valid");

        let tokens_output_total = IntCounterVec::new(
            Opts::new("himadri_tokens_output_total", "Total output tokens"),
            &["provider", "model"],
        )
        .expect("static metric definition is valid");

        let provider_errors = IntCounterVec::new(
            Opts::new("himadri_provider_errors_total", "Total provider errors"),
            &["provider", "model"],
        )
        .expect("static metric definition is valid");

        let cost_usd_total = IntCounterVec::new(
            Opts::new(
                "himadri_cost_usd_total",
                "Total cost in USD (stored as micro-USD)",
            ),
            &["provider", "model"],
        )
        .expect("static metric definition is valid");

        let rate_limit_rejections = IntCounter::with_opts(Opts::new(
            "himadri_rate_limit_rejections_total",
            "Total rate limit rejections",
        ))
        .expect("static metric definition is valid");

        let circuit_breaker_state = IntGauge::with_opts(Opts::new(
            "himadri_circuit_breaker_state",
            "Circuit breaker state (0=closed, 1=open, 2=half_open)",
        ))
        .expect("static metric definition is valid");

        let active_connections = IntGauge::with_opts(Opts::new(
            "himadri_active_connections",
            "Number of active connections",
        ))
        .expect("static metric definition is valid");

        let cache_hits_total = IntCounter::with_opts(Opts::new(
            "himadri_cache_hits_total",
            "Total response cache hits",
        ))
        .expect("static metric definition is valid");

        let cache_misses_total = IntCounter::with_opts(Opts::new(
            "himadri_cache_misses_total",
            "Total response cache misses",
        ))
        .expect("static metric definition is valid");

        let collectors: [Box<dyn prometheus::core::Collector>; 11] = [
            Box::new(requests_total.clone()),
            Box::new(request_duration.clone()),
            Box::new(tokens_input_total.clone()),
            Box::new(tokens_output_total.clone()),
            Box::new(provider_errors.clone()),
            Box::new(cost_usd_total.clone()),
            Box::new(rate_limit_rejections.clone()),
            Box::new(circuit_breaker_state.clone()),
            Box::new(active_connections.clone()),
            Box::new(cache_hits_total.clone()),
            Box::new(cache_misses_total.clone()),
        ];
        // The registry is created fresh above, so registration can only fail
        // on a duplicate metric name within this constructor itself.
        for collector in collectors {
            registry
                .register(collector)
                .expect("metric already registered on a fresh registry");
        }

        Self {
            registry,
            requests_total,
            request_duration,
            tokens_input_total,
            tokens_output_total,
            provider_errors,
            cost_usd_total,
            rate_limit_rejections,
            circuit_breaker_state,
            active_connections,
            cache_hits_total,
            cache_misses_total,
        }
    }

    pub fn encode_metrics(&self) -> String {
        let encoder = TextEncoder::new();
        let metric_families = self.registry.gather();
        let mut buffer = Vec::new();
        encoder
            .encode(&metric_families, &mut buffer)
            .expect("text-encoding gathered metrics into a Vec cannot fail");
        String::from_utf8(buffer).expect("prometheus text format is UTF-8")
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}
