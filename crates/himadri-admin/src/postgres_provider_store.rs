//! Postgres-backed equivalent of [`crate::provider_store::ProviderStore`] /
//! [`crate::provider_store::ModelStore`]. Kept as a mirror-image
//! implementation (same method set, same encryption-at-rest behavior) so
//! `ProviderStoreBackend`/`ModelStoreBackend` in [`crate::provider_backend`]
//! can dispatch to whichever backend `DATABASE_URL` selects.

use uuid::Uuid;

use crate::crypto::CipherKey;
use crate::models::{
    CreateModelRequest, CreateProviderRequest, Model, Provider, UpdateModelRequest,
    UpdateProviderRequest,
};

pub struct PgProviderStore {
    pool: sqlx::PgPool,
    cipher: Option<CipherKey>,
}

impl PgProviderStore {
    pub fn new(pool: sqlx::PgPool, cipher: Option<CipherKey>) -> Self {
        Self { pool, cipher }
    }

    fn encrypt_api_key(&self, api_key: Option<String>) -> Option<String> {
        match (&self.cipher, api_key) {
            (Some(cipher), Some(plaintext)) if !plaintext.is_empty() => {
                Some(cipher.encrypt(&plaintext))
            }
            (_, other) => other,
        }
    }

    fn decrypt_provider(&self, mut provider: Provider) -> Provider {
        if let (Some(cipher), Some(value)) = (&self.cipher, &provider.api_key) {
            match cipher.decrypt(value) {
                Ok(plaintext) => provider.api_key = Some(plaintext),
                Err(e) => {
                    tracing::error!(provider = %provider.name, "failed to decrypt provider api_key: {e}");
                }
            }
        }
        provider
    }

    pub async fn create(
        &self,
        mut request: CreateProviderRequest,
    ) -> Result<Provider, sqlx::Error> {
        let plaintext_key = request.api_key.clone();
        request.api_key = self.encrypt_api_key(request.api_key);
        let id = Uuid::new_v4();
        let now = chrono::Utc::now();

        sqlx::query(
            "INSERT INTO providers (id, name, enabled, api_key, base_url, weight, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $7)",
        )
        .bind(id)
        .bind(&request.name)
        .bind(request.enabled)
        .bind(&request.api_key)
        .bind(&request.base_url)
        .bind(request.weight)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(Provider {
            id: id.to_string(),
            name: request.name,
            enabled: request.enabled,
            api_key: plaintext_key,
            base_url: request.base_url,
            weight: request.weight,
            created_at: now,
            updated_at: now,
        })
    }

