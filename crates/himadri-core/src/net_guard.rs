//! SSRF guard for operator-supplied provider base URLs.
//!
//! The gateway forwards requests to whatever `base_url` a provider is
//! configured with, so a URL pointing at an internal address (loopback, RFC
//! 1918, link-local, the cloud metadata endpoint, …) could be used to reach
//! services that aren't meant to be exposed. This module rejects such URLs by
//! default. Deployments that legitimately proxy to a private LLM backend
//! (self-hosted vLLM, an internal endpoint) can opt out via
//! `ALLOW_PRIVATE_PROVIDER_URLS=1`.
//!
//! Limitation: this checks IP *literals* and known metadata hostnames only.
//! A hostname that resolves to an internal IP at request time (DNS rebinding)
//! is not caught here.

use std::net::IpAddr;

/// Whether provider URLs pointing at private/internal hosts are permitted.
/// Opt in with `ALLOW_PRIVATE_PROVIDER_URLS` set to `1`/`true`/`yes`.
pub fn allow_private_provider_urls() -> bool {
    std::env::var("ALLOW_PRIVATE_PROVIDER_URLS")
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

/// Validate an outbound provider base URL for SSRF safety.
///
/// Requires an `http(s)` scheme and, unless `allow_private` is true, rejects a
/// host that is an IP literal in an internal range or a well-known metadata
/// hostname. Returns a human-readable reason on rejection.
pub fn provider_url_is_allowed(url: &str, allow_private: bool) -> Result<(), String> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .ok_or_else(|| "base_url must use http or https".to_string())?;

    // Authority is everything up to the first path/query/fragment separator.
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // Drop any userinfo ("user:pass@host").
    let host_port = authority.rsplit('@').next().unwrap_or(authority);

    // Extract the bare host, handling bracketed IPv6 ("[::1]:8080").
    let host = if let Some(after_bracket) = host_port.strip_prefix('[') {
        after_bracket.split(']').next().unwrap_or("")
    } else {
        // IPv4 / DNS name: strip a trailing ":port" if present.
        host_port.split(':').next().unwrap_or("")
    };

    if host.is_empty() {
        return Err("base_url has no host".to_string());
    }

    if allow_private {
        return Ok(());
    }

    let lowered = host.to_ascii_lowercase();
    if lowered == "metadata" || lowered == "metadata.google.internal" {
        return Err(format!("base_url host '{host}' is not allowed"));
    }

    if let Ok(ip) = host.parse::<IpAddr>() {
        if ip_is_internal(ip) {
            return Err(format!(
                "base_url points to an internal address '{host}'; set \
                 ALLOW_PRIVATE_PROVIDER_URLS=1 to permit private backends"
            ));
        }
    }

    Ok(())
}

/// True for addresses that should not be reachable from an
/// operator-configured provider URL by default.
fn ip_is_internal(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local() // 169.254.0.0/16, incl. 169.254.169.254 metadata
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64) // 100.64.0.0/10 CGNAT
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_unique_local() // fc00::/7
                || v6.is_unicast_link_local() // fe80::/10
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_public_https_endpoints() {
        assert!(provider_url_is_allowed("https://api.openai.com/v1", false).is_ok());
        assert!(provider_url_is_allowed("https://api.anthropic.com", false).is_ok());
    }

    #[test]
    fn rejects_non_http_schemes() {
        assert!(provider_url_is_allowed("file:///etc/passwd", false).is_err());
        assert!(provider_url_is_allowed("gopher://x/", false).is_err());
    }

    #[test]
    fn rejects_internal_ip_literals() {
        for url in [
            "http://127.0.0.1:8080/v1",
            "http://169.254.169.254/latest/meta-data/", // cloud metadata
            "http://10.0.0.5/v1",
            "http://192.168.1.10/v1",
            "http://[::1]:9000/v1",
            "https://user:pass@127.0.0.1/v1", // userinfo must not hide the host
        ] {
            assert!(
                provider_url_is_allowed(url, false).is_err(),
                "expected rejection for {url}"
            );
        }
    }

    #[test]
    fn rejects_metadata_hostname() {
        assert!(provider_url_is_allowed("http://metadata.google.internal/", false).is_err());
    }

    #[test]
    fn opt_out_permits_private_backends() {
        assert!(provider_url_is_allowed("http://10.0.0.5:8000/v1", true).is_ok());
        // ...but the scheme check still applies.
        assert!(provider_url_is_allowed("file:///x", true).is_err());
    }
}
