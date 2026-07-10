//! Backend-agnostic wrapper over the model/endpoint stores, mirroring
//! [`crate::store::StoreBackend`]'s pattern for API keys. `connect` picks
//! SQLite or Postgres based on `DATABASE_URL`'s scheme so that model/endpoint
//! admin CRUD works against either backend.

use std::sync::Arc;

use crate::crypto::CipherKey;
use crate::error::AdminError;
use crate::models::{
    CreateModelEndpointRequest, CreateModelRequest, Model, ModelEndpoint,
    UpdateModelEndpointRequest, UpdateModelRequest,
};

#[cfg(feature = "postgres")]
use crate::postgres_provider_store::{PgModelEndpointStore, PgModelStore};
#[cfg(feature = "sqlite")]
use crate::provider_store::{ModelEndpointStore, ModelStore};

/// Dispatch a method to the active SQL backend, converting the store's error
/// into [`AdminError`]. Shared by both store backends; adding a store method
/// is one line instead of a two-armed `match`.
macro_rules! pm_dispatch {
    ($self:expr, $method:ident ( $($arg:expr),* )) => {
        match $self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite(s) => s.$method($($arg),*).await.map_err(AdminError::from),
            #[cfg(feature = "postgres")]
            Self::Postgres(s) => s.$method($($arg),*).await.map_err(AdminError::from),
        }
    };
}

#[derive(Clone)]
pub enum ModelStoreBackend {
    #[cfg(feature = "sqlite")]
    Sqlite(Arc<ModelStore>),
    #[cfg(feature = "postgres")]
    Postgres(Arc<PgModelStore>),
}

#[derive(Clone)]
pub enum ModelEndpointStoreBackend {
    #[cfg(feature = "sqlite")]
    Sqlite(Arc<ModelEndpointStore>),
    #[cfg(feature = "postgres")]
    Postgres(Arc<PgModelEndpointStore>),
}

impl ModelStoreBackend {
    pub async fn create(&self, request: CreateModelRequest) -> Result<Model, AdminError> {
        pm_dispatch!(self, create(request))
    }

    pub async fn get(&self, id: &str) -> Result<Option<Model>, AdminError> {
        pm_dispatch!(self, get(id))
    }

    pub async fn list(&self) -> Result<Vec<Model>, AdminError> {
        pm_dispatch!(self, list())
    }

    pub async fn list_enabled(&self) -> Result<Vec<Model>, AdminError> {
        pm_dispatch!(self, list_enabled())
    }

    pub async fn update(
        &self,
        id: &str,
        request: UpdateModelRequest,
    ) -> Result<Option<Model>, AdminError> {
        pm_dispatch!(self, update(id, request))
    }

    /// Concrete model stores already return [`AdminError`] (the delete guard
    /// distinguishes `Conflict` from store failures), so no mapping here.
    pub async fn delete(&self, id: &str) -> Result<bool, AdminError> {
        match self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite(s) => s.delete(id).await,
            #[cfg(feature = "postgres")]
            Self::Postgres(s) => s.delete(id).await,
        }
    }

    pub async fn toggle(&self, id: &str, enabled: bool) -> Result<Option<Model>, AdminError> {
        pm_dispatch!(self, toggle(id, enabled))
    }
}

impl ModelEndpointStoreBackend {
    /// Concrete endpoint stores already return [`AdminError`] (a missing or
    /// malformed parent model id is `NotFound`), so no mapping here.
    pub async fn create(
        &self,
        model_id: &str,
        request: CreateModelEndpointRequest,
    ) -> Result<ModelEndpoint, AdminError> {
        match self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite(s) => s.create(model_id, request).await,
            #[cfg(feature = "postgres")]
            Self::Postgres(s) => s.create(model_id, request).await,
        }
    }

    pub async fn get(&self, id: &str) -> Result<Option<ModelEndpoint>, AdminError> {
        pm_dispatch!(self, get(id))
    }

    pub async fn list(&self) -> Result<Vec<ModelEndpoint>, AdminError> {
        pm_dispatch!(self, list())
    }

    pub async fn list_by_model(&self, model_id: &str) -> Result<Vec<ModelEndpoint>, AdminError> {
        pm_dispatch!(self, list_by_model(model_id))
    }

    pub async fn update(
        &self,
        id: &str,
        request: UpdateModelEndpointRequest,
    ) -> Result<Option<ModelEndpoint>, AdminError> {
        pm_dispatch!(self, update(id, request))
    }

    pub async fn delete(&self, id: &str) -> Result<bool, AdminError> {
        pm_dispatch!(self, delete(id))
    }

    pub async fn toggle(
        &self,
        id: &str,
        enabled: bool,
    ) -> Result<Option<ModelEndpoint>, AdminError> {
        pm_dispatch!(self, toggle(id, enabled))
    }
}

