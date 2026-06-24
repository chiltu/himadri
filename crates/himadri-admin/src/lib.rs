pub mod config_store;
pub mod handlers;
pub mod middleware;
pub mod request_log;
pub mod store;
pub mod usage_store;

#[cfg(feature = "postgres")]
pub mod postgres_backends;

pub use config_store::{ConfigHistoryEntry, GatewayConfigManager, InMemoryConfigStore};
pub use handlers::AdminHandlers;
pub use himadri_core::{AuthContext, AuthScope};
pub use middleware::AuthMiddleware;
pub use request_log::{
    InMemoryRequestLogStore, MaintenanceQuery, RequestLogEntry, RequestLogListResult,
    RequestLogQuery, RequestLogStore,
};
#[cfg(feature = "postgres")]
pub use store::PostgresStore;
pub use store::{
    ApiKey, ApiKeyStore, CreateApiKeyRequest, RateLimitOverride, StoreBackend, TokenBudget,
    UpdateApiKeyRequest,
};
pub use usage_store::{
    DashboardSummary, ModelPricing, ModelUsage, ProviderUsage, UsageRecord, UsageStats, UsageStore,
};

#[cfg(feature = "postgres")]
pub use postgres_backends::{PostgresConfigStore, PostgresRequestLogStore, PostgresUsageStore};

#[cfg(test)]
mod tests;
