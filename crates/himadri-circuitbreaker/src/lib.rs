use async_trait::async_trait;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::time::Duration;
use tracing::{debug, warn};

const STATE_CLOSED: u8 = 0;
const STATE_OPEN: u8 = 1;
const STATE_HALF_OPEN: u8 = 2;

#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    pub failure_threshold: u64,
    pub recovery_timeout: Duration,
    pub half_open_max_calls: u64,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            recovery_timeout: Duration::from_secs(30),
            half_open_max_calls: 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

impl std::fmt::Display for CircuitState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CircuitState::Closed => write!(f, "closed"),
            CircuitState::Open => write!(f, "open"),
            CircuitState::HalfOpen => write!(f, "half_open"),
        }
    }
}

#[async_trait]
pub trait CircuitBreakerTrait: Send + Sync {
    async fn allow(&self) -> bool;
    async fn record_success(&self);
    async fn record_failure(&self);
    async fn state(&self) -> CircuitState;
    async fn failure_count(&self) -> u64;
    async fn reset(&self);
}

#[derive(Debug)]
pub struct CircuitBreaker {
    state: AtomicU8,
    failure_count: AtomicU64,
    success_count: AtomicU64,
    last_failure_time: AtomicU64,
    config: CircuitBreakerConfig,
}

impl CircuitBreaker {
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            state: AtomicU8::new(STATE_CLOSED),
            failure_count: AtomicU64::new(0),
            success_count: AtomicU64::new(0),
            last_failure_time: AtomicU64::new(0),
            config,
        }
    }

    pub fn allow(&self) -> bool {
        match self.state.load(Ordering::Acquire) {
            STATE_CLOSED => true,
            STATE_OPEN => {
                let now = now_millis();
                let last = self.last_failure_time.load(Ordering::Acquire);

                if now.saturating_sub(last) >= self.config.recovery_timeout.as_millis() as u64 {
                    let _ = self.state.compare_exchange(
                        STATE_OPEN,
                        STATE_HALF_OPEN,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    );
                    self.success_count.store(0, Ordering::Relaxed);
                    debug!("Circuit breaker transitioning to half-open");
                    true
                } else {
                    false
                }
            }
            STATE_HALF_OPEN => {
                let successes = self.success_count.fetch_add(1, Ordering::AcqRel);
                successes < self.config.half_open_max_calls
            }
            _ => false,
        }
    }

    pub fn record_success(&self) {
        match self.state.load(Ordering::Acquire) {
            STATE_CLOSED => {
                self.failure_count.store(0, Ordering::Relaxed);
            }
            STATE_HALF_OPEN => {
                let successes = self.success_count.load(Ordering::Acquire);
                if successes >= self.config.half_open_max_calls {
                    self.state.store(STATE_CLOSED, Ordering::Release);
                    self.failure_count.store(0, Ordering::Relaxed);
                    debug!("Circuit breaker closed after successful probes");
                }
            }
            _ => {}
        }
    }

    pub fn record_failure(&self) {
        self.last_failure_time
            .store(now_millis(), Ordering::Release);

        match self.state.load(Ordering::Acquire) {
            STATE_CLOSED => {
                let failures = self.failure_count.fetch_add(1, Ordering::AcqRel) + 1;
                if failures >= self.config.failure_threshold {
                    self.state.store(STATE_OPEN, Ordering::Release);
                    warn!("Circuit breaker opened after {} failures", failures);
                }
            }
            STATE_HALF_OPEN => {
                self.state.store(STATE_OPEN, Ordering::Release);
                warn!("Circuit breaker reopened after failure in half-open state");
            }
            _ => {}
        }
    }

    pub fn state(&self) -> CircuitState {
        match self.state.load(Ordering::Acquire) {
            STATE_CLOSED => CircuitState::Closed,
            STATE_OPEN => CircuitState::Open,
            STATE_HALF_OPEN => CircuitState::HalfOpen,
            _ => CircuitState::Closed,
        }
    }

    pub fn failure_count(&self) -> u64 {
        self.failure_count.load(Ordering::Relaxed)
    }

    pub fn reset(&self) {
        self.state.store(STATE_CLOSED, Ordering::Release);
        self.failure_count.store(0, Ordering::Relaxed);
        self.success_count.store(0, Ordering::Relaxed);
    }
}

#[async_trait]
impl CircuitBreakerTrait for CircuitBreaker {
    async fn allow(&self) -> bool {
        CircuitBreaker::allow(self)
    }

    async fn record_success(&self) {
        CircuitBreaker::record_success(self)
    }