/// Connects the model/endpoint stores based on `DATABASE_URL`'s scheme.
/// Returns `None` if `DATABASE_URL` is unset, the scheme is unsupported, or
/// connecting fails (falling back to the in-memory-only behavior the caller
/// already has: no models configured).
pub async fn connect_model_stores(
    database_url: &str,
    #[allow(unused_variables)] cipher: Option<CipherKey>,
) -> Option<(ModelStoreBackend, ModelEndpointStoreBackend)> {
    #[cfg(feature = "postgres")]
    if database_url.starts_with("postgres") {
        return match sqlx::PgPool::connect(database_url).await {
            Ok(pool) => {
                if let Err(e) = sqlx::migrate!("migrations/postgres").run(&pool).await {
                    tracing::error!("Failed to run Postgres migrations: {e}");
                    return None;
                }
                let model_store = PgModelStore::new(pool.clone());
                let endpoint_store = PgModelEndpointStore::new(pool, cipher);
                Some((
                    ModelStoreBackend::Postgres(Arc::new(model_store)),
                    ModelEndpointStoreBackend::Postgres(Arc::new(endpoint_store)),
                ))
            }
            Err(e) => {
                tracing::warn!("Failed to connect model stores to Postgres: {e}");
                None
            }
        };
    }

    #[cfg(feature = "sqlite")]
    if database_url.starts_with("sqlite") {
        // Create the database file if missing; don't clobber an existing
        // query string (e.g. `sqlite://file.db?mode=ro`).
        let url = if database_url.contains('?') {
            database_url.to_string()
        } else {
            format!("{database_url}?mode=rwc")
        };
        return match sqlx::SqlitePool::connect(&url).await {
            Ok(pool) => {
                if let Err(e) = sqlx::migrate!("migrations/sqlite").run(&pool).await {
                    tracing::error!("Failed to run SQLite migrations: {e}");
                    return None;
                }
                let model_store = ModelStore::new(pool.clone());
                let endpoint_store = ModelEndpointStore::new(pool, cipher);
                Some((
                    ModelStoreBackend::Sqlite(Arc::new(model_store)),
                    ModelEndpointStoreBackend::Sqlite(Arc::new(endpoint_store)),
                ))
            }
            Err(e) => {
                tracing::warn!("Failed to connect model stores to SQLite: {e}");
                None
            }
        };
    }

    #[allow(unreachable_code)]
    None
}

/// Migrates the database at `database_url` to the latest schema version using
/// the migrations embedded in this binary. Unlike the connect paths above
/// (which log and fall back on failure), this returns the error so callers
/// can fail hard — intended for an explicit pre-startup migration step.
pub async fn migrate_to_latest(database_url: &str) -> Result<(), String> {
    #[cfg(feature = "postgres")]
    if database_url.starts_with("postgres") {
        let pool = sqlx::PgPool::connect(database_url)
            .await
            .map_err(|e| format!("failed to connect to Postgres: {e}"))?;
        return sqlx::migrate!("migrations/postgres")
            .run(&pool)
            .await
            .map_err(|e| format!("Postgres migration failed: {e}"));
    }

    #[cfg(feature = "sqlite")]
    if database_url.starts_with("sqlite") {
        let url = if database_url.contains('?') {
            database_url.to_string()
        } else {
            format!("{database_url}?mode=rwc")
        };
        let pool = sqlx::SqlitePool::connect(&url)
            .await
            .map_err(|e| format!("failed to connect to SQLite: {e}"))?;
        return sqlx::migrate!("migrations/sqlite")
            .run(&pool)
            .await
            .map_err(|e| format!("SQLite migration failed: {e}"));
    }

    #[allow(unreachable_code)]
    Err(format!(
        "unsupported DATABASE_URL scheme (expected sqlite:// or postgres://): {database_url}"
    ))
}
