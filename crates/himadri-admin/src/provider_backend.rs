//! Backend-agnostic wrapper over the provider/model stores, mirroring
//! [`crate::store::StoreBackend`]'s pattern for API keys. `connect` picks
//! SQLite or Postgres based on `DATABASE_URL`'s scheme so that, unlike the
//! previous SQLite-only `ProviderStore`, provider/model admin CRUD actually
//! works when the gateway is deployed against Postgres.

use std::sync::Arc;

use crate::crypto::CipherKey;
use crate::models::{
    CreateModelRequest, CreateProviderRequest, Model, Provider, UpdateModelRequest,
    UpdateProviderRequest,
};

#[cfg(feature = "postgres")]
use crate::postgres_provider_store::{PgModelStore, PgProviderStore};
#[cfg(feature = "sqlite")]
use crate::provider_store::{ModelStore, ProviderStore};

#[derive(Clone)]
pub enum ProviderStoreBackend {
    #[cfg(feature = "sqlite")]
    Sqlite(Arc<ProviderStore>),
    #[cfg(feature = "postgres")]
    Postgres(Arc<PgProviderStore>),
}

#[derive(Clone)]
pub enum ModelStoreBackend {
    #[cfg(feature = "sqlite")]
    Sqlite(Arc<ModelStore>),
    #[cfg(feature = "postgres")]
    Postgres(Arc<PgModelStore>),
}

impl ProviderStoreBackend {
    pub async fn create(&self, request: CreateProviderRequest) -> Result<Provider, String> {
        match self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite(s) => s.create(request).await.map_err(|e| e.to_string()),
            #[cfg(feature = "postgres")]
            Self::Postgres(s) => s.create(request).await.map_err(|e| e.to_string()),
        }
    }

    pub async fn get(&self, id: &str) -> Result<Option<Provider>, String> {
        match self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite(s) => s.get(id).await.map_err(|e| e.to_string()),
            #[cfg(feature = "postgres")]
            Self::Postgres(s) => s.get(id).await.map_err(|e| e.to_string()),
        }
    }

    pub async fn list(&self) -> Result<Vec<Provider>, String> {
        match self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite(s) => s.list().await.map_err(|e| e.to_string()),
            #[cfg(feature = "postgres")]
            Self::Postgres(s) => s.list().await.map_err(|e| e.to_string()),
        }
    }

    pub async fn list_enabled(&self) -> Result<Vec<Provider>, String> {
        match self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite(s) => s.list_enabled().await.map_err(|e| e.to_string()),
            #[cfg(feature = "postgres")]
            Self::Postgres(s) => s.list_enabled().await.map_err(|e| e.to_string()),
        }
    }

    pub async fn update(
        &self,
        id: &str,
        request: UpdateProviderRequest,
    ) -> Result<Option<Provider>, String> {
        match self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite(s) => s.update(id, request).await.map_err(|e| e.to_string()),
            #[cfg(feature = "postgres")]
            Self::Postgres(s) => s.update(id, request).await.map_err(|e| e.to_string()),
        }
    }

    pub async fn delete(&self, id: &str) -> Result<bool, String> {
        match self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite(s) => s.delete(id).await.map_err(|e| e.to_string()),
            #[cfg(feature = "postgres")]
            Self::Postgres(s) => s.delete(id).await.map_err(|e| e.to_string()),
        }
    }

    pub async fn toggle(&self, id: &str, enabled: bool) -> Result<Option<Provider>, String> {
        match self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite(s) => s.toggle(id, enabled).await.map_err(|e| e.to_string()),
            #[cfg(feature = "postgres")]
            Self::Postgres(s) => s.toggle(id, enabled).await.map_err(|e| e.to_string()),
        }
    }
}

