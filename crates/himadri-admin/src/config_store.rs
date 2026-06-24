use himadri_core::Config;
use parking_lot::RwLock;
use std::sync::Arc;

/// Config history entry
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ConfigHistoryEntry {
    pub version: u32,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub config: Config,
    pub rolled_back_from: Option<u32>,
}

/// Config store trait for persistence
pub trait ConfigStore: Send + Sync {
    fn save(&self, config: &Config) -> Result<(), String>;
    fn load(&self) -> Result<Option<Config>, String>;
    fn delete(&self) -> Result<(), String>;
}

/// In-memory config store
pub struct InMemoryConfigStore {
    config: RwLock<Option<Config>>,
}

impl InMemoryConfigStore {
    pub fn new() -> Self {
        Self {
            config: RwLock::new(None),
        }
    }
}

impl ConfigStore for InMemoryConfigStore {
    fn save(&self, config: &Config) -> Result<(), String> {
        let mut cfg = self.config.write();
        *cfg = Some(config.clone());
        Ok(())
    }

    fn load(&self) -> Result<Option<Config>, String> {
        let cfg = self.config.read();
        Ok(cfg.clone())
    }

    fn delete(&self) -> Result<(), String> {
        let mut cfg = self.config.write();
        *cfg = None;
        Ok(())
    }
}

/// Gateway config manager with history tracking
pub struct GatewayConfigManager {
    gateway_config: Arc<RwLock<Config>>,
    history: Arc<RwLock<Vec<ConfigHistoryEntry>>>,
    version: Arc<RwLock<u32>>,
}

impl GatewayConfigManager {
    pub fn new(initial_config: Config) -> Self {
        let history = vec![ConfigHistoryEntry {
            version: 1,
            updated_at: chrono::Utc::now(),
            config: initial_config.clone(),
            rolled_back_from: None,
        }];

        Self {
            gateway_config: Arc::new(RwLock::new(initial_config)),
            history: Arc::new(RwLock::new(history)),
            version: Arc::new(RwLock::new(1)),
        }
    }

    /// Get current config
    pub fn get_config(&self) -> Config {
        self.gateway_config.read().clone()
    }

    /// Reload config with validation
    pub fn reload_config(&self, config: Config) -> Result<(), String> {
        // Validate config
        config
            .validate()
            .map_err(|e| format!("Validation failed: {}", e))?;

        // Apply config
        {
            let mut cfg = self.gateway_config.write();
            *cfg = config.clone();
        }

        // Record history
        {
            let mut version = self.version.write();
            *version += 1;
            let mut history = self.history.write();
            history.push(ConfigHistoryEntry {
                version: *version,
                updated_at: chrono::Utc::now(),
                config,
                rolled_back_from: None,
            });
        }

        Ok(())
    }

    /// Get config history
    pub fn get_history(&self) -> Vec<ConfigHistoryEntry> {
        self.history.read().clone()
    }

    /// Rollback to a specific version
    pub fn rollback(&self, version: u32) -> Result<(), String> {
        let history = self.history.read();
        let entry = history
            .iter()
            .find(|e| e.version == version)
            .ok_or_else(|| format!("Version {} not found", version))?;

        let config = entry.config.clone();
        drop(history);

        self.reload_config(config)?;

        // Mark as rolled back
        let mut history = self.history.write();
        if let Some(last) = history.last_mut() {
            last.rolled_back_from = Some(version);
        }

        Ok(())
    }

    /// Reset config to initial state
    pub fn reset_config(&self) -> Result<(), String> {
        let history = self.history.read();
        let initial = history
            .first()
            .map(|e| e.config.clone())
            .ok_or("No initial config")?;
        drop(history);

        {
            let mut cfg = self.gateway_config.write();
            *cfg = initial;
        }

        Ok(())
    }
}

impl Default for InMemoryConfigStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_manager_get_config() {
        let config = Config::default();
        let manager = GatewayConfigManager::new(config.clone());
        let got = manager.get_config();
        assert_eq!(got.strategy.mode, config.strategy.mode);
    }

    #[test]
    fn test_config_manager_reload() {
        let config = Config::default();
        let manager = GatewayConfigManager::new(config);

        let mut new_config = Config::default();
        new_config.strategy.fallback_timeout_ms = 5000;

        manager.reload_config(new_config).unwrap();
        let got = manager.get_config();
        assert_eq!(got.strategy.fallback_timeout_ms, 5000);
    }

    #[test]
    fn test_config_manager_history() {
        let config = Config::default();
        let manager = GatewayConfigManager::new(config);

        let mut new_config = Config::default();
        new_config.strategy.fallback_timeout_ms = 1000;
        manager.reload_config(new_config).unwrap();

        let history = manager.get_history();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].version, 1);
        assert_eq!(history[1].version, 2);
    }

    #[test]
    fn test_config_manager_rollback() {
        let config = Config::default();
        let manager = GatewayConfigManager::new(config);

        let mut new_config = Config::default();
        new_config.strategy.fallback_timeout_ms = 5000;
        manager.reload_config(new_config).unwrap();

        manager.rollback(1).unwrap();
        let got = manager.get_config();
        assert_eq!(got.strategy.fallback_timeout_ms, 30000); // default
    }

    #[test]
    fn test_config_manager_rollback_not_found() {
        let config = Config::default();
        let manager = GatewayConfigManager::new(config);

        let result = manager.rollback(999);
        assert!(result.is_err());
    }

    #[test]
    fn test_config_manager_reset() {
        let config = Config::default();
        let manager = GatewayConfigManager::new(config);

        let mut new_config = Config::default();
        new_config.strategy.fallback_timeout_ms = 5000;
        manager.reload_config(new_config).unwrap();

        manager.reset_config().unwrap();
        let got = manager.get_config();
        assert_eq!(got.strategy.fallback_timeout_ms, 30000); // default
    }

    #[test]
    fn test_config_store_save_load() {
        let store = InMemoryConfigStore::new();
        let config = Config::default();
        store.save(&config).unwrap();
        let loaded = store.load().unwrap();
        assert!(loaded.is_some());
    }

    #[test]
    fn test_config_store_delete() {
        let store = InMemoryConfigStore::new();
        let config = Config::default();
        store.save(&config).unwrap();
        store.delete().unwrap();
        let loaded = store.load().unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn test_config_history_versioning() {
        let config = Config::default();
        let manager = GatewayConfigManager::new(config);

        for i in 0..5 {
            let mut new_config = Config::default();
            new_config.strategy.fallback_timeout_ms = (i + 1) * 1000;
            manager.reload_config(new_config).unwrap();
        }

        let history = manager.get_history();
        assert_eq!(history.len(), 6); // 1 initial + 5 updates
        assert_eq!(history.last().unwrap().version, 6);
    }
}
