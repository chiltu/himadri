//! Backend-agnostic wrapper over the model/endpoint stores, mirroring
//! [`crate::store::StoreBackend`]'s pattern for API keys. `connect` picks
//! SQLite or Postgres based on `DATABASE_URL`'s scheme so that model/endpoint
//! admin CRUD works against either backend.
//!
//! Unlike the API-key store (which uses an enum + dispatch macro), model
//! stores dispatch through the [`ModelStore`] / [`ModelEndpointStore`] trait
//! seam — any backend that implements the trait is usable without adding enum
//! variants or dispatch arms.

use std::sync::Arc;

use crate::crypto::CipherKey;
use crate::model_store::{ModelEndpointStore, ModelStore};

#[cfg(feature = "postgres")]
use crate::postgres_provider_store::{PgModelEndpointStore, PgModelStore};
#[cfg(feature = "sqlite")]
use crate::provider_store::{
    ModelEndpointStore as SqliteEndpointStore, ModelStore as SqliteModelStore,
};

/// A connected model store backend, type-erased behind the [`ModelStore`]
/// trait.
pub type ModelStoreBackend = Arc<dyn ModelStore>;

/// A connected model-endpoint store backend, type-erased behind the
/// [`ModelEndpointStore`] trait.
pub type ModelEndpointStoreBackend = Arc<dyn ModelEndpointStore>;

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
                let model_store: ModelStoreBackend = Arc::new(PgModelStore::new(pool.clone()));
                let endpoint_store: ModelEndpointStoreBackend =
                    Arc::new(PgModelEndpointStore::new(pool, cipher));
                Some((model_store, endpoint_store))
            }
            Err(e) => {
                tracing::warn!("Failed to connect model stores to Postgres: {e}");
                None
            }
        };
    }

    #[cfg(feature = "sqlite")]
    if database_url.starts_with("sqlite") {
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
                let model_store: ModelStoreBackend = Arc::new(SqliteModelStore::new(pool.clone()));
                let endpoint_store: ModelEndpointStoreBackend =
                    Arc::new(SqliteEndpointStore::new(pool, cipher));
                Some((model_store, endpoint_store))
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
