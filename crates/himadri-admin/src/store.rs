use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

// ─── Shared Types ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKey {
    pub id: String,
    pub name: String,
    pub key: String,
    pub scopes: Vec<String>,
    #[serde(default)]
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub last_used_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub usage_count: u64,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
    #[serde(default)]
    pub org_id: Option<String>,
    #[serde(default)]
    pub team_id: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub models: Option<Vec<String>>,
    #[serde(default)]
    pub rate_limit_override: Option<RateLimitOverride>,
    #[serde(default)]
    pub token_budget: Option<TokenBudget>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitOverride {
    pub requests_per_second: Option<u64>,
    pub burst_size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenBudget {
    pub max_tokens_per_request: Option<u32>,
    pub max_tokens_per_day: Option<u64>,
    pub max_tokens_per_month: Option<u64>,
    pub cost_limit_per_day: Option<f64>,
    pub cost_limit_per_month: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateApiKeyRequest {
    pub name: String,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
    #[serde(default)]
    pub org_id: Option<String>,
    #[serde(default)]
    pub team_id: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub models: Option<Vec<String>>,
    #[serde(default)]
    pub rate_limit_override: Option<RateLimitOverride>,
    #[serde(default)]
    pub token_budget: Option<TokenBudget>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UpdateApiKeyRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub scopes: Option<Vec<String>>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub expires_at: Option<Option<DateTime<Utc>>>,
    #[serde(default)]
    pub metadata: Option<Option<serde_json::Value>>,
    #[serde(default)]
    pub org_id: Option<Option<String>>,
    #[serde(default)]
    pub team_id: Option<Option<String>>,
    #[serde(default)]
    pub user_id: Option<Option<String>>,
    #[serde(default)]
    pub models: Option<Option<Vec<String>>>,
    #[serde(default)]
    pub rate_limit_override: Option<Option<RateLimitOverride>>,
    #[serde(default)]
    pub token_budget: Option<Option<TokenBudget>>,
}

// ─── StoreBackend (abstraction over Memory/Postgres) ─────────────────

#[derive(Clone)]
pub enum StoreBackend {
    Memory(Arc<ApiKeyStore>),
    #[cfg(feature = "postgres")]
    Postgres(Arc<PostgresStore>),
    #[cfg(feature = "sqlite")]
    Sqlite(Arc<SqliteStore>),
}

impl StoreBackend {
    pub async fn new() -> Self {
        // Check for Postgres first
        #[cfg(feature = "postgres")]
        if let Ok(database_url) = std::env::var("DATABASE_URL") {
            if database_url.starts_with("postgres") {
                match PostgresStore::new(&database_url).await {
                    Ok(store) => {
                        tracing::info!("Connected to Postgres store");
                        return StoreBackend::Postgres(Arc::new(store));
                    }
                    Err(e) => {
                        tracing::warn!("Failed to connect to Postgres: {}, falling back", e);
                    }
                }
            }
        }

        // Check for SQLite
        #[cfg(feature = "sqlite")]
        if let Ok(database_url) = std::env::var("DATABASE_URL") {
            if database_url.starts_with("sqlite") {
                match SqliteStore::new(&database_url).await {
                    Ok(store) => {
                        tracing::info!("Connected to SQLite store");
                        return StoreBackend::Sqlite(Arc::new(store));
                    }
                    Err(e) => {
                        tracing::warn!("Failed to connect to SQLite: {}, falling back", e);
                    }
                }
            }
        }

        tracing::info!("Using in-memory store (set DATABASE_URL for Postgres/SQLite)");
        StoreBackend::Memory(Arc::new(ApiKeyStore::new()))
    }

    pub async fn create(&self, request: CreateApiKeyRequest) -> Result<ApiKey, String> {
        match self {
            StoreBackend::Memory(store) => Ok(store.create(request)),
            #[cfg(feature = "postgres")]
            StoreBackend::Postgres(store) => store.create(request).await.map_err(|e| e.to_string()),
            #[cfg(feature = "sqlite")]
            StoreBackend::Sqlite(store) => store.create(request).await.map_err(|e| e.to_string()),
        }
    }

    pub async fn get(&self, id: &str) -> Result<Option<ApiKey>, String> {
        match self {
            StoreBackend::Memory(store) => Ok(store.get(id)),
            #[cfg(feature = "postgres")]
            StoreBackend::Postgres(store) => store.get(id).await.map_err(|e| e.to_string()),
            #[cfg(feature = "sqlite")]
            StoreBackend::Sqlite(store) => store.get(id).await.map_err(|e| e.to_string()),
        }
    }

    pub async fn list(&self) -> Result<Vec<ApiKey>, String> {
        match self {
            StoreBackend::Memory(store) => Ok(store.list()),
            #[cfg(feature = "postgres")]
            StoreBackend::Postgres(store) => store.list().await.map_err(|e| e.to_string()),
            #[cfg(feature = "sqlite")]
            StoreBackend::Sqlite(store) => store.list().await.map_err(|e| e.to_string()),
        }
    }

    pub async fn update(
        &self,
        id: &str,
        request: UpdateApiKeyRequest,
    ) -> Result<Option<ApiKey>, String> {
        match self {
            StoreBackend::Memory(store) => Ok(store.update(id, request)),
            #[cfg(feature = "postgres")]
            StoreBackend::Postgres(store) => {
                store.update(id, request).await.map_err(|e| e.to_string())
            }
            #[cfg(feature = "sqlite")]
            StoreBackend::Sqlite(store) => {
                store.update(id, request).await.map_err(|e| e.to_string())
            }
        }
    }

    pub async fn delete(&self, id: &str) -> Result<bool, String> {
        match self {
            StoreBackend::Memory(store) => Ok(store.delete(id)),
            #[cfg(feature = "postgres")]
            StoreBackend::Postgres(store) => store.delete(id).await.map_err(|e| e.to_string()),
            #[cfg(feature = "sqlite")]
            StoreBackend::Sqlite(store) => store.delete(id).await.map_err(|e| e.to_string()),
        }
    }

    pub async fn validate(&self, key: &str) -> Result<Option<ApiKey>, String> {
        match self {
            StoreBackend::Memory(store) => Ok(store.validate(key)),
            #[cfg(feature = "postgres")]
            StoreBackend::Postgres(store) => store.validate(key).await.map_err(|e| e.to_string()),
            #[cfg(feature = "sqlite")]
            StoreBackend::Sqlite(store) => store.validate(key).await.map_err(|e| e.to_string()),
        }
    }

    pub async fn revoke(&self, id: &str) -> Result<bool, String> {
        match self {
            StoreBackend::Memory(store) => Ok(store.revoke(id)),
            #[cfg(feature = "postgres")]
            StoreBackend::Postgres(store) => store.revoke(id).await.map_err(|e| e.to_string()),
            #[cfg(feature = "sqlite")]
            StoreBackend::Sqlite(store) => store.revoke(id).await.map_err(|e| e.to_string()),
        }
    }

    pub async fn rotate(&self, id: &str) -> Result<Option<ApiKey>, String> {
        match self {
            StoreBackend::Memory(store) => Ok(store.rotate(id)),
            #[cfg(feature = "postgres")]
            StoreBackend::Postgres(store) => store.rotate(id).await.map_err(|e| e.to_string()),
            #[cfg(feature = "sqlite")]
            StoreBackend::Sqlite(store) => store.rotate(id).await.map_err(|e| e.to_string()),
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            StoreBackend::Memory(store) => store.list().is_empty(),
            #[cfg(feature = "postgres")]
            StoreBackend::Postgres(_) => false,
            #[cfg(feature = "sqlite")]
            StoreBackend::Sqlite(_) => false,
        }
    }
}

// ─── In-Memory Store ─────────────────────────────────────────────────

pub struct ApiKeyStore {
    keys: DashMap<String, ApiKey>,
    keys_by_key: DashMap<String, String>,
}

impl ApiKeyStore {
    pub fn new() -> Self {
        Self {
            keys: DashMap::new(),
            keys_by_key: DashMap::new(),
        }
    }

    pub fn create(&self, request: CreateApiKeyRequest) -> ApiKey {
        let id = Uuid::new_v4().to_string();
        let key = format!("sk-{}", Uuid::new_v4().to_string().replace('-', ""));

        let api_key = ApiKey {
            id: id.clone(),
            name: request.name,
            key: key.clone(),
            scopes: request.scopes,
            enabled: true,
            created_at: Utc::now(),
            last_used_at: None,
            expires_at: request.expires_at,
            usage_count: 0,
            metadata: request.metadata,
            org_id: request.org_id,
            team_id: request.team_id,
            user_id: request.user_id,
            models: request.models,
            rate_limit_override: request.rate_limit_override,
            token_budget: request.token_budget,
        };

        self.keys.insert(id.clone(), api_key.clone());
        self.keys_by_key.insert(key, id);
        api_key
    }

    pub fn get(&self, id: &str) -> Option<ApiKey> {
        self.keys.get(id).map(|k| k.clone())
    }

    pub fn list(&self) -> Vec<ApiKey> {
        self.keys.iter().map(|k| k.value().clone()).collect()
    }

    pub fn update(&self, id: &str, request: UpdateApiKeyRequest) -> Option<ApiKey> {
        self.keys.get_mut(id).map(|mut key| {
            if let Some(name) = request.name {
                key.name = name;
            }
            if let Some(scopes) = request.scopes {
                key.scopes = scopes;
            }
            if let Some(enabled) = request.enabled {
                key.enabled = enabled;
            }
            if let Some(expires_at) = request.expires_at {
                key.expires_at = expires_at;
            }
            if let Some(metadata) = request.metadata {
                key.metadata = metadata;
            }
            if let Some(org_id) = request.org_id {
                key.org_id = org_id;
            }
            if let Some(team_id) = request.team_id {
                key.team_id = team_id;
            }
            if let Some(user_id) = request.user_id {
                key.user_id = user_id;
            }
            if let Some(models) = request.models {
                key.models = models;
            }
            if let Some(rate_limit_override) = request.rate_limit_override {
                key.rate_limit_override = rate_limit_override;
            }
            if let Some(token_budget) = request.token_budget {
                key.token_budget = token_budget;
            }
            key.clone()
        })
    }

    pub fn delete(&self, id: &str) -> bool {
        if let Some((_, key)) = self.keys.remove(id) {
            self.keys_by_key.remove(&key.key);
            true
        } else {
            false
        }
    }

    pub fn validate(&self, key: &str) -> Option<ApiKey> {
        let id = self.keys_by_key.get(key)?;
        let mut api_key = self.keys.get(id.value())?.clone();
        if !api_key.enabled {
            return None;
        }
        if let Some(expires_at) = api_key.expires_at {
            if Utc::now() > expires_at {
                return None;
            }
        }
        api_key.last_used_at = Some(Utc::now());
        api_key.usage_count += 1;
        self.keys.insert(api_key.id.clone(), api_key.clone());
        Some(api_key)
    }

    pub fn revoke(&self, id: &str) -> bool {
        self.keys
            .get_mut(id)
            .map(|mut key| {
                key.enabled = false;
            })
            .is_some()
    }

    pub fn rotate(&self, id: &str) -> Option<ApiKey> {
        let old_key = self.keys.get(id)?.clone();
        let new_key = format!("sk-{}", Uuid::new_v4().to_string().replace('-', ""));
        self.keys_by_key.remove(&old_key.key);
        let updated = ApiKey {
            key: new_key.clone(),
            ..old_key
        };
        self.keys.insert(id.to_string(), updated.clone());
        self.keys_by_key.insert(new_key, id.to_string());
        Some(updated)
    }
}

impl Default for ApiKeyStore {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Postgres Store ──────────────────────────────────────────────────

#[cfg(feature = "postgres")]
pub struct PostgresStore {
    pool: sqlx::PgPool,
}

#[cfg(feature = "postgres")]
impl PostgresStore {
    pub async fn new(database_url: &str) -> Result<Self, sqlx::Error> {
        let pool = sqlx::PgPool::connect(database_url).await?;
        // Run embedded migrations - tracks version, only applies pending ones
        sqlx::migrate!("migrations/postgres")
            .run(&pool)
            .await
            .map_err(|e| sqlx::Error::Migrate(Box::new(e)))?;
        Ok(Self { pool })
    }

    pub async fn create(&self, request: CreateApiKeyRequest) -> Result<ApiKey, sqlx::Error> {
        let id = Uuid::new_v4();
        let key = format!("sk-{}", Uuid::new_v4().to_string().replace('-', ""));
        let scopes = serde_json::to_value(&request.scopes).unwrap_or_default();
        let models = request
            .models
            .as_ref()
            .map(|m| serde_json::to_value(m).unwrap_or_default());
        let rate_limit = request
            .rate_limit_override
            .as_ref()
            .map(|r| serde_json::to_value(r).unwrap_or_default());
        let budget = request
            .token_budget
            .as_ref()
            .map(|b| serde_json::to_value(b).unwrap_or_default());

        let record = sqlx::query_as::<_, ApiKeyRow>(
            r#"INSERT INTO api_keys (id, name, key, scopes, enabled, created_at, expires_at, metadata, org_id, team_id, user_id, models, rate_limit_override, token_budget)
               VALUES ($1, $2, $3, $4, true, NOW(), $5, $6, $7, $8, $9, $10, $11, $12)
               RETURNING id, name, key, scopes, enabled, created_at, last_used_at, expires_at, usage_count, metadata, org_id, team_id, user_id, models, rate_limit_override, token_budget"#,
        )
        .bind(id).bind(&request.name).bind(&key).bind(&scopes)
        .bind(request.expires_at).bind(request.metadata)
        .bind(&request.org_id).bind(&request.team_id).bind(&request.user_id)
        .bind(models).bind(rate_limit).bind(budget)
        .fetch_one(&self.pool).await?;
        Ok(record.into())
    }

    pub async fn get(&self, id: &str) -> Result<Option<ApiKey>, sqlx::Error> {
        let uuid = Uuid::parse_str(id).map_err(|_| sqlx::Error::RowNotFound)?;
        let record = sqlx::query_as::<_, ApiKeyRow>(
            "SELECT id, name, key, scopes, enabled, created_at, last_used_at, expires_at, usage_count, metadata, org_id, team_id, user_id, models, rate_limit_override, token_budget FROM api_keys WHERE id = $1",
        ).bind(uuid).fetch_optional(&self.pool).await?;
        Ok(record.map(|r| r.into()))
    }

    pub async fn list(&self) -> Result<Vec<ApiKey>, sqlx::Error> {
        let records = sqlx::query_as::<_, ApiKeyRow>(
            "SELECT id, name, key, scopes, enabled, created_at, last_used_at, expires_at, usage_count, metadata, org_id, team_id, user_id, models, rate_limit_override, token_budget FROM api_keys ORDER BY created_at DESC",
        ).fetch_all(&self.pool).await?;
        Ok(records.into_iter().map(|r| r.into()).collect())
    }

    pub async fn list_by_org(&self, org_id: &str) -> Result<Vec<ApiKey>, sqlx::Error> {
        let records = sqlx::query_as::<_, ApiKeyRow>(
            "SELECT id, name, key, scopes, enabled, created_at, last_used_at, expires_at, usage_count, metadata, org_id, team_id, user_id, models, rate_limit_override, token_budget FROM api_keys WHERE org_id = $1 ORDER BY created_at DESC",
        ).bind(org_id).fetch_all(&self.pool).await?;
        Ok(records.into_iter().map(|r| r.into()).collect())
    }

    pub async fn update(
        &self,
        id: &str,
        request: UpdateApiKeyRequest,
    ) -> Result<Option<ApiKey>, sqlx::Error> {
        let uuid = Uuid::parse_str(id).map_err(|_| sqlx::Error::RowNotFound)?;
        let mut sets = Vec::new();
        let mut p = 1u32;
        if request.name.is_some() {
            sets.push(format!("name = ${}", p));
            p += 1;
        }
        if request.scopes.is_some() {
            sets.push(format!("scopes = ${}", p));
            p += 1;
        }
        if request.enabled.is_some() {
            sets.push(format!("enabled = ${}", p));
            p += 1;
        }
        if request.expires_at.is_some() {
            sets.push(format!("expires_at = ${}", p));
            p += 1;
        }
        if request.metadata.is_some() {
            sets.push(format!("metadata = ${}", p));
            p += 1;
        }
        if request.org_id.is_some() {
            sets.push(format!("org_id = ${}", p));
            p += 1;
        }
        if request.team_id.is_some() {
            sets.push(format!("team_id = ${}", p));
            p += 1;
        }
        if request.user_id.is_some() {
            sets.push(format!("user_id = ${}", p));
            p += 1;
        }
        if request.models.is_some() {
            sets.push(format!("models = ${}", p));
            p += 1;
        }
        if request.rate_limit_override.is_some() {
            sets.push(format!("rate_limit_override = ${}", p));
            p += 1;
        }
        if request.token_budget.is_some() {
            sets.push(format!("token_budget = ${}", p));
        }
        if sets.is_empty() {
            return self.get(id).await;
        }

        let q = format!("UPDATE api_keys SET {} WHERE id = ${} RETURNING id, name, key, scopes, enabled, created_at, last_used_at, expires_at, usage_count, metadata, org_id, team_id, user_id, models, rate_limit_override, token_budget", sets.join(", "), p);
        let mut q = sqlx::query_as::<_, ApiKeyRow>(&q).bind(uuid);
        if let Some(v) = &request.name {
            q = q.bind(v);
        }
        if let Some(v) = &request.scopes {
            q = q.bind(serde_json::to_value(v).unwrap_or_default());
        }
        if let Some(v) = request.enabled {
            q = q.bind(v);
        }
        if let Some(v) = request.expires_at {
            q = q.bind(v);
        }
        if let Some(v) = &request.metadata {
            q = q.bind(v);
        }
        if let Some(v) = &request.org_id {
            q = q.bind(v);
        }
        if let Some(v) = &request.team_id {
            q = q.bind(v);
        }
        if let Some(v) = &request.user_id {
            q = q.bind(v);
        }
        if let Some(v) = &request.models {
            q = q.bind(serde_json::to_value(v).unwrap_or_default());
        }
        if let Some(v) = &request.rate_limit_override {
            q = q.bind(serde_json::to_value(v).unwrap_or_default());
        }
        if let Some(v) = &request.token_budget {
            q = q.bind(serde_json::to_value(v).unwrap_or_default());
        }
        Ok(q.fetch_optional(&self.pool).await?.map(|r| r.into()))
    }

    pub async fn delete(&self, id: &str) -> Result<bool, sqlx::Error> {
        let uuid = Uuid::parse_str(id).map_err(|_| sqlx::Error::RowNotFound)?;
        let r = sqlx::query("DELETE FROM api_keys WHERE id = $1")
            .bind(uuid)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn validate(&self, key: &str) -> Result<Option<ApiKey>, sqlx::Error> {
        let record = sqlx::query_as::<_, ApiKeyRow>(
            r#"UPDATE api_keys SET last_used_at = NOW(), usage_count = usage_count + 1
               WHERE key = $1 AND enabled = true AND (expires_at IS NULL OR expires_at > NOW())
               RETURNING id, name, key, scopes, enabled, created_at, last_used_at, expires_at, usage_count, metadata, org_id, team_id, user_id, models, rate_limit_override, token_budget"#,
        ).bind(key).fetch_optional(&self.pool).await?;
        Ok(record.map(|r| r.into()))
    }

    pub async fn revoke(&self, id: &str) -> Result<bool, sqlx::Error> {
        let uuid = Uuid::parse_str(id).map_err(|_| sqlx::Error::RowNotFound)?;
        let r = sqlx::query("UPDATE api_keys SET enabled = false WHERE id = $1")
            .bind(uuid)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn rotate(&self, id: &str) -> Result<Option<ApiKey>, sqlx::Error> {
        let uuid = Uuid::parse_str(id).map_err(|_| sqlx::Error::RowNotFound)?;
        let new_key = format!("sk-{}", Uuid::new_v4().to_string().replace('-', ""));
        let record = sqlx::query_as::<_, ApiKeyRow>(
            "UPDATE api_keys SET key = $1 WHERE id = $2 RETURNING id, name, key, scopes, enabled, created_at, last_used_at, expires_at, usage_count, metadata, org_id, team_id, user_id, models, rate_limit_override, token_budget",
        ).bind(&new_key).bind(uuid).fetch_optional(&self.pool).await?;
        Ok(record.map(|r| r.into()))
    }
}

#[cfg(feature = "postgres")]
#[derive(Debug, sqlx::FromRow)]
struct ApiKeyRow {
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

#[cfg(feature = "postgres")]
impl From<ApiKeyRow> for ApiKey {
    fn from(r: ApiKeyRow) -> Self {
        ApiKey {
            id: r.id.to_string(),
            name: r.name,
            key: r.key,
            scopes: serde_json::from_value(r.scopes).unwrap_or_default(),
            enabled: r.enabled,
            created_at: r.created_at,
            last_used_at: r.last_used_at,
            expires_at: r.expires_at,
            usage_count: r.usage_count as u64,
            metadata: r.metadata,
            org_id: r.org_id,
            team_id: r.team_id,
            user_id: r.user_id,
            models: r.models.and_then(|v| serde_json::from_value(v).ok()),
            rate_limit_override: r
                .rate_limit_override
                .and_then(|v| serde_json::from_value(v).ok()),
            token_budget: r.token_budget.and_then(|v| serde_json::from_value(v).ok()),
        }
    }
}

// ─── SQLite Store ────────────────────────────────────────────────────

#[cfg(feature = "sqlite")]
pub struct SqliteStore {
    pool: sqlx::SqlitePool,
}

#[cfg(feature = "sqlite")]
impl SqliteStore {
    pub async fn new(database_url: &str) -> Result<Self, sqlx::Error> {
        // Ensure SQLite creates the file if it doesn't exist
        let url = if database_url.contains('?') {
            database_url.to_string()
        } else {
            format!("{}?mode=rwc", database_url)
        };
        let pool = sqlx::SqlitePool::connect(&url).await?;
        // Run embedded migrations - tracks version, only applies pending ones
        sqlx::migrate!("migrations/sqlite")
            .run(&pool)
            .await
            .map_err(|e| sqlx::Error::Migrate(Box::new(e)))?;
        Ok(Self { pool })
    }

    pub async fn create(&self, request: CreateApiKeyRequest) -> Result<ApiKey, sqlx::Error> {
        let id = Uuid::new_v4().to_string();
        let key = format!("sk-{}", Uuid::new_v4().to_string().replace('-', ""));
        let scopes = serde_json::to_value(&request.scopes).unwrap_or_default();
        let models = request.models.as_ref().map(|m| serde_json::to_value(m).unwrap_or_default());
        let rate_limit = request.rate_limit_override.as_ref().map(|r| serde_json::to_value(r).unwrap_or_default());
        let budget = request.token_budget.as_ref().map(|b| serde_json::to_value(b).unwrap_or_default());

        sqlx::query(
            r#"INSERT INTO api_keys (id, name, key, scopes, enabled, created_at, expires_at, metadata, org_id, team_id, user_id, models, rate_limit_override, token_budget)
               VALUES (?, ?, ?, ?, 1, datetime('now'), ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(&id).bind(&request.name).bind(&key).bind(&scopes)
        .bind(request.expires_at.map(|dt| dt.to_rfc3339())).bind(request.metadata.map(|m| m.to_string()))
        .bind(&request.org_id).bind(&request.team_id).bind(&request.user_id)
        .bind(models.map(|m| m.to_string())).bind(rate_limit.map(|r| r.to_string())).bind(budget.map(|b| b.to_string()))
        .execute(&self.pool)
        .await?;

        self.get(&id).await?.ok_or(sqlx::Error::RowNotFound)
    }

    pub async fn get(&self, id: &str) -> Result<Option<ApiKey>, sqlx::Error> {
        let row = sqlx::query_as::<_, SqliteApiKeyRow>(
            "SELECT id, name, key, scopes, enabled, created_at, last_used_at, expires_at, usage_count, metadata, org_id, team_id, user_id, models, rate_limit_override, token_budget FROM api_keys WHERE id = ?",
        ).bind(id).fetch_optional(&self.pool).await?;
        Ok(row.map(|r| r.into()))
    }

    pub async fn list(&self) -> Result<Vec<ApiKey>, sqlx::Error> {
        let rows = sqlx::query_as::<_, SqliteApiKeyRow>(
            "SELECT id, name, key, scopes, enabled, created_at, last_used_at, expires_at, usage_count, metadata, org_id, team_id, user_id, models, rate_limit_override, token_budget FROM api_keys ORDER BY created_at DESC",
        ).fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(|r| r.into()).collect())
    }

    pub async fn update(&self, id: &str, request: UpdateApiKeyRequest) -> Result<Option<ApiKey>, sqlx::Error> {
        // Fetch current record, apply changes, save back
        let current = self.get(id).await?.ok_or(sqlx::Error::RowNotFound)?;

        let name = request.name.unwrap_or(current.name);
        let scopes = request.scopes.unwrap_or(current.scopes);
        let enabled = request.enabled.unwrap_or(current.enabled);
        let metadata = request.metadata.unwrap_or(current.metadata);
        let org_id = request.org_id.unwrap_or(current.org_id);
        let team_id = request.team_id.unwrap_or(current.team_id);
        let user_id = request.user_id.unwrap_or(current.user_id);

        let scopes_json = serde_json::to_value(&scopes).unwrap_or_default().to_string();
        let enabled_int = enabled as i32;
        let metadata_str = metadata.as_ref().map(|m| m.to_string());

        sqlx::query(
            "UPDATE api_keys SET name = ?, scopes = ?, enabled = ?, metadata = ?, org_id = ?, team_id = ?, user_id = ? WHERE id = ?",
        )
        .bind(&name).bind(&scopes_json).bind(enabled_int).bind(&metadata_str)
        .bind(&org_id).bind(&team_id).bind(&user_id).bind(id)
        .execute(&self.pool)
        .await?;

        self.get(id).await
    }

    pub async fn delete(&self, id: &str) -> Result<bool, sqlx::Error> {
        let r = sqlx::query("DELETE FROM api_keys WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn validate(&self, key: &str) -> Result<Option<ApiKey>, sqlx::Error> {
        let row = sqlx::query_as::<_, SqliteApiKeyRow>(
            r#"UPDATE api_keys SET last_used_at = datetime('now'), usage_count = usage_count + 1
               WHERE key = ? AND enabled = 1 AND (expires_at IS NULL OR expires_at > datetime('now'))
               RETURNING id, name, key, scopes, enabled, created_at, last_used_at, expires_at, usage_count, metadata, org_id, team_id, user_id, models, rate_limit_override, token_budget"#,
        ).bind(key).fetch_optional(&self.pool).await?;
        Ok(row.map(|r| r.into()))
    }

    pub async fn revoke(&self, id: &str) -> Result<bool, sqlx::Error> {
        let r = sqlx::query("UPDATE api_keys SET enabled = 0 WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn rotate(&self, id: &str) -> Result<Option<ApiKey>, sqlx::Error> {
        let new_key = format!("sk-{}", Uuid::new_v4().to_string().replace('-', ""));
        sqlx::query("UPDATE api_keys SET key = ? WHERE id = ?")
            .bind(&new_key).bind(id)
            .execute(&self.pool)
            .await?;
        self.get(id).await
    }
}

#[cfg(feature = "sqlite")]
#[derive(Debug, sqlx::FromRow)]
struct SqliteApiKeyRow {
    id: String,
    name: String,
    key: String,
    scopes: String,
    enabled: i32,
    created_at: String,
    last_used_at: Option<String>,
    expires_at: Option<String>,
    usage_count: i64,
    metadata: Option<String>,
    org_id: Option<String>,
    team_id: Option<String>,
    user_id: Option<String>,
    models: Option<String>,
    rate_limit_override: Option<String>,
    token_budget: Option<String>,
}

#[cfg(feature = "sqlite")]
impl From<SqliteApiKeyRow> for ApiKey {
    fn from(r: SqliteApiKeyRow) -> Self {
        ApiKey {
            id: r.id,
            name: r.name,
            key: r.key,
            scopes: serde_json::from_str(&r.scopes).unwrap_or_default(),
            enabled: r.enabled != 0,
            created_at: DateTime::parse_from_rfc3339(&r.created_at)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_default(),
            last_used_at: r.last_used_at.and_then(|s| DateTime::parse_from_rfc3339(&s).ok()).map(|dt| dt.with_timezone(&Utc)),
            expires_at: r.expires_at.and_then(|s| DateTime::parse_from_rfc3339(&s).ok()).map(|dt| dt.with_timezone(&Utc)),
            usage_count: r.usage_count as u64,
            metadata: r.metadata.and_then(|s| serde_json::from_str(&s).ok()),
            org_id: r.org_id,
            team_id: r.team_id,
            user_id: r.user_id,
            models: r.models.and_then(|s| serde_json::from_str(&s).ok()),
            rate_limit_override: r.rate_limit_override.and_then(|s| serde_json::from_str(&s).ok()),
            token_budget: r.token_budget.and_then(|s| serde_json::from_str(&s).ok()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_and_validate() {
        let store = ApiKeyStore::new();
        let key = store.create(CreateApiKeyRequest {
            name: "test".into(),
            scopes: vec!["admin".into()],
            expires_at: None,
            metadata: None,
            org_id: Some("org-1".into()),
            team_id: Some("team-1".into()),
            user_id: None,
            models: None,
            rate_limit_override: None,
            token_budget: None,
        });
        assert!(store.validate(&key.key).is_some());
        assert_eq!(key.org_id, Some("org-1".into()));
        assert_eq!(key.team_id, Some("team-1".into()));
    }

    #[test]
    fn test_disabled_key() {
        let store = ApiKeyStore::new();
        let key = store.create(CreateApiKeyRequest {
            name: "test".into(),
            scopes: vec![],
            expires_at: None,
            metadata: None,
            org_id: None,
            team_id: None,
            user_id: None,
            models: None,
            rate_limit_override: None,
            token_budget: None,
        });
        store.update(
            &key.id,
            UpdateApiKeyRequest {
                enabled: Some(false),
                ..Default::default()
            },
        );
        assert!(store.validate(&key.key).is_none());
    }

    #[test]
    fn test_delete() {
        let store = ApiKeyStore::new();
        let key = store.create(CreateApiKeyRequest {
            name: "test".into(),
            scopes: vec![],
            expires_at: None,
            metadata: None,
            org_id: None,
            team_id: None,
            user_id: None,
            models: None,
            rate_limit_override: None,
            token_budget: None,
        });
        assert!(store.delete(&key.id));
        assert!(store.validate(&key.key).is_none());
    }
}
