//! Config apply/reload/rollback and the in-memory version history behind
//! `/admin/config/history`.

use himadri_core::{Config, GatewayError};

use crate::strategy::Strategy;

use super::Gateway;

/// In-memory record of applied config versions, enabling `/admin/config/history`
/// and rollback. Backend-agnostic so it works in every build.
#[derive(Default)]
pub(super) struct ConfigHistory {
    pub(super) entries: Vec<himadri_admin::ConfigHistoryEntry>,
    pub(super) next_version: u32,
}

impl ConfigHistory {
    pub(super) fn record(&mut self, config: Config, rolled_back_from: Option<u32>) {
        let version = self.next_version.max(1);
        self.next_version = version + 1;
        self.entries.push(himadri_admin::ConfigHistoryEntry {
            version,
            updated_at: chrono::Utc::now(),
            config,
            rolled_back_from,
        });
    }
}

impl Gateway {
    /// Validate and apply a config to the live gateway (strategy, targets,
    /// limiter/circuit-breaker state) without touching version history.
    async fn apply_config(&self, config: Config) -> Result<(), GatewayError> {
        config.validate()?;
        // Hold all 3 write locks simultaneously to prevent inconsistent reads.
        // Lock order: strategy → config → targets (see the docs on `Gateway`).
        let mut strategy = self.strategy.write().await;
        let mut cfg = self.config.write().await;
        let mut targets = self.targets.write().await;
        *strategy = Strategy::from_strategy_config(&config.strategy);
        *cfg = config.clone();
        *targets = config.targets;
        drop(strategy);
        drop(cfg);
        drop(targets);
        // Clear stale rate limiter and circuit breaker state
        self.rate_limiter.clear();
        self.circuit_breakers.clear();
        Ok(())
    }

    pub async fn reload_config(&self, config: Config) -> Result<(), GatewayError> {
        self.apply_config(config.clone()).await?;
        self.config_history.write().await.record(config, None);
        Ok(())
    }

    /// Return the recorded config versions, newest first.
    pub async fn config_history(&self) -> Vec<himadri_admin::ConfigHistoryEntry> {
        let mut entries = self.config_history.read().await.entries.clone();
        entries.reverse();
        entries
    }

    /// Roll back to a previously recorded config version. The restored config is
    /// applied and recorded as a new version tagged with the version it was
    /// rolled back from.
    pub async fn rollback_config(&self, version: u32) -> Result<(), GatewayError> {
        let target = self
            .config_history
            .read()
            .await
            .entries
            .iter()
            .find(|e| e.version == version)
            .map(|e| e.config.clone())
            .ok_or_else(|| {
                GatewayError::BadRequest(format!("Config version {} not found", version))
            })?;

        self.apply_config(target.clone()).await?;
        self.config_history
            .write()
            .await
            .record(target, Some(version));
        Ok(())
    }

    pub async fn get_config(&self) -> Config {
        self.config.read().await.clone()
    }
}

#[cfg(test)]
mod config_history_tests {
    use super::*;
    use himadri_core::Config;
    use himadri_observability::Metrics;
    use std::sync::Arc;

    fn gateway() -> Gateway {
        Gateway::new(Config::default(), Arc::new(Metrics::new()))
    }

    #[tokio::test]
    async fn history_seeded_with_initial_version() {
        let gw = gateway();
        let history = gw.config_history().await;
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].version, 1);
        assert!(history[0].rolled_back_from.is_none());
    }

    #[tokio::test]
    async fn reload_appends_a_version() {
        let gw = gateway();
        let mut cfg = Config::default();
        cfg.strategy.fallback_timeout_ms = 12345;
        gw.reload_config(cfg).await.unwrap();

        let history = gw.config_history().await;
        assert_eq!(history.len(), 2);
        // Newest first.
        assert_eq!(history[0].version, 2);
        assert_eq!(history[0].config.strategy.fallback_timeout_ms, 12345);
    }

    #[tokio::test]
    async fn rollback_restores_and_records_new_version() {
        let gw = gateway();

        // v2 with a distinctive value.
        let mut cfg = Config::default();
        cfg.strategy.fallback_timeout_ms = 999;
        gw.reload_config(cfg).await.unwrap();
        assert_eq!(gw.get_config().await.strategy.fallback_timeout_ms, 999);

        // Roll back to v1 (default timeout 30000).
        gw.rollback_config(1).await.unwrap();
        assert_eq!(gw.get_config().await.strategy.fallback_timeout_ms, 30000);

        let history = gw.config_history().await;
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].version, 3);
        assert_eq!(history[0].rolled_back_from, Some(1));
    }

    #[tokio::test]
    async fn rollback_unknown_version_errors() {
        let gw = gateway();
        assert!(gw.rollback_config(999).await.is_err());
    }
}
