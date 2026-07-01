//! Live integration test against the real OpenRouter API
//! (`https://openrouter.ai/api/v1`).
//!
//! These tests are **gated on `OPENROUTER_API_KEY`** and are skipped (they
//! no-op and pass) when it is not set, so the default `cargo test` run — and CI
//! without secrets — stays green. To run them:
//!
//! ```bash
//! OPENROUTER_API_KEY=sk-or-... cargo test -p himadri-provider --test openrouter_live -- --nocapture
//! # optional: pick the model (default is a free one)
//! OPENROUTER_TEST_MODEL='google/gemma-4-26b-a4b-it:free' OPENROUTER_API_KEY=... cargo test ...
//! ```
//!
//! OpenRouter's free models are rate-limited upstream; a `429` is treated as a
//! skip (not a failure) since it reflects upstream capacity, not a defect in
//! this crate. Any other error fails the test.

use futures::StreamExt;
use himadri_core::{ChatCompletionRequest, Message, MessageContent, Role};
use himadri_provider::error::ProviderError;
use himadri_provider::traits::Provider;
use himadri_provider::{OpenAiCompatibleConfig, OpenAiCompatibleProvider};

const DEFAULT_MODEL: &str = "google/gemma-4-26b-a4b-it:free";

/// Returns `Some(key)` when configured, otherwise prints a skip notice and
/// returns `None`.
fn api_key_or_skip(test: &str) -> Option<String> {
    match std::env::var("OPENROUTER_API_KEY") {
        Ok(k) if !k.is_empty() => Some(k),
        _ => {
            eprintln!("[skip] {test}: OPENROUTER_API_KEY not set");
            None
        }
    }
}

fn test_model() -> String {
    std::env::var("OPENROUTER_TEST_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string())
}

fn chat_request(model: &str, prompt: &str, stream: bool) -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: model.to_string(),
        messages: vec![Message {
            role: Role::User,
            content: Some(MessageContent::Text(prompt.to_string())),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }],
        stream,
        temperature: Some(0.0),
        top_p: None,
        max_tokens: Some(64),
        stop: None,
        presence_penalty: None,
        frequency_penalty: None,
        user: None,
        tools: None,
        tool_choice: None,
        extra: Default::default(),
    }
}

/// True for errors that reflect upstream free-tier capacity rather than a
/// defect in our integration, so the test should skip rather than fail.
fn is_upstream_capacity_error(e: &ProviderError) -> bool {
    matches!(
        e,
        ProviderError::RateLimited { .. }
            | ProviderError::Api {
                status: 429 | 502 | 503,
                ..
            }
    )
}

#[tokio::test]
async fn openrouter_live_chat_completion() {
    let Some(key) = api_key_or_skip("openrouter_live_chat_completion") else {
        return;
    };
    let provider = OpenAiCompatibleProvider::new(OpenAiCompatibleConfig::openrouter());
    let model = test_model();
    let req = chat_request(&model, "Reply with a short greeting.", false);

    match provider.complete(&req, &key).await {
        Ok(resp) => {
            assert!(!resp.choices.is_empty(), "expected at least one choice");
            let msg = &resp.choices[0].message;
            let content = msg.content.clone().unwrap_or_default();
            assert!(
                !content.trim().is_empty() || msg.tool_calls.is_some(),
                "expected non-empty content or tool calls, got: {resp:?}"
            );
            // Usage should be populated and parsed.
            let usage = resp.usage.expect("usage should be present");
            assert!(usage.total_tokens > 0, "expected non-zero token usage");
            eprintln!("[ok] openrouter completion ({}): {content}", resp.model);
        }
        Err(e) if is_upstream_capacity_error(&e) => {
            eprintln!("[skip] openrouter rate-limited/unavailable upstream: {e}");
        }
        Err(e) => panic!("openrouter completion failed: {e}"),
    }
}

#[tokio::test]
async fn openrouter_live_streaming() {
    let Some(key) = api_key_or_skip("openrouter_live_streaming") else {
        return;
    };
    let provider = OpenAiCompatibleProvider::new(OpenAiCompatibleConfig::openrouter());
    let model = test_model();
    let req = chat_request(&model, "Count: one two three", true);

    let mut stream = match provider.complete_stream(&req, &key).await {
        Ok(s) => s,
        Err(e) if is_upstream_capacity_error(&e) => {
            eprintln!("[skip] openrouter stream rate-limited/unavailable upstream: {e}");
            return;
        }
        Err(e) => panic!("openrouter stream open failed: {e}"),
    };

    let mut text = String::new();
    let mut chunks = 0usize;
    let mut saw_finish = false;
    while let Some(item) = stream.next().await {
        match item {
            Ok(chunk) => {
                chunks += 1;
                if let Some(choice) = chunk.choices.first() {
                    if let Some(c) = &choice.delta.content {
                        text.push_str(c);
                    }
                    if choice.finish_reason.is_some() {
                        saw_finish = true;
                    }
                }
            }
            Err(e) if is_upstream_capacity_error(&e) => {
                eprintln!("[skip] openrouter stream interrupted by upstream limit: {e}");
                return;
            }
            Err(e) => panic!("openrouter stream chunk error: {e}"),
        }
    }

    assert!(chunks > 0, "expected at least one stream chunk");
    assert!(saw_finish, "expected a finish_reason in the stream");
    eprintln!("[ok] openrouter stream ({chunks} chunks): {text}");
}
