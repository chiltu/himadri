//! End-to-end tests for the PII guardrail (docs/SPEC_GUARDRAILS.md §11):
//! drives `Gateway` directly with a capturing provider so the assertion is
//! on what the *provider actually received* — the whole point of inline
//! redaction — plus block/observe modes, failover, plugin ordering, and the
//! streaming path.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::StreamExt;

use himadri_core::{
    AuthContext, AuthScope, ChatCompletionRequest, ChatCompletionResponse, Choice, Config,
    GatewayError, Message, MessageContent, OrgConfig, PiiGuardrailConfig, PiiModeConfig,
    ResponseMessage, Role, StreamChunk, Target, Usage,
};
use himadri_plugin::PluginManager;
use himadri_plugins::{
    EngineSecrets, PiiGuardrailPlugin, PiiGuardrailSettings, PiiMode, RedactCoreEngine,
    WordFilterPlugin,
};
use himadri_provider::error::ProviderError;
use himadri_provider::traits::{BoxStream, Provider};

/// A provider that records every request it is asked to complete, so tests
/// can assert on the exact content that crossed the gateway→provider
/// boundary.
struct CapturingProvider {
    name: String,
    seen: Mutex<Vec<ChatCompletionRequest>>,
    calls: AtomicUsize,
    fail: bool,
    response_text: String,
}

impl CapturingProvider {
    fn new(name: &str) -> Arc<Self> {
        Self::with_response(name, "ok")
    }

    fn with_response(name: &str, response_text: &str) -> Arc<Self> {
        Arc::new(Self {
            name: name.to_string(),
            seen: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
            fail: false,
            response_text: response_text.to_string(),
        })
    }

    fn failing(name: &str) -> Arc<Self> {
        Arc::new(Self {
            name: name.to_string(),
            seen: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
            fail: true,
            response_text: "ok".to_string(),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }

    fn first_seen_text(&self) -> String {
        let seen = self.seen.lock().unwrap();
        seen[0].messages[0]
            .content
            .as_ref()
            .expect("captured message has content")
            .flat_text()
            .into_owned()
    }

    fn response(&self, request: &ChatCompletionRequest) -> ChatCompletionResponse {
        ChatCompletionResponse {
            id: "cap-1".to_string(),
            object: "chat.completion".to_string(),
            created: 0,
            model: request.model.clone(),
            choices: vec![Choice {
                index: 0,
                message: ResponseMessage {
                    role: Role::Assistant,
                    content: Some(self.response_text.clone()),
                    tool_calls: None,
                },
                finish_reason: Some("stop".to_string()),
            }],
            usage: Some(Usage {
                prompt_tokens: 1,
                completion_tokens: 1,
                total_tokens: 2,
            }),
            system_fingerprint: None,
        }
    }
}

#[async_trait]
impl Provider for CapturingProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn display_name(&self) -> &str {
        &self.name
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["cap-model".to_string()]
    }

    async fn complete(
        &self,
        request: &ChatCompletionRequest,
        _api_key: &str,
    ) -> Result<ChatCompletionResponse, ProviderError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.seen.lock().unwrap().push(request.clone());
        if self.fail {
            return Err(ProviderError::Api {
                status: 500,
                message: "capturing provider set to fail".to_string(),
            });
        }
        Ok(self.response(request))
    }

    async fn complete_stream(
        &self,
        request: &ChatCompletionRequest,
        _api_key: &str,
    ) -> Result<BoxStream<'static, Result<StreamChunk, ProviderError>>, ProviderError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.seen.lock().unwrap().push(request.clone());
        if self.fail {
            return Err(ProviderError::Api {
                status: 500,
                message: "capturing provider set to fail".to_string(),
            });
        }
        let content = self.response_text.clone();
        let stream = async_stream::stream! {
            yield Ok(StreamChunk {
                id: "cap-stream-1".to_string(),
                object: "chat.completion.chunk".to_string(),
                created: 0,
                model: "cap-model".to_string(),
                choices: vec![himadri_core::StreamChoice {
                    index: 0,
                    delta: himadri_core::Delta {
                        role: Some(Role::Assistant),
                        content: Some(content),
                        tool_calls: None,
                    },
                    finish_reason: Some("stop".to_string()),
                }],
                usage: None,
                system_fingerprint: None,
            });
        };
        Ok(Box::pin(stream))
    }
}

fn metrics() -> Arc<himadri_observability::Metrics> {
    Arc::new(himadri_observability::Metrics::new())
}

