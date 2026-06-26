use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::store::{ApiKey, CreateApiKeyRequest, UpdateApiKeyRequest};

pub struct PostgresStore {
    pool: sqlx::PgPool,
}

impl PostgresStore {
    pub async fn new(database_url: &str) -> Result<Self, sqlx::Error> {
        let pool = sqlx::PgPool::connect(database_url).await?;
        sqlx::migrate!("migrations/postgres")
            .run(&pool)
            .await
            .map_err(|e| sqlx::Error::Migrate(Box::new(e)))?;
        Ok(Self { pool })
    }

    pub async fn from_pool(pool: sqlx::PgPool) -> Result<Self, sqlx::Error> {
        sqlx::migrate!("migrations/postgres")
            .run(&pool)
            .await
            .map_err(|e| sqlx::Error::Migrate(Box::new(e)))?;
        Ok(Self { pool })
    }

    pub async fn create(&self, request: CreateApiKeyRequest) -> Result<ApiKey, sqlx::Error> {
        let id = Uuid::new_v4();
        let key = format!("sk-{}", Uuid::new_v4().to_string().replace('-', ""));
        let now = Utc::now();

        let scopes = serde_json::to_value(&request.scopes).unwrap_or_default();
        let metadata = request.metadata;
        let models = request.models.as_ref().map(|m| serde_json::to_value(m).unwrap_or_default());
        let rate_limit = request.rate_limit_override.as_ref().map(|r| serde_json::to_value(r).unwrap_or_default());
        let budget = request.token_budget.as_ref().map(|b| serde_json::to_value(b).unwrap_or_default());

        let record = sqlx::query_as::<_, ApiKeyRecord>(
            r#"
            INSERT INTO api_keys (id, name, key, scopes, enabled, created_at, expires_at, metadata, org_id, team_id, user_id, models, rate_limit_override, token_budget)
            VALUES ($1, $2, $3, $4, true, $5, $6, $7, $8, $9, $10, $11, $12, $13)
            RETURNING id, name, key, scopes, enabled, created_at, last_used_at, expires_at, usage_count, metadata, org_id, team_id, user_id, models, rate_limit_override, token_budget
            "#,
        )
        .bind(id)
        .bind(&request.name)
        .bind(&key)
        .bind(&scopes)
        .bind(now)
        .bind(request.expires_at)
        .bind(metadata)
        .bind(&request.org_id)
        .bind(&request.team_id)
        .bind(&request.user_id)
        .bind(models)
        .bind(rate_limit)
        .bind(budget)
        .fetch_one(&self.pool)
        .await?;

        Ok(record.into())
    }

    pub async fn get(&self, id: &str) -> Result<Option<ApiKey>, sqlx::Error> {
        let uuid = Uuid::parse_str(id).map_err(|_| sqlx::Error::RowNotFound)?;
        let record = sqlx::query_as::<_, ApiKeyRecord>(
            "SELECT id, name, key, scopes, enabled, created_at, last_used_at, expires_at, usage_count, metadata, org_id, team_id, user_id, models, rate_limit_override, token_budget FROM api_keys WHERE id = $1",
        )
        .bind(uuid)
        .fetch_optional(&self.pool)
        .await?;

        Ok(record.map(|r| r.into()))
    }

