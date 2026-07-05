use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use himadri_plugin::context::PluginContext;
use himadri_plugin::traits::{Plugin, PluginError, PluginType, Stage};

// ─── Sliding Window Counter ──────────────────────────────────────────

/// A fixed-window counter that tracks requests within a time window.
/// (Counts reset at window boundaries rather than sliding continuously.)
///
/// Consumption is a bound-checked CAS so concurrent callers can never
/// exceed `max_requests` within one window view, and only the thread that
/// wins the window-rollover CAS resets the count — an unconditional reset
/// used to let racing callers zero each other's counts.
struct FixedWindowCounter {
    /// Packed `(window_index << 32) | count`. Keeping the window id and the
    /// count in one atomic makes rollover + consume a single CAS domain: a
    /// rollover can never wipe consumption another thread just recorded in
    /// the new window (the flaw in the separate-atomics design).
    packed: AtomicU64,
    /// Window duration in microseconds (>= 1).
    window_us: u64,
    /// Maximum requests allowed in the window (clamped to u32::MAX).
    max_requests: u64,
}

const COUNT_MASK: u64 = 0xFFFF_FFFF;

impl FixedWindowCounter {
    fn new(max_requests: u64, window: Duration) -> Self {
        let window_us = (window.as_micros() as u64).max(1);
        Self {
            packed: AtomicU64::new(0),
            window_us,
            max_requests: max_requests.min(COUNT_MASK),
        }
    }

    fn window_index(&self, now_us: u64) -> u64 {
        (now_us / self.window_us) & COUNT_MASK
    }

    /// Try to consume one request. Returns true if allowed.
    fn allow(&self) -> bool {
        self.allow_n(1)
    }

    /// Try to consume N requests. Returns true if allowed.
    fn allow_n(&self, n: u64) -> bool {
        let w = self.window_index(now_micros() as u64);
        self.packed
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |packed| {
                let count = if packed >> 32 == w {
                    packed & COUNT_MASK
                } else {
                    0 // fresh window
                };
                let next = count.saturating_add(n);
                (next <= self.max_requests).then_some((w << 32) | next)
            })
            .is_ok()
    }

    /// Get current count in the window.
    fn count(&self) -> u64 {
        let w = self.window_index(now_micros() as u64);
        let packed = self.packed.load(Ordering::Acquire);
        if packed >> 32 == w {
            packed & COUNT_MASK
        } else {
            0
        }
    }

    /// Reset the counter.
    fn reset(&self) {
        let w = self.window_index(now_micros() as u64);
        self.packed.store(w << 32, Ordering::Release);
    }
}

/// Monotonic microseconds: window rollover must not jump when the wall
/// clock steps (NTP).
fn now_micros() -> i64 {
    static EPOCH: once_cell::sync::Lazy<std::time::Instant> =
        once_cell::sync::Lazy::new(std::time::Instant::now);
    EPOCH.elapsed().as_micros() as i64
}

// ─── Sharded Rate Limiter Store ──────────────────────────────────────

/// Per-key rate limiter store with LRU eviction.
/// Uses sharding to reduce lock contention across keys.
struct RateLimiterStore {
    shards: Vec<parking_lot::RwLock<HashMap<String, Arc<FixedWindowCounter>>>>,
    num_shards: usize,
    rate: u64,
    window: Duration,
    max_keys: usize,
}

impl RateLimiterStore {
    fn new(rate: u64, window: Duration, max_keys: usize, num_shards: usize) -> Self {
        let mut shards = Vec::with_capacity(num_shards);
        for _ in 0..num_shards {
            shards.push(parking_lot::RwLock::new(HashMap::new()));
        }
        Self {
            shards,
            num_shards,
            rate,
            window,
            max_keys,
        }
    }

