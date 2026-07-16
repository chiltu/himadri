pub mod config_store;
pub mod crypto;
pub mod error;
pub mod handlers;
pub mod middleware;
pub mod model_store;
pub mod models;
pub mod provider_backend;
pub mod request_log;
pub mod store;
pub mod usage_store;

#[cfg(feature = "sqlite")]
pub mod provider_store;
#[cfg(feature = "sqlite")]
pub(crate) mod sqlite_time;

#[cfg(feature = "postgres")]
pub mod postgres_backends;
#[cfg(feature = "postgres")]
pub mod postgres_provider_store;

pub use config_store::ConfigHistoryEntry;
pub use crypto::CipherKey;
pub use error::AdminError;
pub use handlers::AdminHandlers;
pub use himadri_core::{AuthContext, AuthScope};
pub use middleware::AuthMiddleware;
pub use models::{
    CreateModelEndpointRequest, CreateModelRequest, Model, ModelEndpoint,
    UpdateModelEndpointRequest, UpdateModelRequest,
};
pub use provider_backend::{
    connect_model_stores, migrate_to_latest, ModelEndpointStoreBackend, ModelStoreBackend,
};
#[cfg(feature = "sqlite")]
pub use provider_store::{ModelEndpointStore, ModelStore};
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
pub use postgres_backends::PostgresRequestLogStore;
#[cfg(feature = "postgres")]
pub use postgres_provider_store::{PgModelEndpointStore, PgModelStore};

#[cfg(test)]
mod tests;

#[cfg(all(test, feature = "sqlite"))]
mod store_parity_tests;
