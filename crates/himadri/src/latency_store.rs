use async_trait::async_trait;
use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, Ordering};

#[async_trait]
pub trait LatencyStore: Send + Sync {
    async fn record(&self, provider: &str, latency_ms: u64);
    async fn get_avg_latency(&self, provider: &str) -> u64;
}

pub struct InMemoryLatencyStore {
    stats: DashMap<String, LatencyEntry>,
}

struct LatencyEntry {
    sum: AtomicU64,
    count: AtomicU64,
}

impl InMemoryLatencyStore {
    pub fn new() -> Self {
        Self {
            stats: DashMap::new(),
        }
    }
}

impl Default for InMemoryLatencyStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LatencyStore for InMemoryLatencyStore {
    async fn record(&self, provider: &str, latency_ms: u64) {
        let entry = self
            .stats
            .entry(provider.to_string())
            .or_insert_with(|| LatencyEntry {
                sum: AtomicU64::new(0),
                count: AtomicU64::new(0),
            });
        entry.sum.fetch_add(latency_ms, Ordering::Relaxed);
        entry.count.fetch_add(1, Ordering::Relaxed);
    }

    async fn get_avg_latency(&self, provider: &str) -> u64 {
        self.stats
            .get(provider)
            .map(|entry| {
                let sum = entry.sum.load(Ordering::Relaxed);
                let count = entry.count.load(Ordering::Relaxed);
                sum.checked_div(count).unwrap_or(u64::MAX)
            })
            .unwrap_or(u64::MAX)
    }
}
