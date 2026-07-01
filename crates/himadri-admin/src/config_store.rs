use himadri_core::Config;
use sqlx::SqlitePool;

/// Config history entry for database storage
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ConfigHistoryEntry {
    pub version: u32,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub config: Config,
    pub rolled_back_from: Option<u32>,
}

/// Parses a timestamp that may be RFC3339 (the format `save` writes) or
/// SQLite's native `datetime('now')` output (`YYYY-MM-DD HH:MM:SS`, no offset
/// — assumed UTC), which rows written before `save` was fixed to bind an
/// explicit RFC3339 value are still stored as.
fn parse_timestamp(s: &str) -> chrono::DateTime<chrono::Utc> {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return dt.with_timezone(&chrono::Utc);
    }
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .map(|naive| naive.and_utc())
        .unwrap_or_default()
}

/// SQLite-backed config store with history tracking
pub struct SqliteConfigStore {
    pool: SqlitePool,
}

impl SqliteConfigStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Load the latest config from database
    pub async fn load_latest(&self) -> Result<Option<Config>, sqlx::Error> {
        let row = sqlx::query_as::<_, (String,)>(
            "SELECT config FROM config_history ORDER BY version DESC LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.and_then(|(json,)| serde_json::from_str(&json).ok()))
    }

    /// Save a new config version to database
    pub async fn save(&self, version: u32, config: &Config) -> Result<(), sqlx::Error> {
        let json = serde_json::to_string(config).map_err(|e| sqlx::Error::Decode(Box::new(e)))?;
        // Bind an explicit RFC3339 timestamp rather than SQLite's `datetime('now')`,
        // whose `YYYY-MM-DD HH:MM:SS` output isn't RFC3339 and fails to parse in
        // `get_history` below — which used to silently drop the whole row.
        sqlx::query("INSERT INTO config_history (version, config, updated_at) VALUES (?, ?, ?)")
            .bind(version)
            .bind(&json)
            .bind(chrono::Utc::now().to_rfc3339())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Get config history from database
    pub async fn get_history(&self) -> Result<Vec<ConfigHistoryEntry>, sqlx::Error> {
        let rows = sqlx::query_as::<_, (u32, String, String, Option<u32>)>(
            "SELECT version, config, updated_at, rolled_back_from FROM config_history ORDER BY version DESC",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .filter_map(|(version, json, updated_at, rolled_back)| {
                let config: Config = serde_json::from_str(&json).ok()?;
                Some(ConfigHistoryEntry {
                    version,
                    updated_at: parse_timestamp(&updated_at),
                    config,
                    rolled_back_from: rolled_back,
                })
            })
            .collect())
    }

    /// Get a specific config version from database
    pub async fn get_version(&self, version: u32) -> Result<Option<Config>, sqlx::Error> {
        let row =
            sqlx::query_as::<_, (String,)>("SELECT config FROM config_history WHERE version = ?")
                .bind(version)
                .fetch_optional(&self.pool)
                .await?;

        Ok(row.and_then(|(json,)| serde_json::from_str(&json).ok()))
    }

    /// Mark a version as rolled back
    pub async fn mark_rolled_back(&self, version: u32) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE config_history SET rolled_back_from = (SELECT MAX(version) FROM config_history) WHERE version = ?",
        )
        .bind(version)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