    fn allow(&self, key: &str) -> bool {
        let shard_idx = self.shard_index(key);
        let shard = &self.shards[shard_idx];

        // Fast path: check if key exists
        {
            let map = shard.read();
            if let Some(counter) = map.get(key) {
                return counter.allow();
            }
        }

        // Slow path: create new counter
        let mut map = shard.write();

        // Check again after acquiring write lock
        if let Some(counter) = map.get(key) {
            return counter.allow();
        }

        // Evict if at capacity
        if self.max_keys > 0 && map.len() >= self.max_keys {
            // Evict a random key to prevent targeted eviction attacks
            let idx = rand::random::<usize>() % map.len();
            if let Some(evict_key) = map.keys().nth(idx).cloned() {
                map.remove(&evict_key);
            }
        }

        let counter = Arc::new(FixedWindowCounter::new(self.rate, self.window));
        let allowed = counter.allow();
        map.insert(key.to_string(), counter);
        allowed
    }

    fn get_count(&self, key: &str) -> u64 {
        let shard_idx = self.shard_index(key);
        let map = self.shards[shard_idx].read();
        map.get(key).map(|c| c.count()).unwrap_or(0)
    }

    fn reset(&self, key: &str) {
        let shard_idx = self.shard_index(key);
        let map = self.shards[shard_idx].read();
        if let Some(counter) = map.get(key) {
            counter.reset();
        }
    }

    fn reset_all(&self) {
        for shard in &self.shards {
            let mut map = shard.write();
            map.clear();
        }
    }

    fn shard_index(&self, key: &str) -> usize {
        use std::hash::{Hash, Hasher};
        let mut hasher = ahash::AHasher::default();
        key.hash(&mut hasher);
        hasher.finish() as usize % self.num_shards
    }
}

// ─── Global Rate Limiter Stores ──────────────────────────────────────

static GLOBAL_STORES: once_cell::sync::Lazy<himadri_plugin::StoreRegistry<RateLimiterStore>> =
    once_cell::sync::Lazy::new(Default::default);

fn get_or_create_store(
    store_id: &str,
    rate: u64,
    window: Duration,
    max_keys: usize,
) -> Arc<RateLimiterStore> {
    GLOBAL_STORES.get_or_create(store_id, || {
        RateLimiterStore::new(rate, window, max_keys, 64)
    })
}

/// Reset rate limit for a specific key.
pub fn reset_store_key(store_id: &str, key: &str) {
    GLOBAL_STORES.with(store_id, |s| s.reset(key));
}

/// Reset all rate limits in a store.
pub fn reset_store(store_id: &str) {
    GLOBAL_STORES.with(store_id, |s| s.reset_all());
}

/// Get current request count for a key in the current window.
pub fn get_request_count(store_id: &str, key: &str) -> u64 {
    GLOBAL_STORES
        .with(store_id, |s| s.get_count(key))
        .unwrap_or(0)
}

// ─── Rate Limit Plugin ───────────────────────────────────────────────

const DEFAULT_MAX_KEYS: usize = 100_000;

pub struct RateLimitPlugin {
    /// Global rate limiter (requests per window)
    global_limiter: Arc<FixedWindowCounter>,
    /// Per-API-key rate limiter store
    key_store: Option<Arc<RateLimiterStore>>,
    /// Per-user rate limiter store
    user_store: Option<Arc<RateLimiterStore>>,
    /// Per-IP rate limiter store
    ip_store: Option<Arc<RateLimiterStore>>,
}

impl RateLimitPlugin {
    pub fn new(config: RateLimitConfig) -> Result<Arc<Self>, String> {
        let rps = config.requests_per_second.unwrap_or(100);
        let window = Duration::from_secs(1);
        let max_keys = config.max_keys.unwrap_or(DEFAULT_MAX_KEYS);

        // Create global limiter
        let global_limiter = Arc::new(FixedWindowCounter::new(rps, window));

        // Create per-key store if configured
        let key_store = config.key_rpm.map(|rpm| {
            get_or_create_store(
                &format!("key-{}", config.store_id.as_deref().unwrap_or("default")),
                rpm,                     // max_requests per window (the RPM value itself)
                Duration::from_secs(60), // 1-minute window for RPM
                max_keys,
            )
        });

        // Create per-user store if configured
        let user_store = config.user_rpm.map(|rpm| {
            get_or_create_store(
                &format!("user-{}", config.store_id.as_deref().unwrap_or("default")),
                rpm,
                Duration::from_secs(60),
                max_keys,
            )
        });

        // Create per-IP store if configured
        let ip_store = config.ip_rpm.map(|rpm| {
            get_or_create_store(
                &format!("ip-{}", config.store_id.as_deref().unwrap_or("default")),
                rpm,
                Duration::from_secs(60),
                max_keys,
            )
        });

        Ok(Arc::new(Self {
            global_limiter,
            key_store,
            user_store,
            ip_store,
        }))
    }
}

