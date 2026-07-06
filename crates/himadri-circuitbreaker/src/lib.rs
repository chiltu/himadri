use async_trait::async_trait;
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

/// In-memory circuit breaker.
///
/// State transitions and probe counters live behind one small mutex: the
/// operations are a handful of integer updates (uncontended lock beats the
/// subtle wipe races the previous multi-atomic design had, where a
/// transition's counter resets could erase a concurrent probe's admission
/// or success).
#[derive(Debug)]
pub struct CircuitBreaker {
    inner: parking_lot::Mutex<BreakerInner>,
    config: CircuitBreakerConfig,
}

#[derive(Debug)]
struct BreakerInner {
    state: u8,
    failure_count: u64,
    /// Probes that actually *succeeded* while half-open — only these close
    /// the circuit; admissions are tracked separately.
    success_count: u64,
    /// Probes admitted while half-open.
    half_open_admitted: u64,
    last_failure_time: u64,
}

impl CircuitBreaker {
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            inner: parking_lot::Mutex::new(BreakerInner {
                state: STATE_CLOSED,
                failure_count: 0,
                success_count: 0,
                half_open_admitted: 0,
                last_failure_time: 0,
            }),
            config,
        }
    }

    pub fn allow(&self) -> bool {
        let mut inner = self.inner.lock();
        match inner.state {
            STATE_CLOSED => true,
            STATE_OPEN => {
                let now = now_millis();
                if now.saturating_sub(inner.last_failure_time)
                    >= self.config.recovery_timeout.as_millis() as u64
                {
                    inner.state = STATE_HALF_OPEN;
                    inner.success_count = 0;
                    inner.half_open_admitted = 1; // this call is the first probe
                    debug!("Circuit breaker transitioning to half-open");
                    true
                } else {
                    false
                }
            }
            STATE_HALF_OPEN if inner.half_open_admitted < self.config.half_open_max_calls => {
                inner.half_open_admitted += 1;
                true
            }
            _ => false,
        }
    }

    pub fn record_success(&self) {
        let mut inner = self.inner.lock();
        match inner.state {
            STATE_CLOSED => {
                inner.failure_count = 0;
            }
            STATE_HALF_OPEN => {
                inner.success_count += 1;
                if inner.success_count >= self.config.half_open_max_calls {
                    inner.state = STATE_CLOSED;
                    inner.failure_count = 0;
                    debug!("Circuit breaker closed after successful probes");
                }
            }
            _ => {}
        }
    }

    pub fn record_failure(&self) {
        let mut inner = self.inner.lock();
        inner.last_failure_time = now_millis();
        match inner.state {
            STATE_CLOSED => {
                inner.failure_count += 1;
                if inner.failure_count >= self.config.failure_threshold {
                    inner.state = STATE_OPEN;
                    warn!(
                        "Circuit breaker opened after {} failures",
                        inner.failure_count
                    );
                }
            }
            STATE_HALF_OPEN => {
                inner.state = STATE_OPEN;
                warn!("Circuit breaker reopened after failure in half-open state");
            }
            _ => {}
        }
    }

    pub fn state(&self) -> CircuitState {
        match self.inner.lock().state {
            STATE_OPEN => CircuitState::Open,
            STATE_HALF_OPEN => CircuitState::HalfOpen,
            _ => CircuitState::Closed,
        }
    }

    pub fn failure_count(&self) -> u64 {
        self.inner.lock().failure_count
    }

    pub fn reset(&self) {
        let mut inner = self.inner.lock();
        inner.state = STATE_CLOSED;
        inner.failure_count = 0;
        inner.success_count = 0;
        inner.half_open_admitted = 0;
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

/// Monotonic milliseconds for the in-memory breaker: recovery timing must
/// not jump when the wall clock steps (NTP).
fn now_millis() -> u64 {
    static EPOCH: once_cell::sync::Lazy<std::time::Instant> =
        once_cell::sync::Lazy::new(std::time::Instant::now);
    EPOCH.elapsed().as_millis() as u64
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

        // Two admitted probes that both succeed close the circuit
        // (half_open_max_calls = 2 *successes* required).
        assert!(cb.allow());
        cb.record_success();
        assert_eq!(cb.state(), CircuitState::HalfOpen);

        assert!(cb.allow());
        cb.record_success();

        assert_eq!(cb.state(), CircuitState::Closed);
    }

    /// Regression: `allow()` used to count *admissions* as successes, so N
    /// probes could close the circuit even if none of them succeeded.
    /// Admissions alone must never close the breaker.
    #[test]
    fn test_admissions_without_successes_do_not_close() {
        let config = CircuitBreakerConfig {
            failure_threshold: 1,
            recovery_timeout: Duration::from_millis(1),
            half_open_max_calls: 2,
        };
        let cb = CircuitBreaker::new(config);

        cb.record_failure();
        std::thread::sleep(Duration::from_millis(10));

        // Admit probes without ever recording a success.
        for _ in 0..10 {
            cb.allow();
        }
        assert_eq!(cb.state(), CircuitState::HalfOpen);

        // And a failed probe reopens the circuit.
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);
    }

    /// Once half-open capacity is consumed, further probes are rejected
    /// until the outcome of the in-flight probes is known.
    #[test]
    fn test_half_open_caps_admissions() {
        let config = CircuitBreakerConfig {
            failure_threshold: 1,
            recovery_timeout: Duration::from_millis(1),
            half_open_max_calls: 2,
        };
        let cb = CircuitBreaker::new(config);

        cb.record_failure();
        std::thread::sleep(Duration::from_millis(10));

        assert!(cb.allow()); // transition probe (1st admission)
        assert!(cb.allow()); // 2nd admission
        assert!(!cb.allow()); // over capacity
    }
}
