use thiserror::Error;

#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("provider not found: {0}")]
    ProviderNotFound(String),

    #[error("circuit breaker open for provider: {0}")]
    CircuitOpen(String),

    #[error("rate limited")]
    RateLimited { retry_after_secs: u64 },

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
pub enum ProviderError {
    #[error("authentication failed: {0}")]
    Auth(String),

    #[error("rate limited, retry after {retry_after_secs}s")]
    RateLimited { retry_after_secs: u64 },

    #[error("model not found: {0}")]
    ModelNotFound(String),

    #[error("context length exceeded: max {max}, got {actual}")]
    ContextLengthExceeded { max: u32, actual: u32 },

    #[error("provider error ({status}): {message}")]
    Api { status: u16, message: String },

    #[error("internal error: {0}")]
    Internal(String),
}

impl ProviderError {
    pub fn retryable(&self) -> bool {
        matches!(
            self,
            ProviderError::RateLimited { .. }
                | ProviderError::Api {
                    status: 502 | 503 | 529,
                    ..
                }
        )
    }

    pub fn status_code(&self) -> u16 {
        match self {
            ProviderError::Auth(_) => 401,
            ProviderError::RateLimited { .. } => 429,
            ProviderError::ModelNotFound(_) => 404,
            ProviderError::ContextLengthExceeded { .. } => 400,
            ProviderError::Api { status, .. } => *status,
            ProviderError::Internal(_) => 500,
        }
    }

    pub fn to_message(&self) -> String {
        match self {
            ProviderError::Auth(s) => format!("auth: {}", s),
            ProviderError::RateLimited { retry_after_secs } => {
                format!("rate limited, retry after {}s", retry_after_secs)
            }
            ProviderError::ModelNotFound(s) => format!("model not found: {}", s),
            ProviderError::ContextLengthExceeded { max, actual } => {
                format!("context length exceeded: max {}, got {}", max, actual)
            }
            ProviderError::Api { status, message } => {
                format!("provider error ({}): {}", status, message)
            }
            ProviderError::Internal(s) => format!("internal: {}", s),
        }
    }
}

impl From<ProviderError> for GatewayError {
    fn from(e: ProviderError) -> Self {
        match &e {
            ProviderError::Auth(_msg) => GatewayError::Unauthorized,
            ProviderError::RateLimited { retry_after_secs } => GatewayError::RateLimited {
                retry_after_secs: *retry_after_secs,
            },
            ProviderError::ModelNotFound(model) => {
                GatewayError::NotFound(format!("model: {}", model))
            }
            ProviderError::Api { status, message } => {
                if *status == 401 {
                    GatewayError::Unauthorized
                } else if *status == 404 {
                    GatewayError::NotFound(message.clone())
                } else if *status >= 500 {
                    GatewayError::ServiceUnavailable(message.clone())
                } else {
                    GatewayError::Provider(e.to_string())
                }
            }
            _ => GatewayError::Provider(e.to_string()),
        }
    }
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
