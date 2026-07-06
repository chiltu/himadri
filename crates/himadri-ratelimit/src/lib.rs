mod token_bucket;

pub use token_bucket::{ShardedRateLimiter, TokenBucket};

use std::sync::Arc;

pub struct RateLimiter {
    global: Arc<ShardedRateLimiter>,
    per_entity: Arc<ShardedRateLimiter>,
    default_rate: u64,
    default_burst: u64,
}

impl RateLimiter {
    pub fn new(default_rate: u64, default_burst: u64) -> Self {
        Self {
            global: Arc::new(ShardedRateLimiter::new(default_rate, default_burst, 64)),
            per_entity: Arc::new(ShardedRateLimiter::new(default_rate, default_burst, 64)),
            default_rate,
            default_burst,
        }
    }

    pub fn check_global(&self) -> bool {
        self.global.allow("global")
    }

    fn check_entity(&self, prefix: &str, id: &str, rate: Option<u64>, burst: Option<u64>) -> bool {
        let r = rate.unwrap_or(self.default_rate);
        let b = burst.unwrap_or(self.default_burst);
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