    pub async fn get(&self, id: &str) -> Result<Option<Provider>, sqlx::Error> {
        let uuid = Uuid::parse_str(id).map_err(|_| sqlx::Error::RowNotFound)?;
        let row = sqlx::query_as::<_, ProviderRow>(
            "SELECT id, name, enabled, api_key, base_url, weight, created_at, updated_at FROM providers WHERE id = $1",
        )
        .bind(uuid)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| self.decrypt_provider(r.into())))
    }

    pub async fn list(&self) -> Result<Vec<Provider>, sqlx::Error> {
        let rows = sqlx::query_as::<_, ProviderRow>(
            "SELECT id, name, enabled, api_key, base_url, weight, created_at, updated_at FROM providers ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| self.decrypt_provider(r.into()))
            .collect())
    }

    pub async fn list_enabled(&self) -> Result<Vec<Provider>, sqlx::Error> {
        let rows = sqlx::query_as::<_, ProviderRow>(
            "SELECT id, name, enabled, api_key, base_url, weight, created_at, updated_at FROM providers WHERE enabled = true ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| self.decrypt_provider(r.into()))
            .collect())
    }

    pub async fn update(
        &self,
        id: &str,
        request: UpdateProviderRequest,
    ) -> Result<Option<Provider>, sqlx::Error> {
        let current = self.get(id).await?.ok_or(sqlx::Error::RowNotFound)?;
        let uuid = Uuid::parse_str(id).map_err(|_| sqlx::Error::RowNotFound)?;

        let name = request.name.unwrap_or(current.name);
        let enabled = request.enabled.unwrap_or(current.enabled);
        let api_key = request.api_key.unwrap_or(current.api_key);
        let base_url = request.base_url.unwrap_or(current.base_url);
        let weight = request.weight.unwrap_or(current.weight);
        let stored_api_key = self.encrypt_api_key(api_key);

        sqlx::query(
            "UPDATE providers SET name = $1, enabled = $2, api_key = $3, base_url = $4, weight = $5, updated_at = NOW() WHERE id = $6",
        )
        .bind(&name)
        .bind(enabled)
        .bind(&stored_api_key)
        .bind(&base_url)
        .bind(weight)
        .bind(uuid)
        .execute(&self.pool)
        .await?;

        self.get(id).await
    }

    pub async fn delete(&self, id: &str) -> Result<bool, sqlx::Error> {
        let uuid = Uuid::parse_str(id).map_err(|_| sqlx::Error::RowNotFound)?;
        let provider = match self.get(id).await? {
            Some(p) => p,
            None => return Ok(false),
        };

        let model_count: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM models WHERE provider_id = $1")
                .bind(uuid)
                .fetch_one(&self.pool)
                .await?;

        if model_count.0 > 0 {
            return Err(sqlx::Error::Protocol(format!(
                "Cannot delete provider '{}' (id: {}): provider has {} model(s). Delete or reassign all models first.",
                provider.name, provider.id, model_count.0
            )));
        }

        let r = sqlx::query("DELETE FROM providers WHERE id = $1")
            .bind(uuid)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn toggle(&self, id: &str, enabled: bool) -> Result<Option<Provider>, sqlx::Error> {
        let uuid = Uuid::parse_str(id).map_err(|_| sqlx::Error::RowNotFound)?;

        if !enabled {
            let enabled_models: (i64,) = sqlx::query_as(
                "SELECT COUNT(*) FROM models WHERE provider_id = $1 AND enabled = true",
            )
            .bind(uuid)
            .fetch_one(&self.pool)
            .await?;

            if enabled_models.0 > 0 {
                let provider = self.get(id).await?.ok_or(sqlx::Error::RowNotFound)?;
                return Err(sqlx::Error::Protocol(format!(
                    "Cannot disable provider '{}': provider has {} enabled model(s). Disable all models first.",
                    provider.name, enabled_models.0
                )));
            }
        }

        sqlx::query("UPDATE providers SET enabled = $1, updated_at = NOW() WHERE id = $2")
            .bind(enabled)
            .bind(uuid)
            .execute(&self.pool)
            .await?;
        self.get(id).await
    }
}

pub struct PgModelStore {
    pool: sqlx::PgPool,
}

