//! Catch-all `/v1/*` passthrough proxy: forwards anything not matched by a
//! specific handler to the first configured target, streaming the upstream
//! body through.

use himadri_core::GatewayError;

use super::{routing_key, Gateway};

static PROXY_CLIENT: once_cell::sync::Lazy<reqwest::Client> = once_cell::sync::Lazy::new(|| {
    reqwest::Client::builder()
        .pool_max_idle_per_host(32)
        .pool_idle_timeout(std::time::Duration::from_secs(90))
        // No total deadline (passthrough may proxy long streams), but bound
        // connect and inter-read gaps so a hung upstream can't pin a
        // request forever.
        .connect_timeout(std::time::Duration::from_secs(10))
        .read_timeout(std::time::Duration::from_secs(120))
        .build()
        .expect("Failed to create proxy HTTP client")
});

impl Gateway {
    pub async fn proxy(
        &self,
        method: &str,
        path: &str,
        headers: &axum::http::HeaderMap,
        body: axum::body::Bytes,
    ) -> Result<
        (
            axum::http::StatusCode,
            axum::http::HeaderMap,
            axum::body::Body,
        ),
        GatewayError,
    > {
        let targets = self.targets.read().await;
        let target = targets
            .first()
            .ok_or_else(|| GatewayError::Internal("No targets configured for proxy".to_string()))?;

        let provider = self
            .providers
            .get(routing_key(target))
            .ok_or_else(|| GatewayError::ProviderNotFound(target.provider.clone()))?;

        let base_url = target
            .base_url
            .clone()
            .unwrap_or_else(|| match provider.name() {
                "openai" => "https://api.openai.com/v1".to_string(),
                "anthropic" => "https://api.anthropic.com".to_string(),
                "gemini" => "https://generativelanguage.googleapis.com".to_string(),
                _ => "https://api.openai.com/v1".to_string(),
            });

        let api_key = self.get_api_key(target)?;
        let url = format!("{}{}", base_url.trim_end_matches('/'), path);

        let m: reqwest::Method = method
            .parse()
            .map_err(|_| GatewayError::BadRequest(format!("Invalid method: {}", method)))?;
        let mut req_builder = PROXY_CLIENT.request(m, &url);

        for (key, value) in headers.iter() {
            if key == "authorization" || key == "host" || key == "content-length" {
                continue;
            }
            req_builder = req_builder.header(key, value);
        }

        if !api_key.is_empty() {
            req_builder = req_builder.header("authorization", format!("Bearer {}", api_key));
        }

        req_builder = req_builder.body(body);

        let resp = req_builder
            .send()
            .await
            .map_err(|e| GatewayError::Provider(format!("Proxy request failed: {}", e)))?;

        let status = axum::http::StatusCode::from_u16(resp.status().as_u16())
            .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR);

        let mut resp_headers = axum::http::HeaderMap::new();
        for (key, value) in resp.headers().iter() {
            if key == "transfer-encoding" || key == "connection" {
                continue;
            }
            if let (Ok(name), Ok(val)) = (
                axum::http::HeaderName::from_bytes(key.as_str().as_bytes()),
                axum::http::HeaderValue::from_bytes(value.as_bytes()),
            ) {
                resp_headers.insert(name, val);
            }
        }

        // Stream the upstream body through instead of buffering it whole:
        // proxied streaming endpoints stay streams, and a large response
        // can't balloon gateway memory.
        let resp_body = axum::body::Body::from_stream(resp.bytes_stream());

        Ok((status, resp_headers, resp_body))
    }
}
