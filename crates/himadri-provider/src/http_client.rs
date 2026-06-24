use dashmap::DashMap;
use reqwest::Client;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

/// Per-provider transport configuration tuned for LLM workloads.
#[derive(Debug, Clone)]
pub struct TransportConfig {
    /// Max idle connections per host
    pub max_idle_per_host: usize,
    /// Max total idle connections
    pub max_idle_conns: usize,
    /// Connection pool idle timeout
    pub pool_idle_timeout: Duration,
    /// Connect timeout
    pub connect_timeout: Duration,
    /// Response header timeout (0 = no timeout, for streaming)
    pub response_header_timeout: Duration,
    /// TCP keepalive interval
    pub tcp_keepalive: Duration,
    /// TCP_NODELAY for low latency
    pub tcp_nodelay: bool,
    /// Enable HTTP/2 multiplexing
    pub http2: bool,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            max_idle_per_host: 100,
            max_idle_conns: 1000,
            pool_idle_timeout: Duration::from_secs(90),
            connect_timeout: Duration::from_secs(10),
            response_header_timeout: Duration::from_secs(30),
            tcp_keepalive: Duration::from_secs(60),
            tcp_nodelay: true,
            http2: true,
        }
    }
}

impl TransportConfig {
    /// Production-tuned config for OpenAI/Azure
    pub fn openai() -> Self {
        Self {
            max_idle_per_host: 200,
            max_idle_conns: 2000,
            pool_idle_timeout: Duration::from_secs(90),
            connect_timeout: Duration::from_secs(10),
            response_header_timeout: Duration::from_secs(30),
            tcp_keepalive: Duration::from_secs(60),
            tcp_nodelay: true,
            http2: true,
        }
    }

    /// Anthropic config (longer timeouts for large prompts)
    pub fn anthropic() -> Self {
        Self {
            max_idle_per_host: 150,
            max_idle_conns: 1500,
            pool_idle_timeout: Duration::from_secs(90),
            connect_timeout: Duration::from_secs(10),
            response_header_timeout: Duration::from_secs(60), // Longer for large prompts
            tcp_keepalive: Duration::from_secs(60),
            tcp_nodelay: true,
            http2: true,
        }
    }

    /// Bedrock/Vertex config (longer timeouts for cloud providers)
    pub fn cloud_provider() -> Self {
        Self {
            max_idle_per_host: 100,
            max_idle_conns: 1000,
            pool_idle_timeout: Duration::from_secs(90),
            connect_timeout: Duration::from_secs(15), // Longer connect timeout
            response_header_timeout: Duration::from_secs(120), // Very long for streaming
            tcp_keepalive: Duration::from_secs(60),
            tcp_nodelay: true,
            http2: true,
        }
    }

    /// Streaming-optimized config (no response header timeout)
    pub fn streaming() -> Self {
        Self {
            max_idle_per_host: 100,
            max_idle_conns: 1000,
            pool_idle_timeout: Duration::from_secs(300), // 5 min idle for long streams
            connect_timeout: Duration::from_secs(10),
            response_header_timeout: Duration::ZERO, // No timeout for streaming
            tcp_keepalive: Duration::from_secs(60),
            tcp_nodelay: true,
            http2: true,
        }
    }

    /// Local Ollama config
    pub fn local() -> Self {
        Self {
            max_idle_per_host: 20,
            max_idle_conns: 50,
            pool_idle_timeout: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(5),
            response_header_timeout: Duration::from_secs(30),
            tcp_keepalive: Duration::from_secs(30),
            tcp_nodelay: true,
            http2: false, // Ollama doesn't use HTTP/2
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

    /// Get a streaming-optimized client (no response header timeout).
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
        let mut builder = Client::builder()
            .pool_max_idle_per_host(config.max_idle_per_host)
            .pool_idle_timeout(config.pool_idle_timeout)
            .connect_timeout(config.connect_timeout)
            .tcp_keepalive(config.tcp_keepalive)
            .tcp_nodelay(config.tcp_nodelay);

        if !config.response_header_timeout.is_zero() {
            builder = builder.timeout(config.response_header_timeout);
        }

        if config.http2 {
            // Enable HTTP/2 (reqwest handles this automatically with TLS)
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

/// Request metrics for transport-level observability
#[derive(Debug, Default)]
pub struct TransportMetrics {
    pub dns_lookups: AtomicU64,
    pub tls_handshakes: AtomicU64,
    pub connections_reused: AtomicU64,
}

impl TransportMetrics {
    pub fn new() -> Self {
        Self::default()
    }
}
