use thiserror::Error;

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

    #[error("network error: {0}")]
    Network(String),

    #[error("parse error: {0}")]
    Parse(String),

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
            ProviderError::Network(_) => 502,
            ProviderError::Parse(_) => 500,
            ProviderError::Internal(_) => 500,
        }
    }
}

impl From<reqwest::Error> for ProviderError {
    fn from(e: reqwest::Error) -> Self {
        ProviderError::Network(e.to_string())
    }
}
