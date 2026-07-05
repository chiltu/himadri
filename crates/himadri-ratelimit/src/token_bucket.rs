use parking_lot::RwLock;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Process-wide monotonic epoch. Refill math must not go backwards, so time
/// is measured against this `Instant` rather than `SystemTime` (which can
/// step backwards under NTP and freeze or flood the refill).
static EPOCH: once_cell::sync::Lazy<Instant> = once_cell::sync::Lazy::new(Instant::now);

fn now_micros() -> u64 {
    EPOCH.elapsed().as_micros() as u64
}

/// Lock-free token bucket rate limiter.
///
/// Both consume and refill are CAS-based: consume uses `fetch_update` with
/// `checked_sub` so concurrent callers can never drive the counter below
/// zero (an unconditional `fetch_sub` would wrap the `AtomicU64` to
/// `u64::MAX`, disabling the limit), and only the thread that wins the
/// `last_refill` CAS credits tokens, so racing refillers cannot double-credit.
pub struct TokenBucket {
    tokens: AtomicU64,
    last_refill: AtomicU64,
    rate: u64,
    capacity: u64,
}

impl TokenBucket {
    pub fn new(rate: u64, capacity: u64) -> Self {
        Self {
            tokens: AtomicU64::new(capacity),
            last_refill: AtomicU64::new(now_micros()),
            rate,
            capacity,
        }
    }

    /// The refill rate (tokens per second) this bucket was built with.
    pub fn rate(&self) -> u64 {
        self.rate
    }

    /// The burst capacity this bucket was built with.
    pub fn capacity(&self) -> u64 {
        self.capacity
    }

    pub fn allow(&self) -> bool {
        self.allow_n(1)
    }

    pub fn allow_n(&self, n: u64) -> bool {
        self.refill();
        self.tokens
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |cur| {
                cur.checked_sub(n)
            })
            .is_ok()
    }

    fn refill(&self) {
        let now = now_micros();
        let last = self.last_refill.load(Ordering::Acquire);
        let credit = now.saturating_sub(last).saturating_mul(self.rate) / 1_000_000;
        if credit == 0 {
            return;
        }
        // Only the CAS winner credits tokens; losers retry on their next call.
        if self
            .last_refill
            .compare_exchange(last, now, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            let _ = self
                .tokens
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |cur| {
                    Some(cur.saturating_add(credit).min(self.capacity))
                });
        }
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
        self.allow_with_params(key, self.rate, self.capacity)
    }

    /// Check the bucket for `key`, creating it with (`rate`, `capacity`) on
    /// first sight and **rebuilding** it if the requested parameters differ
    /// from the ones it was built with (so per-key/org overrides changed at
    /// runtime take effect). Insertion goes through the `entry` API so a
    /// racing creator can never overwrite — and thereby refill — a bucket
    /// another thread just created.
    pub fn allow_with_params(&self, key: &str, rate: u64, capacity: u64) -> bool {
        let shard_idx = self.shard_index(key);
        let shard = &self.shards[shard_idx];

        {
            let map = shard.read();
            if let Some(bucket) = map.get(key) {
                if bucket.rate() == rate && bucket.capacity() == capacity {
                    return bucket.allow();
                }
            }
        }

        let mut map = shard.write();
        let bucket = map
            .entry(key.to_string())
            .or_insert_with(|| std::sync::Arc::new(TokenBucket::new(rate, capacity)));
        if bucket.rate() != rate || bucket.capacity() != capacity {
            *bucket = std::sync::Arc::new(TokenBucket::new(rate, capacity));
        }
        bucket.allow()
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
    use std::sync::Arc;

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

    /// Regression: concurrent callers used to race the check-then-subtract
    /// and wrap the counter to `u64::MAX`, admitting everything. With a
    /// zero refill rate, exactly `capacity` requests may ever succeed no
    /// matter how many threads hammer the bucket.
    #[test]
    fn concurrent_consumption_never_exceeds_capacity() {
        let bucket = Arc::new(TokenBucket::new(0, 1_000));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let bucket = bucket.clone();
            handles.push(std::thread::spawn(move || {
                (0..10_000).filter(|_| bucket.allow()).count()
            }));
        }
        let admitted: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
        assert_eq!(admitted, 1_000);
    }

    /// Regression: changed per-key parameters used to be silently ignored
    /// once a bucket existed. A parameter change must rebuild the bucket.
    #[test]
    fn param_change_rebuilds_bucket() {
        let limiter = ShardedRateLimiter::new(100, 200, 4);
        // Exhaust a capacity-2 bucket.
        assert!(limiter.allow_with_params("k", 0, 2));
        assert!(limiter.allow_with_params("k", 0, 2));
        assert!(!limiter.allow_with_params("k", 0, 2));
        // Raising the capacity takes effect immediately (fresh bucket).
        assert!(limiter.allow_with_params("k", 0, 5));
        // And the rebuilt bucket enforces its own new limit.
        for _ in 0..4 {
            assert!(limiter.allow_with_params("k", 0, 5));
        }
        assert!(!limiter.allow_with_params("k", 0, 5));
    }

    /// Racing creators must share one bucket; an unconditional insert used
    /// to overwrite (and thereby refill) a bucket created by another thread.
    #[test]
    fn concurrent_creation_shares_one_bucket() {
        let limiter = Arc::new(ShardedRateLimiter::new(0, 100, 4));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let limiter = limiter.clone();
            handles.push(std::thread::spawn(move || {
                (0..1_000)
                    .filter(|_| limiter.allow_with_params("shared", 0, 100))
                    .count()
            }));
        }
        let admitted: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
        assert_eq!(admitted, 100);
    }
}