#[derive(Debug, Clone, Default)]
pub struct RateLimitConfig {
    pub store_id: Option<String>,
    pub requests_per_second: Option<u64>,
    pub key_rpm: Option<u64>,
    pub user_rpm: Option<u64>,
    pub ip_rpm: Option<u64>,
    pub max_keys: Option<usize>,
}

#[async_trait]
impl Plugin for RateLimitPlugin {
    fn name(&self) -> &str {
        "rate-limit"
    }

    fn plugin_type(&self) -> PluginType {
        PluginType::Middleware
    }

    fn stage(&self) -> Stage {
        Stage::BeforeRequest
    }

    async fn execute(&self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        // 1. Check global rate limit
        if !self.global_limiter.allow() {
            return Err(PluginError::Rejected {
                name: self.name().to_string(),
                reason: "rate limit exceeded".to_string(),
                kind: himadri_plugin::RejectKind::RateLimited {
                    retry_after_secs: 60,
                },
            });
        }

        // 2. Check per-API-key rate limit
        if let Some(ref key_store) = self.key_store {
            if let Some(key) = Self::get_api_key(ctx) {
                if !key_store.allow(&key) {
                    return Err(PluginError::Rejected {
                        name: self.name().to_string(),
                        reason: "per-key rate limit exceeded".to_string(),
                        kind: himadri_plugin::RejectKind::RateLimited {
                            retry_after_secs: 60,
                        },
                    });
                }
            }
        }

        // 3. Check per-user rate limit
        if let Some(ref user_store) = self.user_store {
            if let Some(user_id) = Self::get_user_id(ctx) {
                if !user_store.allow(&user_id) {
                    return Err(PluginError::Rejected {
                        name: self.name().to_string(),
                        reason: "per-user rate limit exceeded".to_string(),
                        kind: himadri_plugin::RejectKind::RateLimited {
                            retry_after_secs: 60,
                        },
                    });
                }
            }
        }

        // 4. Check per-IP rate limit
        if let Some(ref ip_store) = self.ip_store {
            if let Some(ref ip) = ctx.remote_ip {
                // Validate IP format to prevent key injection via malformed strings
                if ip.parse::<std::net::IpAddr>().is_ok() && !ip_store.allow(ip) {
                    return Err(PluginError::Rejected {
                        name: self.name().to_string(),
                        reason: "per-IP rate limit exceeded".to_string(),
                        kind: himadri_plugin::RejectKind::RateLimited {
                            retry_after_secs: 60,
                        },
                    });
                }
            }
        }

        Ok(())
    }
}

impl RateLimitPlugin {
    /// Resolve the identity per-key limits are tracked under. Prefers the
    /// stable `key_id`/`user_id` from the auth context; never the raw
    /// bearer secret (`AuthContext::api_key`), which must not be spread
    /// into long-lived limiter stores (CWE-522).
    fn get_api_key(ctx: &PluginContext) -> Option<String> {
        if let Some(key) = ctx.get_metadata("api_key") {
            if let Some(s) = key.as_str() {
                if !s.is_empty() {
                    return Some(s.to_string());
                }
            }
        }
        ctx.auth
            .as_ref()
            .and_then(|a| a.key_id.clone().or_else(|| a.user_id.clone()))
    }

    fn get_user_id(ctx: &PluginContext) -> Option<String> {
        ctx.get_metadata("user_id")
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .or_else(|| ctx.auth.as_ref().and_then(|a| a.key_id.clone()))
    }
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use himadri_core::{ChatCompletionRequest, Message, MessageContent, Role};

    fn make_request_ctx(api_key: Option<&str>) -> PluginContext {
        let mut ctx = PluginContext::from_request(
            &ChatCompletionRequest {
                model: "test".to_string(),
                messages: vec![Message {
                    role: Role::User,
                    content: Some(MessageContent::Text("Hello".to_string())),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                }],
                stream: false,
                temperature: None,
                top_p: None,
                max_tokens: None,
                stop: None,
                presence_penalty: None,
                frequency_penalty: None,
                user: None,
                tools: None,
                tool_choice: None,
                extra: Default::default(),
            },
            None,
        );
        if let Some(key) = api_key {
            ctx.set_metadata(
                "api_key".to_string(),
                serde_json::Value::String(key.to_string()),
            );
        }
        ctx
    }

