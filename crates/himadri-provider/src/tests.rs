#[cfg(test)]
#[allow(clippy::module_inception)]
mod tests {
    use crate::{
        OpenAiCompatibleConfig, OpenAiCompatibleProvider, ProviderError, ProviderHttpClient,
    };

    use crate::traits::Provider;

    // ═══════════════════════════════════════════════════════════════════
    // Provider Config Tests
    // ═══════════════════════════════════════════════════════════════════

    #[test]
    fn test_openai_config() {
        let config = OpenAiCompatibleConfig::openai();
        assert_eq!(config.name, "openai");
        assert_eq!(config.base_url, "https://api.openai.com/v1");
        assert!(!config.models.is_empty());
        assert!(config.models.contains(&"gpt-4o".to_string()));
    }

    #[test]
    fn test_openrouter_config() {
        let config = OpenAiCompatibleConfig::openrouter();
        assert_eq!(config.name, "openrouter");
        assert_eq!(config.base_url, "https://openrouter.ai/api/v1");
        assert!(!config.extra_headers.is_empty());
    }

    #[test]
    fn test_groq_config() {
        let config = OpenAiCompatibleConfig::groq();
        assert_eq!(config.name, "groq");
        assert_eq!(config.base_url, "https://api.groq.com/openai/v1");
    }

    #[test]
    fn test_together_config() {
        let config = OpenAiCompatibleConfig::together_ai();
        assert_eq!(config.name, "together");
        assert_eq!(config.base_url, "https://api.together.xyz/v1");
    }

    #[test]
    fn test_azure_config() {
        let provider = OpenAiCompatibleProvider::azure(
            "test-key",
            "https://test.openai.azure.com",
            "gpt-4",
            "2024-10-21",
        );
        assert_eq!(provider.name(), "azure-openai");
    }

    #[test]
    fn test_fireworks_config() {
        let config = OpenAiCompatibleConfig::fireworks();
        assert_eq!(config.name, "fireworks");
        assert!(config.base_url.contains("fireworks"));
    }

    #[test]
    fn test_deepinfra_config() {
        let config = OpenAiCompatibleConfig::deepinfra();
        assert_eq!(config.name, "deepinfra");
        assert!(config.base_url.contains("deepinfra"));
    }

    #[test]
    fn test_cerebras_config() {
        let config = OpenAiCompatibleConfig::cerebras();
        assert_eq!(config.name, "cerebras");
        assert!(config.base_url.contains("cerebras"));
    }

    #[test]
    fn test_novita_config() {
        let config = OpenAiCompatibleConfig::novita();
        assert_eq!(config.name, "novita");
        assert!(config.base_url.contains("novita"));
    }

    // ═══════════════════════════════════════════════════════════════════
    // Provider Trait Tests
    // ═══════════════════════════════════════════════════════════════════

    #[test]
    fn test_provider_name() {
        let provider = OpenAiCompatibleProvider::new(OpenAiCompatibleConfig::openai());
        assert_eq!(provider.name(), "openai");
    }

    #[test]
    fn test_provider_display_name() {
        let provider = OpenAiCompatibleProvider::new(OpenAiCompatibleConfig::openai());
        assert_eq!(provider.display_name(), "OpenAI");
    }

    #[test]
    fn test_provider_supported_models() {
        let provider = OpenAiCompatibleProvider::new(OpenAiCompatibleConfig::openai());
        let models = provider.supported_models();
        assert!(models.contains(&"gpt-4o".to_string()));
        assert!(models.contains(&"gpt-4".to_string()));
    }

    #[test]
    fn test_provider_clone() {
        let provider = OpenAiCompatibleProvider::new(OpenAiCompatibleConfig::openai());
        let cloned = provider.clone();
        assert_eq!(provider.name(), cloned.name());
    }

    #[test]
    fn test_provider_azure_name() {
        let provider = OpenAiCompatibleProvider::azure(
            "key",
            "https://test.openai.azure.com",
            "gpt-4",
            "2024-10-21",
        );
        assert_eq!(provider.name(), "azure-openai");
        assert_eq!(provider.display_name(), "Azure OpenAI");
    }

    // ═══════════════════════════════════════════════════════════════════
    // Error Tests
    // ═══════════════════════════════════════════════════════════════════

    #[test]
    fn test_http_client_different_providers() {
        let pool = ProviderHttpClient::new();
        let client1 = pool.for_provider("openai");
        let client2 = pool.for_provider("anthropic");
        assert_ne!(format!("{:p}", &client1), format!("{:p}", &client2));
    }

    #[test]
    fn test_transport_config() {
        let openai = crate::http_client::TransportConfig::openai();
        assert_eq!(openai.max_idle_per_host, 200);

        let streaming = crate::http_client::TransportConfig::streaming();
        // Streams have no total deadline but must keep a read timeout so a
        // stalled upstream cannot pin a connection forever.
        assert!(streaming.request_timeout.is_zero());
        assert!(!streaming.read_timeout.is_zero());

        // Non-streaming clients need a generous total ceiling: long
        // completions routinely exceed 30s.
        let openai_timeout = crate::http_client::TransportConfig::openai().request_timeout;
        assert!(openai_timeout.as_secs() >= 300);
    }

    // ═══════════════════════════════════════════════════════════════════
    // Error Tests
    // ═══════════════════════════════════════════════════════════════════

