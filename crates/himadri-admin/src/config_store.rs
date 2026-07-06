use himadri_core::Config;

/// A recorded config version, surfaced by `/admin/config/history` and used
/// for rollback. Backend-agnostic (config history is kept in-memory by the
/// gateway), so it compiles in every build regardless of the SQL backend.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ConfigHistoryEntry {
    pub version: u32,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub config: Config,
    pub rolled_back_from: Option<u32>,
}
