use criterion::{black_box, criterion_group, criterion_main, Criterion};
use himadri_circuitbreaker::{CircuitBreaker, CircuitBreakerConfig};
use himadri_core::{ChatCompletionRequest, Message, MessageContent, Role};
use himadri_ratelimit::ShardedRateLimiter;

fn bench_circuit_breaker(c: &mut Criterion) {
    let cb = CircuitBreaker::new(CircuitBreakerConfig::default());

    c.bench_function("circuit_breaker_allow", |b| b.iter(|| cb.allow()));

    c.bench_function("circuit_breaker_record_success", |b| {
        b.iter(|| cb.record_success())
    });
}

fn bench_rate_limiter(c: &mut Criterion) {
    let limiter = ShardedRateLimiter::new(10000, 20000, 64);

    c.bench_function("rate_limiter_allow_new_key", |b| {
        let mut i = 0u64;
        b.iter(|| {
            i += 1;
            limiter.allow(&format!("key-{}", i))
        })
    });

    c.bench_function("rate_limiter_allow_existing_key", |b| {
        limiter.allow("stable-key");
        b.iter(|| limiter.allow("stable-key"))
    });
}

fn bench_request_parsing(c: &mut Criterion) {
    let body = serde_json::json!({
        "model": "gpt-4",
        "messages": [
            {"role": "user", "content": "Hello, world!"}
        ],
        "temperature": 0.7,
        "max_tokens": 100
    });

    c.bench_function("parse_chat_request", |b| {
        b.iter(|| serde_json::from_value::<ChatCompletionRequest>(black_box(body.clone())).unwrap())
    });
}

fn bench_request_building(c: &mut Criterion) {
    let request = ChatCompletionRequest {
        model: "gpt-4".to_string(),
        messages: vec![Message {
            role: Role::User,
            content: Some(MessageContent::Text("Hello".to_string())),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }],
        stream: false,
        temperature: Some(0.7),
        top_p: None,
        max_tokens: Some(100),
        stop: None,
        presence_penalty: None,
        frequency_penalty: None,
        user: None,
        tools: None,
        tool_choice: None,
        extra: Default::default(),
    };

    c.bench_function("serialize_chat_request", |b| {
        b.iter(|| serde_json::to_string(black_box(&request)).unwrap())
    });
}

criterion_group!(
    benches,
    bench_circuit_breaker,
    bench_rate_limiter,
    bench_request_parsing,
    bench_request_building,
);
criterion_main!(benches);