    #[test]
    fn test_error_retryable() {
        assert!(!ProviderError::Auth("test".into()).retryable());
        assert!(ProviderError::RateLimited {
            retry_after_secs: 60
        }
        .retryable());
        assert!(!ProviderError::ModelNotFound("test".into()).retryable());
        assert!(ProviderError::Api {
            status: 502,
            message: "test".into()
        }
        .retryable());
        assert!(ProviderError::Api {
            status: 503,
            message: "test".into()
        }
        .retryable());
        assert!(ProviderError::Api {
            status: 529,
            message: "test".into()
        }
        .retryable());
        assert!(ProviderError::Api {
            status: 500,
            message: "test".into()
        }
        .retryable());
        // Transport failures must be retryable: a hard-down provider has to
        // trip the circuit breaker and fall back to the next target.
        assert!(ProviderError::Network("connection refused".into()).retryable());
        // Client errors must not burn fallback targets.
        assert!(!ProviderError::Api {
            status: 400,
            message: "test".into()
        }
        .retryable());
    }

    #[test]
    fn test_error_status_codes() {
        assert_eq!(ProviderError::Auth("test".into()).status_code(), 401);
        assert_eq!(
            ProviderError::RateLimited {
                retry_after_secs: 60
            }
            .status_code(),
            429
        );
        assert_eq!(
            ProviderError::ModelNotFound("test".into()).status_code(),
            404
        );
        assert_eq!(
            ProviderError::Api {
                status: 418,
                message: "test".into()
            }
            .status_code(),
            418
        );
    }

    // ═══════════════════════════════════════════════════════════════════
    // Embeddings
    // ═══════════════════════════════════════════════════════════════════

    #[test]
    fn embedding_input_serde_roundtrip() {
        use himadri_core::EmbeddingInput;

        let single: EmbeddingInput = serde_json::from_str("\"hello\"").unwrap();
        assert!(matches!(single, EmbeddingInput::Single(s) if s == "hello"));

        let multiple: EmbeddingInput = serde_json::from_str("[\"a\",\"b\"]").unwrap();
        assert!(matches!(multiple, EmbeddingInput::Multiple(v) if v.len() == 2));
    }

    #[tokio::test]
    async fn default_embed_is_unsupported() {
        use crate::AnthropicProvider;
        use himadri_core::{EmbeddingInput, EmbeddingRequest};

        let provider = AnthropicProvider::new(None);
        let request = EmbeddingRequest {
            model: "text-embedding-3-small".to_string(),
            input: EmbeddingInput::Single("hello".to_string()),
            encoding_format: None,
            dimensions: None,
            user: None,
            extra: Default::default(),
        };
        let err = provider.embed(&request, "key").await.unwrap_err();
        assert!(matches!(err, ProviderError::Unsupported(_)));
        assert_eq!(err.status_code(), 501);
    }
}

// ═══════════════════════════════════════════════════════════════════
// HTTP error-mapping tests (no network: reqwest::Response built from
// synthetic http::Response values)
// ═══════════════════════════════════════════════════════════════════

#[cfg(test)]
mod error_mapping_tests {
    use crate::error::ProviderError;

    fn response(status: u16, headers: &[(&str, &str)], body: &str) -> reqwest::Response {
        let mut builder = http::Response::builder().status(status);
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        reqwest::Response::from(builder.body(body.to_string()).unwrap())
    }

    #[tokio::test]
    async fn upstream_429_honors_retry_after_header() {
        let resp = response(429, &[("retry-after", "17")], "{}");
        let err = ProviderError::from_openai_response(resp).await;
        assert!(matches!(
            err,
            ProviderError::RateLimited {
                retry_after_secs: 17
            }
        ));
    }

    #[tokio::test]
    async fn upstream_429_without_header_defaults() {
        let resp = response(429, &[], "{}");
        let err = ProviderError::from_openai_response(resp).await;
        assert!(matches!(
            err,
            ProviderError::RateLimited {
                retry_after_secs: 60
            }
        ));
    }

    #[tokio::test]
    async fn upstream_401_maps_to_auth_with_extracted_message() {
        let resp = response(401, &[], r#"{"error":{"message":"bad key"}}"#);
        let err = ProviderError::from_openai_response(resp).await;
        assert!(matches!(err, ProviderError::Auth(m) if m == "bad key"));
    }

    #[tokio::test]
    async fn upstream_404_maps_to_model_not_found() {
        let resp = response(404, &[], r#"{"message":"no such model"}"#);
        let err = ProviderError::from_openai_response(resp).await;
        assert!(matches!(err, ProviderError::ModelNotFound(m) if m == "no such model"));
    }

    #[tokio::test]
    async fn non_json_body_falls_back_to_raw_text() {
        let resp = response(502, &[], "<html>bad gateway</html>");
        let err = ProviderError::from_openai_response(resp).await;
        assert!(matches!(
            err,
            ProviderError::Api { status: 502, ref message } if message.contains("bad gateway")
        ));
    }

    /// Gateway-facing mapping: upstream semantics must survive the edge.
    #[test]
    fn gateway_error_mapping_preserves_semantics() {
        use himadri_core::GatewayError;

        let e: GatewayError = ProviderError::RateLimited {
            retry_after_secs: 5,
        }
        .into();
        assert_eq!(e.status_code(), 429);

        // Upstream auth failure = the gateway's provider key is bad; the
        // caller's token was fine, so it must NOT surface as 401.
        let e: GatewayError = ProviderError::Auth("k".into()).into();
        assert_eq!(e.status_code(), 503);

        let e: GatewayError = ProviderError::Api {
            status: 400,
            message: "bad request".into(),
        }
        .into();
        assert_eq!(e.status_code(), 400);

        let e: GatewayError = ProviderError::Network("refused".into()).into();
        assert_eq!(e.status_code(), 503);
    }
}
