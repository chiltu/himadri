mod token_bucket;

pub use token_bucket::{ShardedRateLimiter, TokenBucket};

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub struct RateLimiter {
    global: Arc<ShardedRateLimiter>,
    per_entity: Arc<ShardedRateLimiter>,
    // Atomic so a config hot-reload can retune the limits without replacing
    // the limiter (see `set_defaults`). `ShardedRateLimiter::allow_with_params`
    // rebuilds a bucket whose parameters changed, so new values take effect on
    // the next check.
    default_rate: AtomicU64,
    default_burst: AtomicU64,
}

impl RateLimiter {
    pub fn new(default_rate: u64, default_burst: u64) -> Self {
        Self {
            global: Arc::new(ShardedRateLimiter::new(default_rate, default_burst, 64)),
            per_entity: Arc::new(ShardedRateLimiter::new(default_rate, default_burst, 64)),
            default_rate: AtomicU64::new(default_rate),
            default_burst: AtomicU64::new(default_burst),
        }
    }

    /// Retune the default rate/burst applied to the global bucket and to
    /// entities without an explicit override. Existing buckets are rebuilt on
    /// their next check.
    pub fn set_defaults(&self, rate: u64, burst: u64) {
        self.default_rate.store(rate, Ordering::Relaxed);
        self.default_burst.store(burst, Ordering::Relaxed);
    }

    pub fn check_global(&self) -> bool {
        self.global.allow_with_params(
            "global",
            self.default_rate.load(Ordering::Relaxed),
            self.default_burst.load(Ordering::Relaxed),
        )
    }

    fn check_entity(&self, prefix: &str, id: &str, rate: Option<u64>, burst: Option<u64>) -> bool {
        let r = rate.unwrap_or_else(|| self.default_rate.load(Ordering::Relaxed));
        let b = burst.unwrap_or_else(|| self.default_burst.load(Ordering::Relaxed));
        self.per_entity
            .allow_with_params(&format!("{prefix}:{id}"), r, b)
    }

    pub fn check_org(&self, org_id: &str, rate: Option<u64>, burst: Option<u64>) -> bool {
        self.check_entity("org", org_id, rate, burst)
    }

    pub fn check_key(&self, key_id: &str, rate: Option<u64>, burst: Option<u64>) -> bool {
        self.check_entity("key", key_id, rate, burst)
    }

    pub fn clear(&self) {
        self.global.clear();
        self.per_entity.clear();
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new(100, 200)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `set_defaults` retune must take effect on the very next check, for
    /// both the global bucket and per-entity buckets without overrides.
    #[test]
    fn set_defaults_applies_without_restart() {
        let limiter = RateLimiter::new(0, 2);
        assert!(limiter.check_global());
        assert!(limiter.check_global());
        assert!(!limiter.check_global());

        limiter.set_defaults(0, 5);
        for _ in 0..5 {
            assert!(limiter.check_global());
        }
        assert!(!limiter.check_global());

        // Per-key buckets pick up the new defaults too.
        for _ in 0..5 {
            assert!(limiter.check_key("k1", None, None));
        }
        assert!(!limiter.check_key("k1", None, None));
    }

    /// Explicit per-entity overrides still win over the defaults.
    #[test]
    fn explicit_override_beats_defaults() {
        let limiter = RateLimiter::new(0, 100);
        assert!(limiter.check_org("o1", Some(0), Some(1)));
        assert!(!limiter.check_org("o1", Some(0), Some(1)));
    }
}
