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

    #[error("operation not supported: {0}")]
    Unsupported(String),
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
            ProviderError::Unsupported(_) => 501,
        }
    }
}

impl ProviderError {
    /// Map an unsuccessful HTTP response with an OpenAI-shaped error body
    /// (`{"error": {"message": ...}}` or `{"message": ...}`) to a
    /// `ProviderError`, treating 401 as an auth failure.
    pub async fn from_openai_response(response: reqwest::Response) -> Self {
        Self::from_response(response, &[401], |v| {
            v["error"]["message"]
                .as_str()
                .or_else(|| v["message"].as_str())
                .map(str::to_string)
        })
        .await
    }

    /// Map an unsuccessful HTTP response to a `ProviderError`, using
    /// `extract_message` to pull a human-readable message out of the JSON
    /// body (falling back to the raw body) and `auth_statuses` for the
    /// status codes that indicate an authentication failure.
    pub async fn from_response(
        response: reqwest::Response,
        auth_statuses: &[u16],
        extract_message: impl Fn(&serde_json::Value) -> Option<String>,
    ) -> Self {
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();

        let message = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| extract_message(&v))
            .unwrap_or(body);

        if auth_statuses.contains(&status) {
            return ProviderError::Auth(message);
        }
        match status {
            429 => ProviderError::RateLimited {
                retry_after_secs: 60,
            },
            404 => ProviderError::ModelNotFound(message),
            _ => ProviderError::Api { status, message },
        }
    }
}

impl From<reqwest::Error> for ProviderError {
    fn from(e: reqwest::Error) -> Self {
        ProviderError::Network(e.to_string())
    }
}
