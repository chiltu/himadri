use parking_lot::RwLock;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

/// Lock-free token bucket rate limiter.
pub struct TokenBucket {
    tokens: AtomicU64,
    last_refill: AtomicI64,
    rate: u64,
    capacity: u64,
}

impl TokenBucket {
    pub fn new(rate: u64, capacity: u64) -> Self {
        Self {
            tokens: AtomicU64::new(capacity),
            last_refill: AtomicI64::new(Self::now_micros()),
            rate,
            capacity,
        }
    }

    pub fn allow(&self) -> bool {
        self.allow_n(1)
    }

    pub fn allow_n(&self, n: u64) -> bool {
        let now = Self::now_micros();
        let last = self.last_refill.load(Ordering::Acquire);

        let elapsed_us = now.saturating_sub(last);
        let refill = (elapsed_us as u64) * self.rate / 1_000_000;

        if refill > 0 {
            let _ = self.last_refill.compare_exchange_weak(
                last,
                now,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            let current = self.tokens.load(Ordering::Relaxed);
            let new_tokens = (current + refill).min(self.capacity);
            self.tokens.store(new_tokens, Ordering::Relaxed);
        }

        let current = self.tokens.load(Ordering::Acquire);
        if current >= n {
            self.tokens.fetch_sub(n, Ordering::AcqRel);
            true
        } else {
            false
        }
    }

    fn now_micros() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros() as i64
    }
}

/// Sharded rate limiter store for reduced contention.
pub struct ShardedRateLimiter {
    shards: Vec<RwLock<HashMap<String, std::sync::Arc<TokenBucket>>>>,
    num_shards: usize,
    rate: u64,
    capacity: u64,
}

impl ShardedRateLimiter {
    pub fn new(rate: u64, capacity: u64, num_shards: usize) -> Self {
        let mut shards = Vec::with_capacity(num_shards);
        for _ in 0..num_shards {
            shards.push(RwLock::new(HashMap::new()));
        }
        Self {
            shards,
            num_shards,
            rate,
            capacity,
        }
    }

    pub fn allow(&self, key: &str) -> bool {
        let shard_idx = self.shard_index(key);
        let shard = &self.shards[shard_idx];

        {
            let map = shard.read();
            if let Some(bucket) = map.get(key) {
                return bucket.allow();
            }
        }

        let mut map = shard.write();
        let bucket = map
            .entry(key.to_string())
            .or_insert_with(|| std::sync::Arc::new(TokenBucket::new(self.rate, self.capacity)));
        bucket.allow()
    }

    pub fn allow_with_params(&self, key: &str, rate: u64, capacity: u64) -> bool {
        let shard_idx = self.shard_index(key);
        let shard = &self.shards[shard_idx];

        {
            let map = shard.read();
            if let Some(bucket) = map.get(key) {
                return bucket.allow();
            }
        }

        let mut map = shard.write();
        map.insert(
            key.to_string(),
            std::sync::Arc::new(TokenBucket::new(rate, capacity)),
        );
        map.get(key).unwrap().allow()
    }

    fn shard_index(&self, key: &str) -> usize {
        let mut hasher = ahash::AHasher::default();
        key.hash(&mut hasher);
        hasher.finish() as usize % self.num_shards
    }

    pub fn clear(&self) {
        for shard in &self.shards {
            shard.write().clear();
        }
    }
}

impl Default for ShardedRateLimiter {
    fn default() -> Self {
        Self::new(100, 200, 64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_bucket_allows_within_rate() {
        let bucket = TokenBucket::new(10, 10);
        for _ in 0..10 {
            assert!(bucket.allow());
        }
        assert!(!bucket.allow());
    }

    #[test]
    fn test_sharded_rate_limiter() {
        let limiter = ShardedRateLimiter::new(10, 10, 4);
        for _ in 0..10 {
            assert!(limiter.allow("user1"));
        }
        assert!(!limiter.allow("user1"));
        assert!(limiter.allow("user2"));
    }

    #[test]
    fn test_allow_with_params() {
        let limiter = ShardedRateLimiter::new(100, 200, 4);
        // Key with rate=5, capacity=5
        for _ in 0..5 {
            assert!(limiter.allow_with_params("user1", 5, 5));
        }
        assert!(!limiter.allow_with_params("user1", 5, 5));
        // Different key with higher rate
        assert!(limiter.allow_with_params("user2", 100, 100));
    }
}