fn target(provider: &str) -> Target {
    Target {
        provider: provider.to_string(),
        weight: 1.0,
        models: None,
        id: None,
        api_key_env: None,
        base_url: None,
    }
}

fn request_with_text(text: &str) -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: "cap-model".to_string(),
        messages: vec![Message {
            role: Role::User,
            content: Some(MessageContent::Text(text.to_string())),
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn pii_settings(mode: PiiMode) -> PiiGuardrailSettings {
    PiiGuardrailSettings {
        mode,
        ..Default::default()
    }
}

/// Gateway with one capturing target and the PII guardrail in the pipeline.
fn gateway_with_pii(providers: &[Arc<CapturingProvider>], mode: PiiMode) -> himadri::Gateway {
    let config = Config {
        targets: providers.iter().map(|p| target(p.name())).collect(),
        // Fallback tries targets in order; with one target it behaves like
        // Single, and the failover test needs the second target reachable.
        strategy: himadri_core::StrategyConfig {
            mode: himadri_core::StrategyMode::Fallback,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut gateway = himadri::Gateway::new(config, metrics());
    for provider in providers {
        gateway.register_provider(provider.clone());
    }

    let engine = RedactCoreEngine::new(EngineSecrets::default()).expect("engine builds");
    let mut pm = PluginManager::new();
    pm.register(PiiGuardrailPlugin::new(engine, pii_settings(mode), None));
    gateway.set_plugin_manager(pm);
    gateway
}

const PII_PROMPT: &str =
    "Please email john@example.com about SSN 123-45-6789 using key sk-abcdefghij0123456789.";

/// Use case: redact mode — the provider receives placeholders, never the
/// raw email/SSN/API key, and the request still succeeds.
#[tokio::test]
async fn redact_mode_provider_sees_redacted_content() {
    let provider = CapturingProvider::new("cap");
    let gateway = gateway_with_pii(std::slice::from_ref(&provider), PiiMode::Redact);

    let response = gateway
        .route(request_with_text(PII_PROMPT), None, None)
        .await
        .expect("redacted request routes successfully");
    assert_eq!(response.choices[0].message.content.as_deref(), Some("ok"));

    let seen = provider.first_seen_text();
    assert!(seen.contains("[EMAIL_ADDRESS]"), "provider saw: {seen}");
    assert!(seen.contains("[US_SSN]"), "provider saw: {seen}");
    assert!(!seen.contains("john@example.com"), "provider saw: {seen}");
    assert!(!seen.contains("123-45-6789"), "provider saw: {seen}");
    assert!(
        !seen.contains("sk-abcdefghij0123456789"),
        "provider saw: {seen}"
    );
}

/// Use case: block mode — the request is rejected with a 400-style error
/// naming entity types (never values) and the provider is never called.
#[tokio::test]
async fn block_mode_rejects_before_provider_dispatch() {
    let provider = CapturingProvider::new("cap");
    let gateway = gateway_with_pii(std::slice::from_ref(&provider), PiiMode::Block);

    let err = gateway
        .route(request_with_text(PII_PROMPT), None, None)
        .await
        .expect_err("PII request must be blocked");
    match err {
        GatewayError::BadRequest(reason) => {
            assert!(reason.contains("US_SSN"), "reason: {reason}");
            assert!(
                !reason.contains("123-45-6789"),
                "reason leaks value: {reason}"
            );
        }
        other => panic!("expected BadRequest, got {other:?}"),
    }
    assert_eq!(provider.calls(), 0, "provider must not be called");
}

/// Use case: observe mode — content is forwarded verbatim.
#[tokio::test]
async fn observe_mode_forwards_content_verbatim() {
    let provider = CapturingProvider::new("cap");
    let gateway = gateway_with_pii(std::slice::from_ref(&provider), PiiMode::Observe);

    gateway
        .route(request_with_text(PII_PROMPT), None, None)
        .await
        .expect("observe mode forwards");
    assert_eq!(provider.first_seen_text(), PII_PROMPT);
}

/// Use case: clean prompts are untouched by redact mode.
#[tokio::test]
async fn clean_prompt_passes_through_redact_mode_unchanged() {
    let provider = CapturingProvider::new("cap");
    let gateway = gateway_with_pii(std::slice::from_ref(&provider), PiiMode::Redact);

    let prompt = "Summarize the quarterly report in three bullet points.";
    gateway
        .route(request_with_text(prompt), None, None)
        .await
        .expect("clean request routes");
    assert_eq!(provider.first_seen_text(), prompt);
}

/// Use case: failover — redaction happens once, before target selection,
/// so the second target sees the same redacted request as the first.
#[tokio::test]
async fn failover_target_receives_redacted_request() {
    let failing = CapturingProvider::failing("cap-a");
    let backup = CapturingProvider::new("cap-b");
    let gateway = gateway_with_pii(&[failing.clone(), backup.clone()], PiiMode::Redact);

    gateway
        .route(request_with_text(PII_PROMPT), None, None)
        .await
        .expect("failover succeeds via backup");

    assert_eq!(failing.calls(), 1);
    assert_eq!(backup.calls(), 1);
    for (name, provider) in [("primary", &failing), ("backup", &backup)] {
        let seen = provider.first_seen_text();
        assert!(
            seen.contains("[EMAIL_ADDRESS]") && !seen.contains("john@example.com"),
            "{name} saw: {seen}"
        );
    }
}

/// Use case: plugin ordering — the word filter runs after the PII guardrail
/// and therefore sees redacted text. A blocklist entry matching the raw
/// email no longer fires once redaction removed it.
#[tokio::test]
async fn word_filter_runs_on_redacted_text() {
    let provider = CapturingProvider::new("cap");
    let config = Config {
        targets: vec![target("cap")],
        ..Default::default()
    };
    let mut gateway = himadri::Gateway::new(config, metrics());
    gateway.register_provider(provider.clone());

    let engine = RedactCoreEngine::new(EngineSecrets::default()).expect("engine builds");
    let mut pm = PluginManager::new();
    pm.register(PiiGuardrailPlugin::new(
        engine,
        pii_settings(PiiMode::Redact),
        None,
    ));
    pm.register(WordFilterPlugin::new(vec!["john@example.com".to_string()]));
    gateway.set_plugin_manager(pm);

    gateway
        .route(request_with_text(PII_PROMPT), None, None)
        .await
        .expect("word filter must not fire on redacted text");
    assert_eq!(provider.calls(), 1);
}

/// Use case: streaming — request-side redaction applies identically on
/// `route_stream`.
#[tokio::test]
async fn streaming_request_is_redacted_before_dispatch() {
    let provider = CapturingProvider::new("cap");
    let gateway = gateway_with_pii(std::slice::from_ref(&provider), PiiMode::Redact);

    let mut request = request_with_text(PII_PROMPT);
    request.stream = true;
    let mut stream = gateway
        .route_stream(request, None, None)
        .await
        .expect("stream opens");
    while let Some(chunk) = stream.next().await {
        chunk.expect("stream chunk ok");
    }

    let seen = provider.first_seen_text();
    assert!(seen.contains("[EMAIL_ADDRESS]"), "provider saw: {seen}");
    assert!(!seen.contains("john@example.com"), "provider saw: {seen}");
}

/// Use case: block mode on a streaming request rejects before any stream
/// opens.
#[tokio::test]
async fn streaming_block_mode_rejects_before_stream_opens() {
    let provider = CapturingProvider::new("cap");
    let gateway = gateway_with_pii(std::slice::from_ref(&provider), PiiMode::Block);

    let mut request = request_with_text(PII_PROMPT);
    request.stream = true;
    match gateway.route_stream(request, None, None).await {
        Err(GatewayError::BadRequest(_)) => {}
        Err(other) => panic!("expected BadRequest, got {other:?}"),
        Ok(_) => panic!("expected the stream request to be blocked"),
    }
    assert_eq!(provider.calls(), 0);
}

// ═══════════════════════════════════════════════════════════════════════
// Config-driven guardrails (Phase 2): org overrides and live reload
// ═══════════════════════════════════════════════════════════════════════

fn pii_section(enabled: bool, mode: PiiModeConfig) -> PiiGuardrailConfig {
    PiiGuardrailConfig {
        enabled,
        mode,
        ..Default::default()
    }
}

fn auth_for_org(org: &str) -> AuthContext {
    AuthContext {
        org_id: Some(org.to_string()),
        scope: AuthScope::ApiKey,
        ..Default::default()
    }
}

/// Gateway whose PII guardrail resolves against the gateway's own live
/// config handle — the production wiring shape (`wire_plugins`).
fn gateway_with_config_driven_pii(
    provider: &Arc<CapturingProvider>,
    config: Config,
) -> himadri::Gateway {
    let mut gateway = himadri::Gateway::new(config, metrics());
    gateway.register_provider(provider.clone());

    let engine: Arc<dyn himadri_plugins::PiiEngine> =
        RedactCoreEngine::new(EngineSecrets::default()).expect("engine builds");
    let mut pm = PluginManager::new();
    pm.register(PiiGuardrailPlugin::with_config(
        engine.clone(),
        None,
        gateway.config_handle(),
        None,
    ));
    pm.register_response_guardrail(himadri_plugins::PiiResponseGuardrail::with_config(
        engine,
        None,
        gateway.config_handle(),
        None,
    ));
    gateway.set_plugin_manager(pm);
    gateway
}

/// Use case: a global redact policy with a per-org block override — the
/// org's requests are rejected while other orgs get redaction.
#[tokio::test]
async fn org_override_beats_global_policy_end_to_end() {
    let provider = CapturingProvider::new("cap");
    let mut config = Config {
        targets: vec![target("cap")],
        ..Default::default()
    };
    config.guardrails.pii = pii_section(true, PiiModeConfig::Redact);
    let mut org = OrgConfig::default();
    org.guardrails.pii = Some(pii_section(true, PiiModeConfig::Block));
    config.orgs.insert("acme".to_string(), org);

    let gateway = gateway_with_config_driven_pii(&provider, config);

    // acme is blocked.
    let err = gateway
        .route(
            request_with_text(PII_PROMPT),
            Some(&auth_for_org("acme")),
            None,
        )
        .await
        .expect_err("acme must be blocked");
    assert!(matches!(err, GatewayError::BadRequest(_)));
    assert_eq!(provider.calls(), 0);

    // Another org falls through to the global redact policy.
    gateway
        .route(
            request_with_text(PII_PROMPT),
            Some(&auth_for_org("globex")),
            None,
        )
        .await
        .expect("globex is redacted, not blocked");
    let seen = provider.first_seen_text();
    assert!(seen.contains("[EMAIL_ADDRESS]"), "provider saw: {seen}");
}

/// Use case: an org opts out of the global policy wholesale
/// (`pii.enabled: false`) — its content is forwarded verbatim.
#[tokio::test]
async fn disabled_org_override_opts_out_end_to_end() {
    let provider = CapturingProvider::new("cap");
    let mut config = Config {
        targets: vec![target("cap")],
        ..Default::default()
    };
    config.guardrails.pii = pii_section(true, PiiModeConfig::Redact);
    let mut org = OrgConfig::default();
    org.guardrails.pii = Some(pii_section(false, PiiModeConfig::Redact));
    config.orgs.insert("acme".to_string(), org);

    let gateway = gateway_with_config_driven_pii(&provider, config);
    gateway
        .route(
            request_with_text(PII_PROMPT),
            Some(&auth_for_org("acme")),
            None,
        )
        .await
        .expect("opted-out org routes verbatim");
    assert_eq!(provider.first_seen_text(), PII_PROMPT);
}

/// Use case: enabling guardrails via an admin config reload takes effect
/// on the next request — no restart, no plugin re-registration.
#[tokio::test]
async fn admin_reload_enables_guardrails_live() {
    let provider = CapturingProvider::new("cap");
    let config = Config {
        targets: vec![target("cap")],
        ..Default::default()
    };
    let gateway = gateway_with_config_driven_pii(&provider, config.clone());

    // Guardrails disabled: forwarded verbatim.
    gateway
        .route(request_with_text(PII_PROMPT), None, None)
        .await
        .expect("routes without guardrails");
    assert_eq!(provider.first_seen_text(), PII_PROMPT);

    // Admin enables the global policy via reload.
    let mut enabled = config;
    enabled.guardrails.pii = pii_section(true, PiiModeConfig::Redact);
    gateway
        .reload_config(enabled)
        .await
        .expect("reload applies");

    gateway
        .route(request_with_text(PII_PROMPT), None, None)
        .await
        .expect("routes with guardrails");
    let seen = {
        let all = provider.seen.lock().unwrap();
        all.last().unwrap().messages[0]
            .content
            .as_ref()
            .unwrap()
            .flat_text()
            .into_owned()
    };
    assert!(seen.contains("[EMAIL_ADDRESS]"), "provider saw: {seen}");
    assert!(!seen.contains("john@example.com"), "provider saw: {seen}");
}

/// Use case: a legacy config still using `content_filter.block_pii` keeps
/// blocking after a reload — the deprecation shim maps it to `pii` block
/// mode on apply.
#[tokio::test]
async fn deprecated_block_pii_still_blocks_via_shim() {
    let provider = CapturingProvider::new("cap");
    let config = Config {
        targets: vec![target("cap")],
        ..Default::default()
    };
    let gateway = gateway_with_config_driven_pii(&provider, config.clone());

    let mut legacy = config;
    let mut org = OrgConfig::default();
    org.guardrails.enabled = true;
    org.guardrails.content_filter = Some(himadri_core::ContentFilterConfig {
        enabled: true,
        block_pii: true,
        ..Default::default()
    });
    legacy.orgs.insert("acme".to_string(), org);
    gateway.reload_config(legacy).await.expect("reload applies");

    let err = gateway
        .route(
            request_with_text(PII_PROMPT),
            Some(&auth_for_org("acme")),
            None,
        )
        .await
        .expect_err("legacy block_pii must still block");
    assert!(matches!(err, GatewayError::BadRequest(_)));
    assert_eq!(provider.calls(), 0);
}

// ═══════════════════════════════════════════════════════════════════════
// Response-side guardrail (Phase 3)
// ═══════════════════════════════════════════════════════════════════════

const PII_RESPONSE: &str = "Sure — reach Jane at jane@corp.org or SSN 123-45-6789.";

fn config_with_response_mode(mode: himadri_core::PiiResponseModeConfig) -> Config {
    let mut config = Config {
        targets: vec![target("cap")],
        ..Default::default()
    };
    config.guardrails.pii = PiiGuardrailConfig {
        enabled: true,
        mode: PiiModeConfig::Redact,
        response_mode: mode,
        ..Default::default()
    };
    config
}

/// Use case: response redaction — the model output's PII is rewritten
/// before the client sees it.
#[tokio::test]
async fn response_redact_rewrites_model_output() {
    let provider = CapturingProvider::with_response("cap", PII_RESPONSE);
    let gateway = gateway_with_config_driven_pii(
        &provider,
        config_with_response_mode(himadri_core::PiiResponseModeConfig::Redact),
    );

    let response = gateway
        .route(request_with_text("hello"), None, None)
        .await
        .expect("routes with response redaction");
    let content = response.choices[0].message.content.as_deref().unwrap();
    assert!(content.contains("[EMAIL_ADDRESS]"), "client saw: {content}");
    assert!(content.contains("[US_SSN]"), "client saw: {content}");
    assert!(!content.contains("jane@corp.org"), "client saw: {content}");
}

/// Use case: response block — PII in model output turns the response into
/// a 400 naming entity types only.
#[tokio::test]
async fn response_block_withholds_model_output() {
    let provider = CapturingProvider::with_response("cap", PII_RESPONSE);
    let gateway = gateway_with_config_driven_pii(
        &provider,
        config_with_response_mode(himadri_core::PiiResponseModeConfig::Block),
    );

    let err = gateway
        .route(request_with_text("hello"), None, None)
        .await
        .expect_err("PII response must be blocked");
    match err {
        GatewayError::BadRequest(reason) => {
            assert!(reason.contains("US_SSN"), "reason: {reason}");
            assert!(
                !reason.contains("123-45-6789"),
                "reason leaks value: {reason}"
            );
        }
        other => panic!("expected BadRequest, got {other:?}"),
    }
}

/// Use case: response_mode off (the default) — model output passes through
/// even when request-side redaction is on.
#[tokio::test]
async fn response_mode_off_passes_output_through() {
    let provider = CapturingProvider::with_response("cap", PII_RESPONSE);
    let gateway = gateway_with_config_driven_pii(
        &provider,
        config_with_response_mode(himadri_core::PiiResponseModeConfig::Off),
    );

    let response = gateway
        .route(request_with_text("hello"), None, None)
        .await
        .expect("routes without response scanning");
    assert_eq!(
        response.choices[0].message.content.as_deref(),
        Some(PII_RESPONSE)
    );
}

/// Use case: streaming — response guardrails at stream end are post-hoc
/// (chunks already delivered), so even block mode must not break the
/// stream. Documented limitation, asserted here so a behavior change is
/// deliberate.
#[tokio::test]
async fn streaming_response_guardrail_is_post_hoc_only() {
    let provider = CapturingProvider::with_response("cap", PII_RESPONSE);
    let gateway = gateway_with_config_driven_pii(
        &provider,
        config_with_response_mode(himadri_core::PiiResponseModeConfig::Block),
    );

    let mut request = request_with_text("hello");
    request.stream = true;
    let mut stream = gateway
        .route_stream(request, None, None)
        .await
        .expect("stream opens");
    let mut delivered = String::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.expect("stream chunk ok");
        for choice in &chunk.choices {
            if let Some(content) = &choice.delta.content {
                delivered.push_str(content);
            }
        }
    }
    // The full (unredacted) content was delivered before the end-of-stream
    // guardrail could act.
    assert_eq!(delivered, PII_RESPONSE);
}
