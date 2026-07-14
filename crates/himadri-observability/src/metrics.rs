use prometheus::{
    Encoder, Histogram, HistogramOpts, HistogramVec, IntCounter, IntCounterVec, IntGauge, Opts,
    Registry, TextEncoder,
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
    pub guardrail_pii_detections_total: IntCounterVec,
    pub guardrail_blocked_total: IntCounterVec,
    pub guardrail_scan_duration: HistogramVec,
    pub guardrail_engine_errors_total: IntCounter,
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

        let guardrail_pii_detections_total = IntCounterVec::new(
            Opts::new(
                "himadri_guardrails_pii_detections_total",
                "PII entities detected by guardrails (types/counts only, never values)",
            ),
            &["entity_type", "direction", "action"],
        )
        .expect("static metric definition is valid");

        let guardrail_blocked_total = IntCounterVec::new(
            Opts::new(
                "himadri_guardrails_requests_blocked_total",
                "Requests/responses blocked by PII guardrails",
            ),
            &["direction"],
        )
        .expect("static metric definition is valid");

        let guardrail_scan_duration = HistogramVec::new(
            HistogramOpts::new(
                "himadri_guardrails_scan_duration_seconds",
                "PII guardrail scan duration in seconds",
            )
            .buckets(vec![0.0001, 0.0005, 0.001, 0.005, 0.01, 0.05, 0.1, 0.5]),
            &["direction"],
        )
        .expect("static metric definition is valid");

        let guardrail_engine_errors_total = IntCounter::with_opts(Opts::new(
            "himadri_guardrails_engine_errors_total",
            "PII guardrail engine failures",
        ))
        .expect("static metric definition is valid");

        let collectors: [Box<dyn prometheus::core::Collector>; 15] = [
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
            Box::new(guardrail_pii_detections_total.clone()),
            Box::new(guardrail_blocked_total.clone()),
            Box::new(guardrail_scan_duration.clone()),
            Box::new(guardrail_engine_errors_total.clone()),
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
            guardrail_pii_detections_total,
            guardrail_blocked_total,
            guardrail_scan_duration,
            guardrail_engine_errors_total,
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