    pub async fn list(&self) -> Result<Vec<ApiKey>, sqlx::Error> {
        let records = sqlx::query_as::<_, ApiKeyRecord>(
            "SELECT id, name, key, scopes, enabled, created_at, last_used_at, expires_at, usage_count, metadata, org_id, team_id, user_id, models, rate_limit_override, token_budget FROM api_keys ORDER BY created_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(records.into_iter().map(|r| r.into()).collect())
    }

    pub async fn list_by_org(&self, org_id: &str) -> Result<Vec<ApiKey>, sqlx::Error> {
        let records = sqlx::query_as::<_, ApiKeyRecord>(
            "SELECT id, name, key, scopes, enabled, created_at, last_used_at, expires_at, usage_count, metadata, org_id, team_id, user_id, models, rate_limit_override, token_budget FROM api_keys WHERE org_id = $1 ORDER BY created_at DESC",
        )
        .bind(org_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(records.into_iter().map(|r| r.into()).collect())
    }

    pub async fn update(&self, id: &str, request: UpdateApiKeyRequest) -> Result<Option<ApiKey>, sqlx::Error> {
        let uuid = Uuid::parse_str(id).map_err(|_| sqlx::Error::RowNotFound)?;

        // Build dynamic UPDATE query
        let mut updates = Vec::new();
        let mut param_count = 1u32;

        if request.name.is_some() {
            updates.push(format!("name = ${}", param_count));
            param_count += 1;
        }
        if request.scopes.is_some() {
            updates.push(format!("scopes = ${}", param_count));
            param_count += 1;
        }
        if request.enabled.is_some() {
            updates.push(format!("enabled = ${}", param_count));
            param_count += 1;
        }
        if request.expires_at.is_some() {
            updates.push(format!("expires_at = ${}", param_count));
            param_count += 1;
        }
        if request.metadata.is_some() {
            updates.push(format!("metadata = ${}", param_count));
            param_count += 1;
        }
        if request.org_id.is_some() {
            updates.push(format!("org_id = ${}", param_count));
            param_count += 1;
        }
        if request.team_id.is_some() {
            updates.push(format!("team_id = ${}", param_count));
            param_count += 1;
        }
        if request.user_id.is_some() {
            updates.push(format!("user_id = ${}", param_count));
            param_count += 1;
        }
        if request.models.is_some() {
            updates.push(format!("models = ${}", param_count));
            param_count += 1;
        }
        if request.rate_limit_override.is_some() {
            updates.push(format!("rate_limit_override = ${}", param_count));
            param_count += 1;
        }
        if request.token_budget.is_some() {
            updates.push(format!("token_budget = ${}", param_count));
            param_count += 1;
        }

        if updates.is_empty() {
            return self.get(id).await;
        }

        let query = format!(
            "UPDATE api_keys SET {} WHERE id = ${} RETURNING id, name, key, scopes, enabled, created_at, last_used_at, expires_at, usage_count, metadata, org_id, team_id, user_id, models, rate_limit_override, token_budget",
            updates.join(", "),
            param_count
        );

        let mut q = sqlx::query_as::<_, ApiKeyRecord>(&query).bind(uuid);

        if let Some(name) = &request.name {
            q = q.bind(name);
        }
        if let Some(scopes) = &request.scopes {
            let scopes_val = serde_json::to_value(scopes).unwrap_or_default();
            q = q.bind(scopes_val);
        }
        if let Some(enabled) = request.enabled {
            q = q.bind(enabled);
        }
        if let Some(expires_at) = request.expires_at {
            q = q.bind(expires_at);
        }
        if let Some(metadata) = &request.metadata {
            q = q.bind(metadata);
        }
        if let Some(org_id) = &request.org_id {
            q = q.bind(org_id);
        }
        if let Some(team_id) = &request.team_id {
            q = q.bind(team_id);
        }
        if let Some(user_id) = &request.user_id {
            q = q.bind(user_id);
        }
        if let Some(models) = &request.models {
            let models_val = serde_json::to_value(models).unwrap_or_default();
            q = q.bind(models_val);
        }
        if let Some(rate_limit) = &request.rate_limit_override {
            let rl_val = serde_json::to_value(rate_limit).unwrap_or_default();
            q = q.bind(rl_val);
        }
        if let Some(budget) = &request.token_budget {
            let b_val = serde_json::to_value(budget).unwrap_or_default();
            q = q.bind(b_val);
        }

        let record = q.fetch_optional(&self.pool).await?;
        Ok(record.map(|r| r.into()))
    }

    pub async fn delete(&self, id: &str) -> Result<bool, sqlx::Error> {
        let uuid = Uuid::parse_str(id).map_err(|_| sqlx::Error::RowNotFound)?;
        let result = sqlx::query("DELETE FROM api_keys WHERE id = $1")
            .bind(uuid)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn validate(&self, key: &str) -> Result<Option<ApiKey>, sqlx::Error> {
        let record = sqlx::query_as::<_, ApiKeyRecord>(
            r#"
            UPDATE api_keys
            SET last_used_at = NOW(), usage_count = usage_count + 1
            WHERE key = $1 AND enabled = true AND (expires_at IS NULL OR expires_at > NOW())
            RETURNING id, name, key, scopes, enabled, created_at, last_used_at, expires_at, usage_count, metadata, org_id, team_id, user_id, models, rate_limit_override, token_budget
            "#,
        )
        .bind(key)
        .fetch_optional(&self.pool)
        .await?;

        Ok(record.map(|r| r.into()))
    }

    pub async fn revoke(&self, id: &str) -> Result<bool, sqlx::Error> {
        let uuid = Uuid::parse_str(id).map_err(|_| sqlx::Error::RowNotFound)?;
        let result = sqlx::query("UPDATE api_keys SET enabled = false WHERE id = $1")
            .bind(uuid)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn rotate(&self, id: &str) -> Result<Option<ApiKey>, sqlx::Error> {
        let uuid = Uuid::parse_str(id).map_err(|_| sqlx::Error::RowNotFound)?;
        let new_key = format!("sk-{}", Uuid::new_v4().to_string().replace('-', ""));

        let record = sqlx::query_as::<_, ApiKeyRecord>(
            "UPDATE api_keys SET key = $1 WHERE id = $2 RETURNING id, name, key, scopes, enabled, created_at, last_used_at, expires_at, usage_count, metadata, org_id, team_id, user_id, models, rate_limit_override, token_budget",
        )
        .bind(&new_key)
        .bind(uuid)
        .fetch_optional(&self.pool)
        .await?;

        Ok(record.map(|r| r.into()))
    }

    pub fn pool(&self) -> &sqlx::PgPool {
        &self.pool
    }
}

#[derive(Debug, sqlx::FromRow)]
struct ApiKeyRecord {
    id: Uuid,
    name: String,
    key: String,
    scopes: serde_json::Value,
    enabled: bool,
    created_at: DateTime<Utc>,
    last_used_at: Option<DateTime<Utc>>,
    expires_at: Option<DateTime<Utc>>,
    usage_count: i64,
    metadata: Option<serde_json::Value>,
    org_id: Option<String>,
    team_id: Option<String>,
    user_id: Option<String>,
    models: Option<serde_json::Value>,
    rate_limit_override: Option<serde_json::Value>,
    token_budget: Option<serde_json::Value>,
}

impl From<ApiKeyRecord> for ApiKey {
    fn from(record: ApiKeyRecord) -> Self {
        let scopes = serde_json::from_value(record.scopes).unwrap_or_default();
        ApiKey {
            id: record.id.to_string(),
            name: record.name,
            key: record.key,
            scopes,
            enabled: record.enabled,
            created_at: record.created_at,
            last_used_at: record.last_used_at,
            expires_at: record.expires_at,
            usage_count: record.usage_count as u64,
            metadata: record.metadata,
            org_id: record.org_id,
            team_id: record.team_id,
            user_id: record.user_id,
            models: record.models.and_then(|v| serde_json::from_value(v).ok()),
            rate_limit_override: record.rate_limit_override.and_then(|v| serde_json::from_value(v).ok()),
            token_budget: record.token_budget.and_then(|v| serde_json::from_value(v).ok()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // These tests require a running Postgres instance
    // Run with: cargo test --features postgres -- --ignored

    #[ignore]
    #[tokio::test]
    async fn test_postgres_create_and_validate() {
        let database_url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://localhost:5432/himadri_test".to_string());

        let store = PostgresStore::new(&database_url).await.unwrap();
        let key = store
            .create(CreateApiKeyRequest {
                name: "test".to_string(),
                scopes: vec!["admin".to_string()],
                expires_at: None,
                metadata: None,
                org_id: Some("org-1".to_string()),
                team_id: Some("team-1".to_string()),
                user_id: None,
                models: None,
                rate_limit_override: None,
                token_budget: None,
            })
            .await
            .unwrap();

        assert!(store.validate(&key.key).await.unwrap().is_some());
        assert_eq!(key.org_id, Some("org-1".to_string()));
        assert_eq!(key.team_id, Some("team-1".to_string()));
    }

    #[ignore]
    #[tokio::test]
    async fn test_postgres_disabled_key() {
        let database_url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://localhost:5432/himadri_test".to_string());

        let store = PostgresStore::new(&database_url).await.unwrap();
        let key = store
            .create(CreateApiKeyRequest {
                name: "test".to_string(),
                scopes: vec![],
                expires_at: None,
                metadata: None,
                org_id: None,
                team_id: None,
                user_id: None,
                models: None,
                rate_limit_override: None,
                token_budget: None,
            })
            .await
            .unwrap();

        store
            .update(
                &key.id,
                UpdateApiKeyRequest {
                    enabled: Some(false),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert!(store.validate(&key.key).await.unwrap().is_none());
    }
}
