use dashmap::DashMap;
use reqwest::Client;
use std::time::Duration;

/// Per-provider transport configuration tuned for LLM workloads.
///
/// `request_timeout` is the **total request deadline** (reqwest's
/// `Client::timeout`); `read_timeout` guards against a stalled connection
/// by bounding the gap between successive reads (reqwest's
/// `Client::read_timeout`), which is what actually protects long-lived
/// streams. `Duration::ZERO` disables either bound.
#[derive(Debug, Clone)]
pub struct TransportConfig {
    /// Max idle connections per host
    pub max_idle_per_host: usize,
    /// Connection pool idle timeout
    pub pool_idle_timeout: Duration,
    /// Connect timeout
    pub connect_timeout: Duration,
    /// Total request deadline (0 = unbounded; rely on `read_timeout`)
    pub request_timeout: Duration,
    /// Max gap between successive body reads (0 = unbounded)
    pub read_timeout: Duration,
    /// TCP keepalive interval
    pub tcp_keepalive: Duration,
    /// TCP_NODELAY for low latency
    pub tcp_nodelay: bool,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            max_idle_per_host: 100,
            pool_idle_timeout: Duration::from_secs(90),
            connect_timeout: Duration::from_secs(10),
            // Generous ceiling: long non-streaming completions (large
            // max_tokens, reasoning models) routinely exceed 30s.
            request_timeout: Duration::from_secs(600),
            read_timeout: Duration::from_secs(120),
            tcp_keepalive: Duration::from_secs(60),
            tcp_nodelay: true,
        }
    }
}

impl TransportConfig {
    /// Production-tuned config for OpenAI/Azure
    pub fn openai() -> Self {
        Self {
            max_idle_per_host: 200,
            ..Self::default()
        }
    }

    /// Anthropic config (longer read timeout for large prompts)
    pub fn anthropic() -> Self {
        Self {
            max_idle_per_host: 150,
            read_timeout: Duration::from_secs(180),
            ..Self::default()
        }
    }

    /// Bedrock/Vertex config (longer timeouts for cloud providers)
    pub fn cloud_provider() -> Self {
        Self {
            connect_timeout: Duration::from_secs(15),
            read_timeout: Duration::from_secs(180),
            ..Self::default()
        }
    }

    /// Streaming-optimized config: no total deadline (streams may
    /// legitimately run very long), but a read timeout so a stalled
    /// upstream cannot pin a connection forever.
    pub fn streaming() -> Self {
        Self {
            pool_idle_timeout: Duration::from_secs(300), // 5 min idle for long streams
            request_timeout: Duration::ZERO,
            read_timeout: Duration::from_secs(300),
            ..Self::default()
        }
    }

    /// Local Ollama config
    pub fn local() -> Self {
        Self {
            max_idle_per_host: 20,
            pool_idle_timeout: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(5),
            tcp_keepalive: Duration::from_secs(30),
            ..Self::default()
        }
    }
}

/// Per-provider HTTP client pool with tuned connection settings.
pub struct ProviderHttpClient {
    clients: DashMap<String, Client>,
    streaming_client: Client,
}

impl ProviderHttpClient {
    pub fn new() -> Self {
        Self {
            clients: DashMap::new(),
            streaming_client: Self::build_streaming_client(),
        }
    }

    /// Get or create an HTTP client for a provider.
    pub fn for_provider(&self, provider: &str) -> Client {
        if let Some(client) = self.clients.get(provider) {
            return client.clone();
        }

        let client = Self::build_provider_client(provider);
        self.clients
            .entry(provider.to_string())
            .or_insert(client)
            .clone()
    }

    /// Get a provider client with custom config.
    pub fn for_provider_with_config(&self, provider: &str, config: TransportConfig) -> Client {
        let key = format!("{}:custom", provider);
        if let Some(client) = self.clients.get(&key) {
            return client.clone();
        }

        let client = Self::build_client_from_config(&config);
        self.clients.entry(key).or_insert(client).clone()
    }

    /// Get a streaming-optimized client (read timeout only, no total deadline).
    pub fn shared_streaming(&self) -> Client {
        self.streaming_client.clone()
    }

    fn build_provider_client(provider: &str) -> Client {
        let config = match provider {
            "openai" | "azure-openai" | "azure" => TransportConfig::openai(),
            "anthropic" => TransportConfig::anthropic(),
            "bedrock" | "vertex" => TransportConfig::cloud_provider(),
            "ollama" => TransportConfig::local(),
            "openrouter" | "together" | "groq" | "fireworks" | "deepinfra" | "cerebras"
            | "novita" => {
                TransportConfig::openai() // Most OpenAI-compatible providers
            }
            _ => TransportConfig::default(),
        };

        Self::build_client_from_config(&config)
    }

    fn build_client_from_config(config: &TransportConfig) -> Client {
        // propagation seam: to inject the current trace context into upstream
        // provider calls, add a middleware here (or at each request) that writes
        // the W3C `traceparent`/`tracestate` headers from the active
        // OpenTelemetry context via the globally-installed propagator.
        let mut builder = Client::builder()
            .pool_max_idle_per_host(config.max_idle_per_host)
            .pool_idle_timeout(config.pool_idle_timeout)
            .connect_timeout(config.connect_timeout)
            .tcp_keepalive(config.tcp_keepalive)
            .tcp_nodelay(config.tcp_nodelay);

        if !config.request_timeout.is_zero() {
            builder = builder.timeout(config.request_timeout);
        }
        if !config.read_timeout.is_zero() {
            builder = builder.read_timeout(config.read_timeout);
        }

        builder.build().expect("Failed to build HTTP client")
    }

    fn build_streaming_client() -> Client {
        let config = TransportConfig::streaming();
        Self::build_client_from_config(&config)
    }
}

impl Default for ProviderHttpClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Shared client pool singleton
pub static CLIENT_POOL: once_cell::sync::Lazy<ProviderHttpClient> =
    once_cell::sync::Lazy::new(ProviderHttpClient::new);
