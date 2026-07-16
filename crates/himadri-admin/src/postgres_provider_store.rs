//! Postgres-backed equivalent of [`crate::provider_store::ModelStore`] /
//! [`crate::provider_store::ModelEndpointStore`]. Implements the
//! [`ModelStore`](crate::model_store::ModelStore) and
//! [`ModelEndpointStore`](crate::model_store::ModelEndpointStore) traits
//! so callers dispatch generically through the trait seam instead of
//! matching on a backend enum.

use async_trait::async_trait;
use uuid::Uuid;

use crate::crypto::CipherKey;
use crate::error::AdminError;
use crate::model_store::{
    ModelEndpointStore as ModelEndpointStoreTrait, ModelStore as ModelStoreTrait,
};
use crate::models::{
    CreateModelEndpointRequest, CreateModelRequest, Model, ModelEndpoint,
    UpdateModelEndpointRequest, UpdateModelRequest,
};

pub struct PgModelStore {
    pool: sqlx::PgPool,
}

impl PgModelStore {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ModelStoreTrait for PgModelStore {
    async fn create(&self, request: CreateModelRequest) -> Result<Model, AdminError> {
        let id = Uuid::new_v4();
        let now = chrono::Utc::now();

        sqlx::query(
            "INSERT INTO models (id, name, display_name, enabled, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $5)",
        )
        .bind(id)
        .bind(&request.name)
        .bind(&request.display_name)
        .bind(request.enabled)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(Model {
            id: id.to_string(),
            name: request.name,
            display_name: request.display_name,
            enabled: request.enabled,
            created_at: now,
            updated_at: now,
        })
    }

