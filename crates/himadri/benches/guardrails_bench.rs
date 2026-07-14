//! PII guardrail engine benchmarks (docs/SPEC_GUARDRAILS.md §10).
//!
//! Validates the performance budget (p99 added latency < 5 ms for a 4 KiB
//! prompt with the full entity set) and the 16 KiB inline-vs-spawn_blocking
//! threshold, and probes for pathological regex backtracking on
//! adversarial near-miss input.
//!
//! Run with: `cargo bench -p himadri --bench guardrails_bench`

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use himadri_plugins::{EngineSecrets, PiiEngine, RedactCoreEngine, RedactOptions};

/// Realistic prose with no PII, tiled to `len` bytes.
fn clean_text(len: usize) -> String {
    let para = "The quarterly review covered roadmap priorities, hiring plans, \
                and infrastructure costs. Action items were assigned to each \
                workstream lead with follow-ups scheduled for next sprint. ";
    para.repeat(len / para.len() + 1)[..len].to_string()
}

/// Prose with one PII cluster (~email, phone, SSN, card, API key) roughly
/// every 400 bytes.
fn pii_dense_text(len: usize) -> String {
    let para = "Contact John at john.doe@example.com or (555) 123-4567. \
                His SSN is 123-45-6789 and card 4111 1111 1111 1111. \
                The service key is sk-abcdefghij0123456789 for the staging box. ";
    para.repeat(len / para.len() + 1)[..len].to_string()
}

/// Near-miss input: long digit/token runs that *almost* match several
/// recognizers — the worst case for backtracking-prone patterns.
fn adversarial_text(len: usize) -> String {
    let unit = "999-99-999 4111-1111-1111-111 sk-short eyJx.eyJ 123.456.789.999.999 \
                AKIA0123456789ABCDE 12345678901234567890123456789012345678901234567890 ";
    unit.repeat(len / unit.len() + 1)[..len].to_string()
}

fn bench_engine(c: &mut Criterion) {
    let engine = RedactCoreEngine::new(EngineSecrets::default()).expect("engine builds");
    let opts = RedactOptions::default();

    let mut group = c.benchmark_group("pii_guardrail");
    for &size in &[1024usize, 4 * 1024, 16 * 1024, 128 * 1024] {
        group.throughput(Throughput::Bytes(size as u64));

        let clean = clean_text(size);
        group.bench_with_input(BenchmarkId::new("scan_clean", size), &clean, |b, text| {
            b.iter(|| engine.scan(black_box(text), &opts).unwrap())
        });

        let dense = pii_dense_text(size);
        group.bench_with_input(BenchmarkId::new("redact_dense", size), &dense, |b, text| {
            b.iter(|| engine.redact(black_box(text), &opts).unwrap())
        });

        let adversarial = adversarial_text(size);
        group.bench_with_input(
            BenchmarkId::new("scan_adversarial", size),
            &adversarial,
            |b, text| b.iter(|| engine.scan(black_box(text), &opts).unwrap()),
        );
    }
    group.finish();

    // The budget case called out in the SPEC: full entity set, 4 KiB.
    let budget_prompt = pii_dense_text(4 * 1024);
    c.bench_function("pii_guardrail_budget_redact_4k", |b| {
        b.iter(|| engine.redact(black_box(&budget_prompt), &opts).unwrap())
    });
}

criterion_group!(benches, bench_engine);
criterion_main!(benches);
