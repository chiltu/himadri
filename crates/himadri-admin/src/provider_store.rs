use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::crypto::CipherKey;
use crate::error::AdminError;
use crate::model_store::{
    ModelEndpointStore as ModelEndpointStoreTrait, ModelStore as ModelStoreTrait,
};
use crate::models::{
    CreateModelEndpointRequest, CreateModelRequest, Model, ModelEndpoint,
    UpdateModelEndpointRequest, UpdateModelRequest,
};

/// SQLite-backed model store
pub struct ModelStore {
    pool: SqlitePool,
}

impl ModelStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ModelStoreTrait for ModelStore {
    async fn create(&self, request: CreateModelRequest) -> Result<Model, AdminError> {
        let model = Model::new(request);
        sqlx::query(
            "INSERT INTO models (id, name, display_name, enabled, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&model.id)
        .bind(&model.name)
        .bind(&model.display_name)
        .bind(model.enabled)
        .bind(model.created_at.to_rfc3339())
        .bind(model.updated_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(model)
    }

    async fn get(&self, id: &str) -> Result<Option<Model>, AdminError> {
        let row = sqlx::query_as::<_, ModelRow>(
            "SELECT id, name, display_name, enabled, created_at, updated_at FROM models WHERE id = ?",
        )
        .bind(id)
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
            "SELECT id, name, display_name, enabled, created_at, updated_at FROM models WHERE enabled = 1 ORDER BY name",
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

        let name = request.name.unwrap_or(current.name);
        let display_name = request.display_name.unwrap_or(current.display_name);
        let enabled = request.enabled.unwrap_or(current.enabled);

        sqlx::query(
            "UPDATE models SET name = ?, display_name = ?, enabled = ?, updated_at = ? WHERE id = ?",
        )
        .bind(&name)
        .bind(&display_name)
        .bind(enabled)
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(id)
        .execute(&self.pool)
        .await?;

        self.get(id).await
    }

    async fn delete(&self, id: &str) -> Result<bool, AdminError> {
        let model = self.get(id).await?;
        let model = match model {
            Some(m) => m,
            None => return Ok(false),
        };

        if model.enabled {
            return Err(AdminError::Conflict(format!(
                "Cannot delete model '{}' (id: {}): model is enabled. Disable it first before deletion.",
                model.name, model.id
            )));
        }

        sqlx::query("DELETE FROM model_endpoints WHERE model_id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        let r = sqlx::query("DELETE FROM models WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    async fn toggle(&self, id: &str, enabled: bool) -> Result<Option<Model>, AdminError> {
        sqlx::query("UPDATE models SET enabled = ?, updated_at = ? WHERE id = ?")
            .bind(enabled)
            .bind(chrono::Utc::now().to_rfc3339())
            .bind(id)
            .execute(&self.pool)
            .await?;
        self.get(id).await
    }
}

/// SQLite-backed store for model endpoints (a model's provider routes). Mirrors
/// [`ProviderStore`]'s encryption-at-rest for `api_key`.
pub struct ModelEndpointStore {
    pool: SqlitePool,
    cipher: Option<CipherKey>,
}

impl ModelEndpointStore {
    pub fn new(pool: SqlitePool, cipher: Option<CipherKey>) -> Self {
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
impl ModelEndpointStoreTrait for ModelEndpointStore {
    async fn create(
        &self,
        model_id: &str,
        mut request: CreateModelEndpointRequest,
    ) -> Result<ModelEndpoint, AdminError> {
        let model_exists: Option<(String,)> = sqlx::query_as("SELECT id FROM models WHERE id = ?")
            .bind(model_id)
            .fetch_optional(&self.pool)
            .await?;
        if model_exists.is_none() {
            return Err(AdminError::NotFound);
        }

        let plaintext_key = request.api_key.clone();
        request.api_key = self.encrypt_api_key(request.api_key);
        let mut endpoint = ModelEndpoint::new(model_id.to_string(), request);
        sqlx::query(
            "INSERT INTO model_endpoints (id, model_id, provider_type, base_url, api_key, weight, enabled, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&endpoint.id)
        .bind(&endpoint.model_id)
        .bind(&endpoint.provider_type)
        .bind(&endpoint.base_url)
        .bind(&endpoint.api_key)
        .bind(endpoint.weight)
        .bind(endpoint.enabled)
        .bind(endpoint.created_at.to_rfc3339())
        .bind(endpoint.updated_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        endpoint.api_key = plaintext_key;
        Ok(endpoint)
    }

    async fn get(&self, id: &str) -> Result<Option<ModelEndpoint>, AdminError> {
        let row = sqlx::query_as::<_, ModelEndpointRow>(
            "SELECT id, model_id, provider_type, base_url, api_key, weight, enabled, created_at, updated_at FROM model_endpoints WHERE id = ?",
        )
        .bind(id)
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
        let rows = sqlx::query_as::<_, ModelEndpointRow>(
            "SELECT id, model_id, provider_type, base_url, api_key, weight, enabled, created_at, updated_at FROM model_endpoints WHERE model_id = ? ORDER BY created_at",
        )
        .bind(model_id)
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

        let (provider_type, base_url) = request.effective_routing_pair(&current);
        let weight = request.weight.unwrap_or(current.weight);
        let enabled = request.enabled.unwrap_or(current.enabled);
        let now = chrono::Utc::now().to_rfc3339();

        match request.api_key {
            Some(new_key) => {
                let stored_api_key = self.encrypt_api_key(new_key);
                sqlx::query(
                    "UPDATE model_endpoints SET provider_type = ?, base_url = ?, api_key = ?, weight = ?, enabled = ?, updated_at = ? WHERE id = ?",
                )
                .bind(&provider_type)
                .bind(&base_url)
                .bind(&stored_api_key)
                .bind(weight)
                .bind(enabled)
                .bind(&now)
                .bind(id)
                .execute(&self.pool)
                .await?;
            }
            None => {
                sqlx::query(
                    "UPDATE model_endpoints SET provider_type = ?, base_url = ?, weight = ?, enabled = ?, updated_at = ? WHERE id = ?",
                )
                .bind(&provider_type)
                .bind(&base_url)
                .bind(weight)
                .bind(enabled)
                .bind(&now)
                .bind(id)
                .execute(&self.pool)
                .await?;
            }
        }

        self.get(id).await
    }

    async fn delete(&self, id: &str) -> Result<bool, AdminError> {
        let r = sqlx::query("DELETE FROM model_endpoints WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    async fn toggle(&self, id: &str, enabled: bool) -> Result<Option<ModelEndpoint>, AdminError> {
        sqlx::query("UPDATE model_endpoints SET enabled = ?, updated_at = ? WHERE id = ?")
            .bind(enabled)
            .bind(chrono::Utc::now().to_rfc3339())
            .bind(id)
            .execute(&self.pool)
            .await?;
        self.get(id).await
    }
}

// Row types for database mapping

#[derive(Debug, sqlx::FromRow)]
struct ModelRow {
    id: String,
    name: String,
    display_name: Option<String>,
    enabled: i32,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, sqlx::FromRow)]
struct ModelEndpointRow {
    id: String,
    model_id: String,
    provider_type: String,
    base_url: Option<String>,
    api_key: Option<String>,
    weight: f64,
    enabled: i32,
    created_at: String,
    updated_at: String,
}

impl From<ModelRow> for Model {
    fn from(r: ModelRow) -> Self {
        Model {
            id: r.id,
            name: r.name,
            display_name: r.display_name,
            enabled: r.enabled != 0,
            created_at: crate::sqlite_time::parse_or_default(&r.created_at),
            updated_at: crate::sqlite_time::parse_or_default(&r.updated_at),
        }
    }
}

impl From<ModelEndpointRow> for ModelEndpoint {
    fn from(r: ModelEndpointRow) -> Self {
        ModelEndpoint {
            id: r.id,
            model_id: r.model_id,
            provider_type: r.provider_type,
            base_url: r.base_url,
            api_key: r.api_key,
            weight: r.weight,
            enabled: r.enabled != 0,
            created_at: crate::sqlite_time::parse_or_default(&r.created_at),
            updated_at: crate::sqlite_time::parse_or_default(&r.updated_at),
        }
    }
}
