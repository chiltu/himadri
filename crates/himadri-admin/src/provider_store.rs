use sqlx::SqlitePool;

use crate::models::{
    CreateModelRequest, CreateProviderRequest, Model, Provider, UpdateModelRequest,
    UpdateProviderRequest,
};

/// SQLite-backed provider store
pub struct ProviderStore {
    pool: SqlitePool,
}

impl ProviderStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn create(&self, request: CreateProviderRequest) -> Result<Provider, sqlx::Error> {
        let provider = Provider::new(request);
        sqlx::query(
            "INSERT INTO providers (id, name, enabled, api_key, base_url, weight, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&provider.id)
        .bind(&provider.name)
        .bind(provider.enabled)
        .bind(&provider.api_key)
        .bind(&provider.base_url)
        .bind(provider.weight)
        .bind(provider.created_at.to_rfc3339())
        .bind(provider.updated_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(provider)
    }

    pub async fn get(&self, id: &str) -> Result<Option<Provider>, sqlx::Error> {
        let row = sqlx::query_as::<_, ProviderRow>(
            "SELECT id, name, enabled, api_key, base_url, weight, created_at, updated_at FROM providers WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.into()))
    }

    pub async fn list(&self) -> Result<Vec<Provider>, sqlx::Error> {
        let rows = sqlx::query_as::<_, ProviderRow>(
            "SELECT id, name, enabled, api_key, base_url, weight, created_at, updated_at FROM providers ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| r.into()).collect())
    }

    pub async fn list_enabled(&self) -> Result<Vec<Provider>, sqlx::Error> {
        let rows = sqlx::query_as::<_, ProviderRow>(
            "SELECT id, name, enabled, api_key, base_url, weight, created_at, updated_at FROM providers WHERE enabled = 1 ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| r.into()).collect())
    }

    pub async fn update(
        &self,
        id: &str,
        request: UpdateProviderRequest,
    ) -> Result<Option<Provider>, sqlx::Error> {
        let current = self.get(id).await?.ok_or(sqlx::Error::RowNotFound)?;

        let name = request.name.unwrap_or(current.name);
        let enabled = request.enabled.unwrap_or(current.enabled);
        let api_key = request.api_key.unwrap_or(current.api_key);
        let base_url = request.base_url.unwrap_or(current.base_url);
        let weight = request.weight.unwrap_or(current.weight);

        sqlx::query(
            "UPDATE providers SET name = ?, enabled = ?, api_key = ?, base_url = ?, weight = ?, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(&name)
        .bind(enabled)
        .bind(&api_key)
        .bind(&base_url)
        .bind(weight)
        .bind(id)
        .execute(&self.pool)
        .await?;

        self.get(id).await
    }

    pub async fn delete(&self, id: &str) -> Result<bool, sqlx::Error> {
        // Check if provider exists
        let provider = self.get(id).await?;
        let provider = match provider {
            Some(p) => p,
            None => return Ok(false),
        };

        // Check if provider has any models
        let model_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM models WHERE provider_id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await?;

        if model_count.0 > 0 {
            return Err(sqlx::Error::Protocol(format!(
                "Cannot delete provider '{}' (id: {}): provider has {} model(s). Delete or reassign all models first.",
                provider.name, provider.id, model_count.0
            )));
        }

        let r = sqlx::query("DELETE FROM providers WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn toggle(&self, id: &str, enabled: bool) -> Result<Option<Provider>, sqlx::Error> {
        // If disabling, check for enabled models
        if !enabled {
            let enabled_models: (i64,) = sqlx::query_as(
                "SELECT COUNT(*) FROM models WHERE provider_id = ? AND enabled = 1",
            )
            .bind(id)
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

        sqlx::query("UPDATE providers SET enabled = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(enabled)
            .bind(id)
            .execute(&self.pool)
            .await?;
        self.get(id).await
    }
}

/// SQLite-backed model store
pub struct ModelStore {
    pool: SqlitePool,
}

impl ModelStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn create(&self, request: CreateModelRequest) -> Result<Model, sqlx::Error> {
        // Validate provider exists
        let provider_row = sqlx::query_as::<_, ProviderRow>(
            "SELECT id, name, enabled, api_key, base_url, weight, created_at, updated_at FROM providers WHERE id = ?",
        )
        .bind(&request.provider_id)
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

        // Validate provider is enabled
        if provider_row.enabled == 0 {
            return Err(sqlx::Error::Protocol(format!(
                "Provider '{}' is disabled",
                provider_row.name
            )));
        }

        let model = Model::new(request);
        sqlx::query(
            "INSERT INTO models (id, name, provider_id, display_name, enabled, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&model.id)
        .bind(&model.name)
        .bind(&model.provider_id)
        .bind(&model.display_name)
        .bind(model.enabled)
        .bind(model.created_at.to_rfc3339())
        .bind(model.updated_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(model)
    }

    pub async fn get(&self, id: &str) -> Result<Option<Model>, sqlx::Error> {
        let row = sqlx::query_as::<_, ModelRow>(
            "SELECT id, name, provider_id, display_name, enabled, created_at, updated_at FROM models WHERE id = ?",
        )
        .bind(id)
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
        let rows = sqlx::query_as::<_, ModelRow>(
            "SELECT id, name, provider_id, display_name, enabled, created_at, updated_at FROM models WHERE provider_id = ? ORDER BY name",
        )
        .bind(provider_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| r.into()).collect())
    }

    pub async fn list_enabled(&self) -> Result<Vec<Model>, sqlx::Error> {
        let rows = sqlx::query_as::<_, ModelRow>(
            "SELECT id, name, provider_id, display_name, enabled, created_at, updated_at FROM models WHERE enabled = 1 ORDER BY name",
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

        let name = request.name.unwrap_or(current.name);
        let provider_id = request.provider_id.unwrap_or(current.provider_id.clone());
        let display_name = request.display_name.unwrap_or(current.display_name);
        let enabled = request.enabled.unwrap_or(current.enabled);

        // Validate provider exists and is enabled if changing provider
        if provider_id != current.provider_id {
            let provider_row = sqlx::query_as::<_, ProviderRow>(
                "SELECT id, name, enabled, api_key, base_url, weight, created_at, updated_at FROM providers WHERE id = ?",
            )
            .bind(&provider_id)
            .fetch_optional(&self.pool)
            .await?;

            match provider_row {
                Some(row) if row.enabled == 0 => {
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

        sqlx::query(
            "UPDATE models SET name = ?, provider_id = ?, display_name = ?, enabled = ?, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(&name)
        .bind(&provider_id)
        .bind(&display_name)
        .bind(enabled)
        .bind(id)
        .execute(&self.pool)
        .await?;

        self.get(id).await
    }

    pub async fn delete(&self, id: &str) -> Result<bool, sqlx::Error> {
        // Check if model exists and is enabled (active deployment)
        let model = self.get(id).await?;
        let model = match model {
            Some(m) => m,
            None => return Ok(false),
        };

        if model.enabled {
            return Err(sqlx::Error::Protocol(format!(
                "Cannot delete model '{}' (id: {}): model is enabled. Disable it first before deletion.",
                model.name, model.id
            )));
        }

        let r = sqlx::query("DELETE FROM models WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn toggle(&self, id: &str, enabled: bool) -> Result<Option<Model>, sqlx::Error> {
        sqlx::query("UPDATE models SET enabled = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(enabled)
            .bind(id)
            .execute(&self.pool)
            .await?;
        self.get(id).await
    }
}

// Row types for database mapping

#[derive(Debug, sqlx::FromRow)]
struct ProviderRow {
    id: String,
    name: String,
    enabled: i32,
    api_key: Option<String>,
    base_url: Option<String>,
    weight: f64,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, sqlx::FromRow)]
struct ModelRow {
    id: String,
    name: String,
    provider_id: String,
    display_name: Option<String>,
    enabled: i32,
    created_at: String,
    updated_at: String,
}

impl From<ProviderRow> for Provider {
    fn from(r: ProviderRow) -> Self {
        Provider {
            id: r.id,
            name: r.name,
            enabled: r.enabled != 0,
            api_key: r.api_key,
            base_url: r.base_url,
            weight: r.weight,
            created_at: chrono::DateTime::parse_from_rfc3339(&r.created_at)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_default(),
            updated_at: chrono::DateTime::parse_from_rfc3339(&r.updated_at)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_default(),
        }
    }
}

impl From<ModelRow> for Model {
    fn from(r: ModelRow) -> Self {
        Model {
            id: r.id,
            name: r.name,
            provider_id: r.provider_id,
            display_name: r.display_name,
            enabled: r.enabled != 0,
            created_at: chrono::DateTime::parse_from_rfc3339(&r.created_at)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_default(),
            updated_at: chrono::DateTime::parse_from_rfc3339(&r.updated_at)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_default(),
        }
    }
}
