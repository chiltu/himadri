mod token_bucket;

#[cfg(feature = "redis")]
mod redis_store;

pub use token_bucket::{ShardedRateLimiter, TokenBucket};

#[cfg(feature = "redis")]
pub use redis_store::RedisRateLimiter;

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

    pub fn check_org(&self, org_id: &str, rate: Option<u64>, burst: Option<u64>) -> bool {
        let r = rate.unwrap_or(self.default_rate);
        let b = burst.unwrap_or(self.default_burst);
        self.per_entity
            .allow_with_params(&format!("org:{}", org_id), r, b)
    }

    pub fn check_key(&self, key_id: &str, rate: Option<u64>, burst: Option<u64>) -> bool {
        let r = rate.unwrap_or(self.default_rate);
        let b = burst.unwrap_or(self.default_burst);
        self.per_entity
            .allow_with_params(&format!("key:{}", key_id), r, b)
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