impl PgModelStore {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }

    pub async fn create(&self, request: CreateModelRequest) -> Result<Model, sqlx::Error> {
        let provider_uuid =
            Uuid::parse_str(&request.provider_id).map_err(|_| sqlx::Error::RowNotFound)?;

        let provider_row = sqlx::query_as::<_, ProviderRow>(
            "SELECT id, name, enabled, api_key, base_url, weight, created_at, updated_at FROM providers WHERE id = $1",
        )
        .bind(provider_uuid)
        .fetch_optional(&self.pool)
        .await?;

        let provider_row = match provider_row {
            Some(row) => row,
            None => {
                return Err(sqlx::Error::Protocol(format!(
                    "Provider with id '{}' does not exist",
                    request.provider_id
                )));
            }
        };

        if !provider_row.enabled {
            return Err(sqlx::Error::Protocol(format!(
                "Provider '{}' is disabled",
                provider_row.name
            )));
        }

        let id = Uuid::new_v4();
        let now = chrono::Utc::now();

        sqlx::query(
            "INSERT INTO models (id, name, provider_id, display_name, enabled, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $6)",
        )
        .bind(id)
        .bind(&request.name)
        .bind(provider_uuid)
        .bind(&request.display_name)
        .bind(request.enabled)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(Model {
            id: id.to_string(),
            name: request.name,
            provider_id: request.provider_id,
            display_name: request.display_name,
            enabled: request.enabled,
            created_at: now,
            updated_at: now,
        })
    }

    pub async fn get(&self, id: &str) -> Result<Option<Model>, sqlx::Error> {
        let uuid = Uuid::parse_str(id).map_err(|_| sqlx::Error::RowNotFound)?;
        let row = sqlx::query_as::<_, ModelRow>(
            "SELECT id, name, provider_id, display_name, enabled, created_at, updated_at FROM models WHERE id = $1",
        )
        .bind(uuid)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.into()))
    }

    pub async fn list(&self) -> Result<Vec<Model>, sqlx::Error> {
        let rows = sqlx::query_as::<_, ModelRow>(
            "SELECT id, name, provider_id, display_name, enabled, created_at, updated_at FROM models ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| r.into()).collect())
    }

    pub async fn list_by_provider(&self, provider_id: &str) -> Result<Vec<Model>, sqlx::Error> {
        let provider_uuid = Uuid::parse_str(provider_id).map_err(|_| sqlx::Error::RowNotFound)?;
        let rows = sqlx::query_as::<_, ModelRow>(
            "SELECT id, name, provider_id, display_name, enabled, created_at, updated_at FROM models WHERE provider_id = $1 ORDER BY name",
        )
        .bind(provider_uuid)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| r.into()).collect())
    }

    pub async fn list_enabled(&self) -> Result<Vec<Model>, sqlx::Error> {
        let rows = sqlx::query_as::<_, ModelRow>(
            "SELECT id, name, provider_id, display_name, enabled, created_at, updated_at FROM models WHERE enabled = true ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| r.into()).collect())
    }

    pub async fn update(
        &self,
        id: &str,
        request: UpdateModelRequest,
    ) -> Result<Option<Model>, sqlx::Error> {
        let current = self.get(id).await?.ok_or(sqlx::Error::RowNotFound)?;
        let uuid = Uuid::parse_str(id).map_err(|_| sqlx::Error::RowNotFound)?;

        let name = request.name.unwrap_or(current.name);
        let provider_id = request.provider_id.unwrap_or(current.provider_id.clone());
        let display_name = request.display_name.unwrap_or(current.display_name);
        let enabled = request.enabled.unwrap_or(current.enabled);

        if provider_id != current.provider_id {
            let provider_uuid =
                Uuid::parse_str(&provider_id).map_err(|_| sqlx::Error::RowNotFound)?;
            let provider_row = sqlx::query_as::<_, ProviderRow>(
                "SELECT id, name, enabled, api_key, base_url, weight, created_at, updated_at FROM providers WHERE id = $1",
            )
            .bind(provider_uuid)
            .fetch_optional(&self.pool)
            .await?;

            match provider_row {
                Some(row) if !row.enabled => {
                    return Err(sqlx::Error::Protocol(format!(
                        "Provider '{}' is disabled",
                        row.name
                    )));
                }
                None => {
                    return Err(sqlx::Error::Protocol(format!(
                        "Provider with id '{}' does not exist",
                        provider_id
                    )));
                }
                _ => {}
            }
        }

        let provider_uuid = Uuid::parse_str(&provider_id).map_err(|_| sqlx::Error::RowNotFound)?;

        sqlx::query(
            "UPDATE models SET name = $1, provider_id = $2, display_name = $3, enabled = $4, updated_at = NOW() WHERE id = $5",
        )
        .bind(&name)
        .bind(provider_uuid)
        .bind(&display_name)
        .bind(enabled)
        .bind(uuid)
        .execute(&self.pool)
        .await?;

        self.get(id).await
    }

    pub async fn delete(&self, id: &str) -> Result<bool, sqlx::Error> {
        let uuid = Uuid::parse_str(id).map_err(|_| sqlx::Error::RowNotFound)?;
        let model = match self.get(id).await? {
            Some(m) => m,
            None => return Ok(false),
        };

        if model.enabled {
            return Err(sqlx::Error::Protocol(format!(
                "Cannot delete model '{}' (id: {}): model is enabled. Disable it first before deletion.",
                model.name, model.id
            )));
        }

        let r = sqlx::query("DELETE FROM models WHERE id = $1")
            .bind(uuid)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn toggle(&self, id: &str, enabled: bool) -> Result<Option<Model>, sqlx::Error> {
        let uuid = Uuid::parse_str(id).map_err(|_| sqlx::Error::RowNotFound)?;
        sqlx::query("UPDATE models SET enabled = $1, updated_at = NOW() WHERE id = $2")
            .bind(enabled)
            .bind(uuid)
            .execute(&self.pool)
            .await?;
        self.get(id).await
    }
}

#[derive(Debug, sqlx::FromRow)]
struct ProviderRow {
    id: Uuid,
    name: String,
    enabled: bool,
    api_key: Option<String>,
    base_url: Option<String>,
    weight: f64,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, sqlx::FromRow)]
struct ModelRow {
    id: Uuid,
    name: String,
    provider_id: Uuid,
    display_name: Option<String>,
    enabled: bool,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<ProviderRow> for Provider {
    fn from(r: ProviderRow) -> Self {
        Provider {
            id: r.id.to_string(),
            name: r.name,
            enabled: r.enabled,
            api_key: r.api_key,
            base_url: r.base_url,
            weight: r.weight,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

impl From<ModelRow> for Model {
    fn from(r: ModelRow) -> Self {
        Model {
            id: r.id.to_string(),
            name: r.name,
            provider_id: r.provider_id.to_string(),
            display_name: r.display_name,
            enabled: r.enabled,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}