    #[test]
    fn test_sliding_window_allows_within_limit() {
        let counter = FixedWindowCounter::new(10, Duration::from_secs(1));
        for _ in 0..10 {
            assert!(counter.allow());
        }
        assert!(!counter.allow());
    }

    #[test]
    fn test_sliding_window_resets_after_window() {
        let counter = FixedWindowCounter::new(5, Duration::from_millis(100));
        for _ in 0..5 {
            assert!(counter.allow());
        }
        assert!(!counter.allow());

        // Wait for window to expire
        std::thread::sleep(Duration::from_millis(150));
        assert!(counter.allow());
    }

    #[test]
    fn test_sharded_store_allows_within_rate() {
        let store = RateLimiterStore::new(10, Duration::from_secs(1), 1000, 4);
        for _ in 0..10 {
            assert!(store.allow("user1"));
        }
        assert!(!store.allow("user1"));
    }

    #[test]
    fn test_sharded_store_independent_keys() {
        let store = RateLimiterStore::new(5, Duration::from_secs(1), 1000, 4);

        for _ in 0..5 {
            assert!(store.allow("user1"));
        }
        assert!(!store.allow("user1"));

        // Different key should still work
        assert!(store.allow("user2"));
    }

    #[test]
    fn test_init_defaults() {
        let plugin = RateLimitPlugin::new(RateLimitConfig::default()).unwrap();
        assert!(plugin.key_store.is_none());
        assert!(plugin.user_store.is_none());
    }

    #[tokio::test]
    async fn test_global_rate_limit() {
        let plugin = RateLimitPlugin::new(RateLimitConfig {
            requests_per_second: Some(5),
            ..Default::default()
        })
        .unwrap();

        // Should allow first 5 requests
        for _ in 0..5 {
            let mut ctx = make_request_ctx(None);
            assert!(plugin.execute(&mut ctx).await.is_ok());
        }

        // 6th should be rejected
        let mut ctx = make_request_ctx(None);
        let result = plugin.execute(&mut ctx).await;
        assert!(result.is_err());
        assert!(matches!(result, Err(PluginError::Rejected { .. })));
    }

