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
        assert!(streaming.response_header_timeout.is_zero());

        let local = crate::http_client::TransportConfig::local();
        assert!(!local.http2);
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
        assert!(!ProviderError::Api {
            status: 500,
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
}