    async fn record_failure(&self) {
        CircuitBreaker::record_failure(self)
    }

    async fn state(&self) -> CircuitState {
        CircuitBreaker::state(self)
    }

    async fn failure_count(&self) -> u64 {
        CircuitBreaker::failure_count(self)
    }

    async fn reset(&self) {
        CircuitBreaker::reset(self)
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

// ─── Redis-backed Circuit Breaker ────────────────────────────────────

#[cfg(feature = "redis")]
pub struct RedisCircuitBreaker {
    client: redis::Client,
    prefix: String,
    config: CircuitBreakerConfig,
}

#[cfg(feature = "redis")]
impl RedisCircuitBreaker {
    pub async fn new(
        redis_url: &str,
        provider: &str,
        config: CircuitBreakerConfig,
    ) -> Result<Self, redis::RedisError> {
        let client = redis::Client::open(redis_url)?;
        let mut conn = client.get_async_connection().await?;
        redis::cmd("PING").query_async::<_, ()>(&mut conn).await?;
        Ok(Self {
            client,
            prefix: format!("himadri:cb:{}:", provider),
            config,
        })
    }

    fn key(&self, suffix: &str) -> String {
        format!("{}{}", self.prefix, suffix)
    }
}

#[cfg(feature = "redis")]
#[async_trait]
impl CircuitBreakerTrait for RedisCircuitBreaker {
    async fn allow(&self) -> bool {
        let mut conn = match self.client.get_async_connection().await {
            Ok(c) => c,
            Err(_) => return true,
        };

        let state: u8 = redis::cmd("GET")
            .arg(self.key("state"))
            .query_async(&mut conn)
            .await
            .unwrap_or(STATE_CLOSED);

        match state {
            STATE_CLOSED => true,
            STATE_OPEN => {
                let last_failure: u64 = redis::cmd("GET")
                    .arg(self.key("last_failure"))
                    .query_async(&mut conn)
                    .await
                    .unwrap_or(0);

                let now = now_millis();
                if now.saturating_sub(last_failure)
                    >= self.config.recovery_timeout.as_millis() as u64
                {
                    let _: () = redis::cmd("SET")
                        .arg(self.key("state"))
                        .arg(STATE_HALF_OPEN)
                        .query_async::<_, ()>(&mut conn)
                        .await
                        .unwrap_or(());
                    let _: () = redis::cmd("SET")
                        .arg(self.key("success_count"))
                        .arg(0)
                        .query_async::<_, ()>(&mut conn)
                        .await
                        .unwrap_or(());
                    debug!("Redis circuit breaker transitioning to half-open");
                    true
                } else {
                    false
                }
            }
            STATE_HALF_OPEN => {
                let successes: u64 = redis::cmd("INCR")
                    .arg(self.key("success_count"))
                    .query_async(&mut conn)
                    .await
                    .unwrap_or(0);
                successes <= self.config.half_open_max_calls
            }
            _ => false,
        }
    }

    async fn record_success(&self) {
        let mut conn = match self.client.get_async_connection().await {
            Ok(c) => c,
            Err(_) => return,
        };

        let state: u8 = redis::cmd("GET")
            .arg(self.key("state"))
            .query_async(&mut conn)
            .await
            .unwrap_or(STATE_CLOSED);

        match state {
            STATE_CLOSED => {
                let _: () = redis::cmd("SET")
                    .arg(self.key("failure_count"))
                    .arg(0)
                    .query_async::<_, ()>(&mut conn)
                    .await
                    .unwrap_or(());
            }
            STATE_HALF_OPEN => {
                let successes: u64 = redis::cmd("GET")
                    .arg(self.key("success_count"))
                    .query_async(&mut conn)
                    .await
                    .unwrap_or(0);
                if successes >= self.config.half_open_max_calls {
                    let _: () = redis::cmd("SET")
                        .arg(self.key("state"))
                        .arg(STATE_CLOSED)
                        .query_async::<_, ()>(&mut conn)
                        .await
                        .unwrap_or(());
                    let _: () = redis::cmd("SET")
                        .arg(self.key("failure_count"))
                        .arg(0)
                        .query_async::<_, ()>(&mut conn)
                        .await
                        .unwrap_or(());
                    debug!("Redis circuit breaker closed after successful probes");
                }
            }
            _ => {}
        }
    }

    async fn record_failure(&self) {
        let mut conn = match self.client.get_async_connection().await {
            Ok(c) => c,
            Err(_) => return,
        };

        let _: () = redis::cmd("SET")
            .arg(self.key("last_failure"))
            .arg(now_millis())
            .query_async::<_, ()>(&mut conn)
            .await
            .unwrap_or(());

        let state: u8 = redis::cmd("GET")
            .arg(self.key("state"))
            .query_async(&mut conn)
            .await
            .unwrap_or(STATE_CLOSED);

        match state {
            STATE_CLOSED => {
                let failures: u64 = redis::cmd("INCR")
                    .arg(self.key("failure_count"))
                    .query_async(&mut conn)
                    .await
                    .unwrap_or(0);
                if failures >= self.config.failure_threshold {
                    let _: () = redis::cmd("SET")
                        .arg(self.key("state"))
                        .arg(STATE_OPEN)
                        .query_async::<_, ()>(&mut conn)
                        .await
                        .unwrap_or(());
                    warn!("Redis circuit breaker opened after {} failures", failures);
                }
            }
            STATE_HALF_OPEN => {
                let _: () = redis::cmd("SET")
                    .arg(self.key("state"))
                    .arg(STATE_OPEN)
                    .query_async::<_, ()>(&mut conn)
                    .await
                    .unwrap_or(());
                warn!("Redis circuit breaker reopened after failure in half-open state");
            }
            _ => {}
        }
    }

    async fn state(&self) -> CircuitState {
        let mut conn = match self.client.get_async_connection().await {
            Ok(c) => c,
            Err(_) => return CircuitState::Closed,
        };

        let state: u8 = redis::cmd("GET")
            .arg(self.key("state"))
            .query_async(&mut conn)
            .await
            .unwrap_or(STATE_CLOSED);

        match state {
            STATE_CLOSED => CircuitState::Closed,
            STATE_OPEN => CircuitState::Open,
            STATE_HALF_OPEN => CircuitState::HalfOpen,
            _ => CircuitState::Closed,
        }
    }

    async fn failure_count(&self) -> u64 {
        let mut conn = match self.client.get_async_connection().await {
            Ok(c) => c,
            Err(_) => return 0,
        };

        redis::cmd("GET")
            .arg(self.key("failure_count"))
            .query_async(&mut conn)
            .await
            .unwrap_or(0)
    }

    async fn reset(&self) {
        let mut conn = match self.client.get_async_connection().await {
            Ok(c) => c,
            Err(_) => return,
        };

        let _: () = redis::cmd("SET")
            .arg(self.key("state"))
            .arg(STATE_CLOSED)
            .query_async::<_, ()>(&mut conn)
            .await
            .unwrap_or(());
        let _: () = redis::cmd("SET")
            .arg(self.key("failure_count"))
            .arg(0)
            .query_async::<_, ()>(&mut conn)
            .await
            .unwrap_or(());
        let _: () = redis::cmd("SET")
            .arg(self.key("success_count"))
            .arg(0)
            .query_async::<_, ()>(&mut conn)
            .await
            .unwrap_or(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_circuit_breaker_starts_closed() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig::default());
        assert!(cb.allow());
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn test_opens_after_threshold() {
        let config = CircuitBreakerConfig {
            failure_threshold: 3,
            ..Default::default()
        };
        let cb = CircuitBreaker::new(config);

        cb.record_failure();
        assert!(cb.allow());
        assert_eq!(cb.state(), CircuitState::Closed);

        cb.record_failure();
        assert!(cb.allow());
        assert_eq!(cb.state(), CircuitState::Closed);

        cb.record_failure();
        assert!(!cb.allow());
        assert_eq!(cb.state(), CircuitState::Open);
    }

    #[test]
    fn test_half_open_after_timeout() {
        let config = CircuitBreakerConfig {
            failure_threshold: 1,
            recovery_timeout: Duration::from_millis(1),
            ..Default::default()
        };
        let cb = CircuitBreaker::new(config);

        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);

        std::thread::sleep(Duration::from_millis(10));

        assert!(cb.allow());
        assert_eq!(cb.state(), CircuitState::HalfOpen);
    }

    #[test]
    fn test_closes_after_successful_probes() {
        let config = CircuitBreakerConfig {
            failure_threshold: 1,
            recovery_timeout: Duration::from_millis(1),
            half_open_max_calls: 2,
        };
        let cb = CircuitBreaker::new(config);

        cb.record_failure();
        std::thread::sleep(Duration::from_millis(10));

        // First allow() transitions OPEN -> HALF_OPEN, doesn't increment success_count
        cb.allow();
        cb.record_success();

        // Second allow() increments success_count to 1
        cb.allow();
        cb.record_success();

        // Third allow() increments success_count to 2, triggering close
        cb.allow();
        cb.record_success();

        assert_eq!(cb.state(), CircuitState::Closed);
    }
}
