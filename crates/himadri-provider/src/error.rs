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

    #[error("unknown provider type: {0}")]
    UnknownType(String),

    #[error("missing base_url for provider: {0}")]
    MissingBaseUrl(String),

    #[error("invalid provider configuration: {0}")]
    InvalidConfiguration(String),
}

impl ProviderError {
    /// Whether failing over to another target may succeed.
    ///
    /// Transport failures (`Network`) are retryable: a dead or unreachable
    /// provider must both trip its circuit breaker and let the request fall
    /// back to the next configured target — otherwise a hard-down primary
    /// takes its models offline despite healthy fallbacks.
    pub fn retryable(&self) -> bool {
        matches!(
            self,
            ProviderError::RateLimited { .. }
                | ProviderError::Network(_)
                | ProviderError::Api {
                    status: 500 | 502 | 503 | 504 | 529,
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
            ProviderError::UnknownType(_) => 400,
            ProviderError::MissingBaseUrl(_) => 400,
            ProviderError::InvalidConfiguration(_) => 400,
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
        // Honor the upstream Retry-After (seconds form) instead of guessing.
        let retry_after_secs = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
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
                retry_after_secs: retry_after_secs.unwrap_or(60),
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

/// Structured mapping into the gateway's error surface so upstream failures
/// keep their semantics instead of flattening to 500: an upstream 429 stays
/// a 429 (clients back off), an upstream 4xx stays a client error, and an
/// upstream auth failure — which means the *gateway's* provider key is bad,
/// not the caller's — surfaces as 503, not a misleading 401.
impl From<ProviderError> for himadri_core::GatewayError {
    fn from(e: ProviderError) -> Self {
        use himadri_core::GatewayError as G;
        match e {
            ProviderError::Auth(_) => {
                G::ServiceUnavailable("upstream provider authentication failed".to_string())
            }
            ProviderError::RateLimited { retry_after_secs } => G::RateLimited { retry_after_secs },
            ProviderError::ModelNotFound(model) => G::NotFound(format!("model: {}", model)),
            ProviderError::ContextLengthExceeded { max, actual } => G::BadRequest(format!(
                "context length exceeded: max {}, got {}",
                max, actual
            )),
            ProviderError::Api {
                status,
                ref message,
            } => {
                if status == 401 || status == 403 {
                    G::ServiceUnavailable("upstream provider authentication failed".to_string())
                } else if status == 429 {
                    G::RateLimited {
                        retry_after_secs: 60,
                    }
                } else if (400..500).contains(&status) {
                    G::BadRequest(message.clone())
                } else {
                    G::ServiceUnavailable(message.clone())
                }
            }
            ProviderError::Network(msg) => G::ServiceUnavailable(msg),
            ProviderError::Parse(msg) => G::Provider(format!("parse error: {}", msg)),
            ProviderError::Internal(msg) => G::Internal(msg),
            ProviderError::Unsupported(msg) => {
                G::BadRequest(format!("operation not supported: {}", msg))
            }
            ProviderError::UnknownType(msg) => G::BadRequest(format!("unknown provider type: {}", msg)),
            ProviderError::MissingBaseUrl(msg) => {
                G::BadRequest(format!("missing base_url for provider: {}", msg))
            }
            ProviderError::InvalidConfiguration(msg) => {
                G::BadRequest(format!("invalid provider configuration: {}", msg))
            }
        }
    }
}