impl ModelStoreBackend {
    pub async fn create(&self, request: CreateModelRequest) -> Result<Model, String> {
        match self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite(s) => s.create(request).await.map_err(|e| e.to_string()),
            #[cfg(feature = "postgres")]
            Self::Postgres(s) => s.create(request).await.map_err(|e| e.to_string()),
        }
    }

    pub async fn get(&self, id: &str) -> Result<Option<Model>, String> {
        match self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite(s) => s.get(id).await.map_err(|e| e.to_string()),
            #[cfg(feature = "postgres")]
            Self::Postgres(s) => s.get(id).await.map_err(|e| e.to_string()),
        }
    }

    pub async fn list(&self) -> Result<Vec<Model>, String> {
        match self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite(s) => s.list().await.map_err(|e| e.to_string()),
            #[cfg(feature = "postgres")]
            Self::Postgres(s) => s.list().await.map_err(|e| e.to_string()),
        }
    }

    pub async fn list_by_provider(&self, provider_id: &str) -> Result<Vec<Model>, String> {
        match self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite(s) => s
                .list_by_provider(provider_id)
                .await
                .map_err(|e| e.to_string()),
            #[cfg(feature = "postgres")]
            Self::Postgres(s) => s
                .list_by_provider(provider_id)
                .await
                .map_err(|e| e.to_string()),
        }
    }

    pub async fn list_enabled(&self) -> Result<Vec<Model>, String> {
        match self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite(s) => s.list_enabled().await.map_err(|e| e.to_string()),
            #[cfg(feature = "postgres")]
            Self::Postgres(s) => s.list_enabled().await.map_err(|e| e.to_string()),
        }
    }

    pub async fn update(
        &self,
        id: &str,
        request: UpdateModelRequest,
    ) -> Result<Option<Model>, String> {
        match self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite(s) => s.update(id, request).await.map_err(|e| e.to_string()),
            #[cfg(feature = "postgres")]
            Self::Postgres(s) => s.update(id, request).await.map_err(|e| e.to_string()),
        }
    }

    pub async fn delete(&self, id: &str) -> Result<bool, String> {
        match self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite(s) => s.delete(id).await.map_err(|e| e.to_string()),
            #[cfg(feature = "postgres")]
            Self::Postgres(s) => s.delete(id).await.map_err(|e| e.to_string()),
        }
    }

    pub async fn toggle(&self, id: &str, enabled: bool) -> Result<Option<Model>, String> {
        match self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite(s) => s.toggle(id, enabled).await.map_err(|e| e.to_string()),
            #[cfg(feature = "postgres")]
            Self::Postgres(s) => s.toggle(id, enabled).await.map_err(|e| e.to_string()),
        }
    }
}

/// Connects the provider/model stores based on `DATABASE_URL`'s scheme.
/// Returns `None` if `DATABASE_URL` is unset, the scheme is unsupported, or
/// connecting fails (falling back to the in-memory-only behavior the caller
/// already has for providers/models: none configured).
pub async fn connect_provider_model_stores(
    database_url: &str,
    #[allow(unused_variables)] cipher: Option<CipherKey>,
) -> Option<(ProviderStoreBackend, ModelStoreBackend)> {
    #[cfg(feature = "postgres")]
    if database_url.starts_with("postgres") {
        return match sqlx::PgPool::connect(database_url).await {
            Ok(pool) => {
                if let Err(e) = sqlx::migrate!("migrations/postgres").run(&pool).await {
                    tracing::error!("Failed to run Postgres migrations: {e}");
                    return None;
                }
                let provider_store = PgProviderStore::new(pool.clone(), cipher);
                let model_store = PgModelStore::new(pool);
                Some((
                    ProviderStoreBackend::Postgres(Arc::new(provider_store)),
                    ModelStoreBackend::Postgres(Arc::new(model_store)),
                ))
            }
            Err(e) => {
                tracing::warn!("Failed to connect provider/model stores to Postgres: {e}");
                None
            }
        };
    }

    #[cfg(feature = "sqlite")]
    if database_url.starts_with("sqlite") {
        return match sqlx::SqlitePool::connect(&format!("{database_url}?mode=rwc")).await {
            Ok(pool) => {
                if let Err(e) = sqlx::migrate!("migrations/sqlite").run(&pool).await {
                    tracing::error!("Failed to run SQLite migrations: {e}");
                    return None;
                }
                let provider_store = ProviderStore::new(pool.clone(), cipher);
                let model_store = ModelStore::new(pool);
                Some((
                    ProviderStoreBackend::Sqlite(Arc::new(provider_store)),
                    ModelStoreBackend::Sqlite(Arc::new(model_store)),
                ))
            }
            Err(e) => {
                tracing::warn!("Failed to connect provider/model stores to SQLite: {e}");
                None
            }
        };
    }

    #[allow(unreachable_code)]
    None
}