    #[tokio::test]
    async fn test_per_key_rate_limit() {
        let plugin = RateLimitPlugin::new(RateLimitConfig {
            key_rpm: Some(3), // 3 RPM = 1 per 20 seconds
            store_id: Some("test-key-rl".to_string()),
            ..Default::default()
        })
        .unwrap();

        let api_key = "test-key";

        // First request should pass
        let mut ctx = make_request_ctx(Some(api_key));
        assert!(plugin.execute(&mut ctx).await.is_ok());

        // Second and third should pass (burst allows 3)
        let mut ctx = make_request_ctx(Some(api_key));
        assert!(plugin.execute(&mut ctx).await.is_ok());

        let mut ctx = make_request_ctx(Some(api_key));
        assert!(plugin.execute(&mut ctx).await.is_ok());

        // 4th should be rejected (exceeds 3 RPM)
        let mut ctx = make_request_ctx(Some(api_key));
        let result = plugin.execute(&mut ctx).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_per_key_independent() {
        let plugin = RateLimitPlugin::new(RateLimitConfig {
            key_rpm: Some(2),
            store_id: Some("test-key-independent".to_string()),
            ..Default::default()
        })
        .unwrap();

        // Exhaust key1
        let mut ctx1 = make_request_ctx(Some("key1"));
        assert!(plugin.execute(&mut ctx1).await.is_ok());
        let mut ctx1 = make_request_ctx(Some("key1"));
        assert!(plugin.execute(&mut ctx1).await.is_ok());
        let mut ctx1 = make_request_ctx(Some("key1"));
        assert!(plugin.execute(&mut ctx1).await.is_err()); // Exceeded

        // key2 should still work
        let mut ctx2 = make_request_ctx(Some("key2"));
        assert!(plugin.execute(&mut ctx2).await.is_ok());
    }

    #[tokio::test]
    async fn test_no_api_key_skips_key_limit() {
        let plugin = RateLimitPlugin::new(RateLimitConfig {
            key_rpm: Some(1),
            store_id: Some("test-no-key".to_string()),
            ..Default::default()
        })
        .unwrap();

        // Multiple requests without key should pass (only global limit applies)
        for _ in 0..10 {
            let mut ctx = make_request_ctx(None);
            assert!(plugin.execute(&mut ctx).await.is_ok());
        }
    }

    #[tokio::test]
    async fn test_unlimited_global() {
        let plugin = RateLimitPlugin::new(RateLimitConfig {
            requests_per_second: Some(1_000_000),
            ..Default::default()
        })
        .unwrap();

        // Should allow many requests
        for _ in 0..1000 {
            let mut ctx = make_request_ctx(None);
            assert!(plugin.execute(&mut ctx).await.is_ok());
        }
    }

    fn make_request_ctx_with_ip(api_key: Option<&str>, ip: Option<&str>) -> PluginContext {
        let mut ctx = make_request_ctx(api_key);
        if let Some(ip) = ip {
            ctx.remote_ip = Some(ip.to_string());
        }
        ctx
    }

    #[tokio::test]
    async fn test_per_ip_rate_limit() {
        let plugin = RateLimitPlugin::new(RateLimitConfig {
            ip_rpm: Some(3),
            store_id: Some("test-ip-rl".to_string()),
            ..Default::default()
        })
        .unwrap();

        let ip = "192.168.1.1";

        // First 3 should pass
        for _ in 0..3 {
            let mut ctx = make_request_ctx_with_ip(None, Some(ip));
            assert!(plugin.execute(&mut ctx).await.is_ok());
        }

        // 4th should be rejected
        let mut ctx = make_request_ctx_with_ip(None, Some(ip));
        let result = plugin.execute(&mut ctx).await;
        assert!(result.is_err());
        assert!(
            matches!(result, Err(PluginError::Rejected { ref reason, .. }) if reason.contains("per-IP"))
        );
    }

    #[tokio::test]
    async fn test_per_ip_independent() {
        let plugin = RateLimitPlugin::new(RateLimitConfig {
            ip_rpm: Some(2),
            store_id: Some("test-ip-independent".to_string()),
            ..Default::default()
        })
        .unwrap();

        // Exhaust IP 1
        let mut ctx1 = make_request_ctx_with_ip(None, Some("10.0.0.1"));
        assert!(plugin.execute(&mut ctx1).await.is_ok());
        let mut ctx1 = make_request_ctx_with_ip(None, Some("10.0.0.1"));
        assert!(plugin.execute(&mut ctx1).await.is_ok());
        let mut ctx1 = make_request_ctx_with_ip(None, Some("10.0.0.1"));
        assert!(plugin.execute(&mut ctx1).await.is_err()); // Exceeded

        // IP 2 should still work
        let mut ctx2 = make_request_ctx_with_ip(None, Some("10.0.0.2"));
        assert!(plugin.execute(&mut ctx2).await.is_ok());
    }

    #[tokio::test]
    async fn test_no_ip_skips_ip_limit() {
        let plugin = RateLimitPlugin::new(RateLimitConfig {
            ip_rpm: Some(1),
            store_id: Some("test-no-ip".to_string()),
            requests_per_second: Some(1_000_000),
            ..Default::default()
        })
        .unwrap();

        // Multiple requests without IP should pass (only global limit applies)
        for _ in 0..10 {
            let mut ctx = make_request_ctx_with_ip(None, None);
            assert!(plugin.execute(&mut ctx).await.is_ok());
        }
    }

    #[test]
    fn test_init_ip_store_created() {
        let plugin = RateLimitPlugin::new(RateLimitConfig {
            ip_rpm: Some(100),
            ..Default::default()
        })
        .unwrap();
        assert!(plugin.ip_store.is_some());
    }

    #[test]
    fn test_init_no_ip_store_when_unset() {
        let plugin = RateLimitPlugin::new(RateLimitConfig::default()).unwrap();
        assert!(plugin.ip_store.is_none());
    }
}