    async fn get(&self, id: &str) -> Result<Option<Model>, AdminError> {
        let Ok(uuid) = Uuid::parse_str(id) else {
            return Ok(None);
        };
        let row = sqlx::query_as::<_, ModelRow>(
            "SELECT id, name, display_name, enabled, created_at, updated_at FROM models WHERE id = $1",
        )
        .bind(uuid)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.into()))
    }

    async fn list(&self) -> Result<Vec<Model>, AdminError> {
        let rows = sqlx::query_as::<_, ModelRow>(
            "SELECT id, name, display_name, enabled, created_at, updated_at FROM models ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| r.into()).collect())
    }

    async fn list_enabled(&self) -> Result<Vec<Model>, AdminError> {
        let rows = sqlx::query_as::<_, ModelRow>(
            "SELECT id, name, display_name, enabled, created_at, updated_at FROM models WHERE enabled = true ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| r.into()).collect())
    }

    async fn update(
        &self,
        id: &str,
        request: UpdateModelRequest,
    ) -> Result<Option<Model>, AdminError> {
        let current = self.get(id).await?.ok_or(sqlx::Error::RowNotFound)?;
        let uuid = Uuid::parse_str(id).map_err(|_| sqlx::Error::RowNotFound)?;

        let name = request.name.unwrap_or(current.name);
        let display_name = request.display_name.unwrap_or(current.display_name);
        let enabled = request.enabled.unwrap_or(current.enabled);

        sqlx::query(
            "UPDATE models SET name = $1, display_name = $2, enabled = $3, updated_at = NOW() WHERE id = $4",
        )
        .bind(&name)
        .bind(&display_name)
        .bind(enabled)
        .bind(uuid)
        .execute(&self.pool)
        .await?;

        self.get(id).await
    }

    async fn delete(&self, id: &str) -> Result<bool, AdminError> {
        let Ok(uuid) = Uuid::parse_str(id) else {
            return Ok(false);
        };
        let model = match self.get(id).await? {
            Some(m) => m,
            None => return Ok(false),
        };

        if model.enabled {
            return Err(AdminError::Conflict(format!(
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

    async fn toggle(&self, id: &str, enabled: bool) -> Result<Option<Model>, AdminError> {
        let Ok(uuid) = Uuid::parse_str(id) else {
            return Ok(None);
        };
        sqlx::query("UPDATE models SET enabled = $1, updated_at = NOW() WHERE id = $2")
            .bind(enabled)
            .bind(uuid)
            .execute(&self.pool)
            .await?;
        self.get(id).await
    }
}

/// Postgres-backed store for model endpoints (a model's provider routes),
/// mirroring [`crate::provider_store::ModelEndpointStore`] with the same
/// encryption-at-rest behavior.
pub struct PgModelEndpointStore {
    pool: sqlx::PgPool,
    cipher: Option<CipherKey>,
}

impl PgModelEndpointStore {
    pub fn new(pool: sqlx::PgPool, cipher: Option<CipherKey>) -> Self {
        Self { pool, cipher }
    }

    fn encrypt_api_key(&self, api_key: Option<String>) -> Option<String> {
        crate::crypto::encrypt_endpoint_api_key(self.cipher.as_ref(), api_key)
    }

    fn decrypt_endpoint(&self, endpoint: ModelEndpoint) -> ModelEndpoint {
        crate::crypto::decrypt_endpoint(self.cipher.as_ref(), endpoint)
    }
}

#[async_trait]
impl ModelEndpointStoreTrait for PgModelEndpointStore {
    async fn create(
        &self,
        model_id: &str,
        mut request: CreateModelEndpointRequest,
    ) -> Result<ModelEndpoint, AdminError> {
        let Ok(model_uuid) = Uuid::parse_str(model_id) else {
            return Err(AdminError::NotFound);
        };
        let model_exists: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM models WHERE id = $1")
            .bind(model_uuid)
            .fetch_optional(&self.pool)
            .await?;
        if model_exists.is_none() {
            return Err(AdminError::NotFound);
        }

        let plaintext_key = request.api_key.clone();
        request.api_key = self.encrypt_api_key(request.api_key);
        let id = Uuid::new_v4();
        let now = chrono::Utc::now();

        sqlx::query(
            "INSERT INTO model_endpoints (id, model_id, provider_type, base_url, api_key, weight, enabled, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8)",
        )
        .bind(id)
        .bind(model_uuid)
        .bind(&request.provider_type)
        .bind(&request.base_url)
        .bind(&request.api_key)
        .bind(request.weight)
        .bind(request.enabled)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(ModelEndpoint {
            id: id.to_string(),
            model_id: model_id.to_string(),
            provider_type: request.provider_type,
            base_url: request.base_url,
            api_key: plaintext_key,
            weight: request.weight,
            enabled: request.enabled,
            created_at: now,
            updated_at: now,
        })
    }

    async fn get(&self, id: &str) -> Result<Option<ModelEndpoint>, AdminError> {
        let Ok(uuid) = Uuid::parse_str(id) else {
            return Ok(None);
        };
        let row = sqlx::query_as::<_, ModelEndpointRow>(
            "SELECT id, model_id, provider_type, base_url, api_key, weight, enabled, created_at, updated_at FROM model_endpoints WHERE id = $1",
        )
        .bind(uuid)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| self.decrypt_endpoint(r.into())))
    }

    async fn list(&self) -> Result<Vec<ModelEndpoint>, AdminError> {
        let rows = sqlx::query_as::<_, ModelEndpointRow>(
            "SELECT id, model_id, provider_type, base_url, api_key, weight, enabled, created_at, updated_at FROM model_endpoints ORDER BY created_at",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| self.decrypt_endpoint(r.into()))
            .collect())
    }

    async fn list_by_model(&self, model_id: &str) -> Result<Vec<ModelEndpoint>, AdminError> {
        let Ok(model_uuid) = Uuid::parse_str(model_id) else {
            return Ok(vec![]);
        };
        let rows = sqlx::query_as::<_, ModelEndpointRow>(
            "SELECT id, model_id, provider_type, base_url, api_key, weight, enabled, created_at, updated_at FROM model_endpoints WHERE model_id = $1 ORDER BY created_at",
        )
        .bind(model_uuid)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| self.decrypt_endpoint(r.into()))
            .collect())
    }

    async fn update(
        &self,
        id: &str,
        request: UpdateModelEndpointRequest,
    ) -> Result<Option<ModelEndpoint>, AdminError> {
        let current = self.get(id).await?.ok_or(sqlx::Error::RowNotFound)?;
        let uuid = Uuid::parse_str(id).map_err(|_| sqlx::Error::RowNotFound)?;

        let (provider_type, base_url) = request.effective_routing_pair(&current);
        let weight = request.weight.unwrap_or(current.weight);
        let enabled = request.enabled.unwrap_or(current.enabled);

        match request.api_key {
            Some(new_key) => {
                let stored_api_key = self.encrypt_api_key(new_key);
                sqlx::query(
                    "UPDATE model_endpoints SET provider_type = $1, base_url = $2, api_key = $3, weight = $4, enabled = $5, updated_at = NOW() WHERE id = $6",
                )
                .bind(&provider_type)
                .bind(&base_url)
                .bind(&stored_api_key)
                .bind(weight)
                .bind(enabled)
                .bind(uuid)
                .execute(&self.pool)
                .await?;
            }
            None => {
                sqlx::query(
                    "UPDATE model_endpoints SET provider_type = $1, base_url = $2, weight = $3, enabled = $4, updated_at = NOW() WHERE id = $5",
                )
                .bind(&provider_type)
                .bind(&base_url)
                .bind(weight)
                .bind(enabled)
                .bind(uuid)
                .execute(&self.pool)
                .await?;
            }
        }

        self.get(id).await
    }

    async fn delete(&self, id: &str) -> Result<bool, AdminError> {
        let Ok(uuid) = Uuid::parse_str(id) else {
            return Ok(false);
        };
        let r = sqlx::query("DELETE FROM model_endpoints WHERE id = $1")
            .bind(uuid)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    async fn toggle(&self, id: &str, enabled: bool) -> Result<Option<ModelEndpoint>, AdminError> {
        let Ok(uuid) = Uuid::parse_str(id) else {
            return Ok(None);
        };
        sqlx::query("UPDATE model_endpoints SET enabled = $1, updated_at = NOW() WHERE id = $2")
            .bind(enabled)
            .bind(uuid)
            .execute(&self.pool)
            .await?;
        self.get(id).await
    }
}

#[derive(Debug, sqlx::FromRow)]
struct ModelRow {
    id: Uuid,
    name: String,
    display_name: Option<String>,
    enabled: bool,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, sqlx::FromRow)]
struct ModelEndpointRow {
    id: Uuid,
    model_id: Uuid,
    provider_type: String,
    base_url: Option<String>,
    api_key: Option<String>,
    weight: f64,
    enabled: bool,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<ModelRow> for Model {
    fn from(r: ModelRow) -> Self {
        Model {
            id: r.id.to_string(),
            name: r.name,
            display_name: r.display_name,
            enabled: r.enabled,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

impl From<ModelEndpointRow> for ModelEndpoint {
    fn from(r: ModelEndpointRow) -> Self {
        ModelEndpoint {
            id: r.id.to_string(),
            model_id: r.model_id.to_string(),
            provider_type: r.provider_type,
            base_url: r.base_url,
            api_key: r.api_key,
            weight: r.weight,
            enabled: r.enabled,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}
