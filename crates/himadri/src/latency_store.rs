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

#[cfg(feature = "redis")]
pub struct RedisLatencyStore {
    client: redis::Client,
    prefix: String,
    window_secs: u64,
}

#[cfg(feature = "redis")]
impl RedisLatencyStore {
    pub async fn new(redis_url: &str, window_secs: u64) -> Result<Self, redis::RedisError> {
        let client = redis::Client::open(redis_url)?;
        let mut conn = client.get_async_connection().await?;
        redis::cmd("PING").query_async::<_, ()>(&mut conn).await?;
        Ok(Self {
            client,
            prefix: "himadri:latency:".to_string(),
            window_secs,
        })
    }
}

#[cfg(feature = "redis")]
#[async_trait]
impl LatencyStore for RedisLatencyStore {
    async fn record(&self, provider: &str, latency_ms: u64) {
        let mut conn = match self.client.get_async_connection().await {
            Ok(c) => c,
            Err(_) => return,
        };

        let key = format!("{}{}", self.prefix, provider);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        // Add to sorted set with timestamp as score
        let member = format!("{}:{}", now, latency_ms);
        let _: () = redis::cmd("ZADD")
            .arg(&key)
            .arg(now)
            .arg(&member)
            .query_async::<_, ()>(&mut conn)
            .await
            .unwrap_or(());

        // Remove entries outside the window
        let window_start = now.saturating_sub(self.window_secs * 1000);
        let _: () = redis::cmd("ZREMRANGEBYSCORE")
            .arg(&key)
            .arg(0)
            .arg(window_start)
            .query_async::<_, ()>(&mut conn)
            .await
            .unwrap_or(());

        // Set TTL
        let _: () = redis::cmd("EXPIRE")
            .arg(&key)
            .arg(self.window_secs + 10)
            .query_async::<_, ()>(&mut conn)
            .await
            .unwrap_or(());
    }

    async fn get_avg_latency(&self, provider: &str) -> u64 {
        let mut conn = match self.client.get_async_connection().await {
            Ok(c) => c,
            Err(_) => return u64::MAX,
        };

        let key = format!("{}{}", self.prefix, provider);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let window_start = now.saturating_sub(self.window_secs * 1000);

        // Get all members in the window
        let members: Vec<String> = redis::cmd("ZRANGEBYSCORE")
            .arg(&key)
            .arg(window_start)
            .arg("+inf")
            .query_async(&mut conn)
            .await
            .unwrap_or_default();

        if members.is_empty() {
            return u64::MAX;
        }

        // Extract latency values (format: "timestamp:latency_ms")
        let mut total: u64 = 0;
        let mut count: u64 = 0;
        for member in &members {
            if let Some(latency_str) = member.rsplit(':').next() {
                if let Ok(latency) = latency_str.parse::<u64>() {
                    total += latency;
                    count += 1;
                }
            }
        }

        total.checked_div(count).unwrap_or(u64::MAX)
    }
}
