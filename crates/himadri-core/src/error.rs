use thiserror::Error;

#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("provider not found: {0}")]
    ProviderNotFound(String),

    #[error("circuit breaker open for provider: {0}")]
    CircuitOpen(String),

    #[error("rate limited")]
    RateLimited { retry_after_secs: u64 },

    /// A spend/budget cap is exhausted. Surfaces as 429, matching OpenAI's
    /// `insufficient_quota` convention, but carries the human-readable cap
    /// message and is not retryable in the short term.
    #[error("quota exceeded: {0}")]
    QuotaExceeded(String),

    #[error("unauthorized")]
    Unauthorized,

    #[error("forbidden: {0}")]
    Forbidden(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("provider error: {0}")]
    Provider(String),

    #[error("config error: {0}")]
    Config(#[from] ConfigError),

    #[error("internal error: {0}")]
    Internal(String),

    #[error("service unavailable: {0}")]
    ServiceUnavailable(String),
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("invalid strategy mode: {0}")]
    InvalidStrategy(String),

    #[error("missing required field: {0}")]
    MissingField(String),

    #[error("invalid value for {field}: {reason}")]
    InvalidValue { field: String, reason: String },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("parse error: {0}")]
    Parse(String),
}

impl GatewayError {
    pub fn status_code(&self) -> u16 {
        match self {
            GatewayError::ProviderNotFound(_) => 404,
            GatewayError::CircuitOpen(_) => 503,
            GatewayError::RateLimited { .. } => 429,
            GatewayError::QuotaExceeded(_) => 429,
            GatewayError::Unauthorized => 401,
            GatewayError::Forbidden(_) => 403,
            GatewayError::NotFound(_) => 404,
            GatewayError::BadRequest(_) => 400,
            GatewayError::Provider(_) => 500,
            GatewayError::Config(_) => 400,
            GatewayError::Internal(_) => 500,
            GatewayError::ServiceUnavailable(_) => 503,
        }
    }

    pub fn retryable(&self) -> bool {
        matches!(
            self,
            GatewayError::CircuitOpen(_)
                | GatewayError::RateLimited { .. }
                | GatewayError::ServiceUnavailable(_)
        )
    }
}
