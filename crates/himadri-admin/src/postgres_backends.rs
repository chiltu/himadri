use himadri_core::Config;
use sqlx::PgPool;

use crate::config_store::ConfigStore;
use crate::request_log::{
    MaintenanceQuery, RequestLogEntry, RequestLogListResult, RequestLogQuery, RequestLogStore,
};

/// Postgres-backed config store
pub struct PostgresConfigStore {
    pool: PgPool,
}

impl PostgresConfigStore {
    pub async fn new(database_url: &str) -> Result<Self, sqlx::Error> {
        let pool = sqlx::PgPool::connect(database_url).await?;
        Self::run_migrations(&pool).await?;
        Ok(Self { pool })
    }

    async fn run_migrations(pool: &PgPool) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS config_history (
                version SERIAL PRIMARY KEY,
                config JSONB NOT NULL,
                updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                rolled_back_from INTEGER
            );
            "#,
        )
        .execute(pool)
        .await?;
        Ok(())
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

impl ConfigStore for PostgresConfigStore {
    fn save(&self, _config: &Config) -> Result<(), String> {
        // For Postgres config store, we use the GatewayConfigManager's history
        // This is a simplified version - full implementation would save to DB
        Ok(())
    }

    fn load(&self) -> Result<Option<Config>, String> {
        // For Postgres config store, load from DB
        // This is a simplified version - full implementation would load from DB
        Ok(None)
    }

    fn delete(&self) -> Result<(), String> {
        // For Postgres config store, clear from DB
        Ok(())
    }
}

/// Postgres-backed request log store
pub struct PostgresRequestLogStore {
    pool: PgPool,
}

impl PostgresRequestLogStore {
    pub async fn new(database_url: &str) -> Result<Self, sqlx::Error> {
        let pool = sqlx::PgPool::connect(database_url).await?;
        Self::run_migrations(&pool).await?;
        Ok(Self { pool })
    }

    async fn run_migrations(pool: &PgPool) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS request_logs (
                id SERIAL PRIMARY KEY,
                trace_id VARCHAR(255) NOT NULL,
                stage VARCHAR(50) NOT NULL,
                model VARCHAR(255) NOT NULL,
                provider VARCHAR(255) NOT NULL,
                prompt_tokens INTEGER NOT NULL DEFAULT 0,
                completion_tokens INTEGER NOT NULL DEFAULT 0,
                total_tokens INTEGER NOT NULL DEFAULT 0,
                error_message TEXT,
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
            );
            CREATE INDEX IF NOT EXISTS idx_request_logs_trace_id ON request_logs(trace_id);
            CREATE INDEX IF NOT EXISTS idx_request_logs_model ON request_logs(model);
            CREATE INDEX IF NOT EXISTS idx_request_logs_provider ON request_logs(provider);
            CREATE INDEX IF NOT EXISTS idx_request_logs_created_at ON request_logs(created_at);
            "#,
        )
        .execute(pool)
        .await?;
        Ok(())
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

impl RequestLogStore for PostgresRequestLogStore {
    fn write(&self, entry: RequestLogEntry) -> Result<(), String> {
        // Note: In production, this should use tokio::spawn for async execution
        // For now, we block on the async call
        let pool = self.pool.clone();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                sqlx::query(
                    r#"
                    INSERT INTO request_logs (trace_id, stage, model, provider, prompt_tokens, completion_tokens, total_tokens, error_message, created_at)
                    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                    "#,
                )
                .bind(&entry.trace_id)
                .bind(&entry.stage)
                .bind(&entry.model)
                .bind(&entry.provider)
                .bind(entry.prompt_tokens as i32)
                .bind(entry.completion_tokens as i32)
                .bind(entry.total_tokens as i32)
                .bind(&entry.error_message)
                .bind(entry.created_at)
                .execute(&pool)
                .await
                .map_err(|e| e.to_string())?;
                Ok(())
            })
        })
    }

    fn list(&self, query: RequestLogQuery) -> Result<RequestLogListResult, String> {
        let pool = self.pool.clone();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                // Build query
                let mut conditions = Vec::new();
                let mut param_count = 1u32;

                if query.model.is_some() {
                    conditions.push(format!("model = ${}", param_count));
                    param_count += 1;
                }
                if query.provider.is_some() {
                    conditions.push(format!("provider = ${}", param_count));
                    param_count += 1;
                }
                if query.since.is_some() {
                    conditions.push(format!("created_at >= ${}", param_count));
                    param_count += 1;
                }
                let _ = param_count;

                let where_clause = if conditions.is_empty() {
                    String::new()
                } else {
                    format!("WHERE {}", conditions.join(" AND "))
                };

                // Count query
                let count_query = format!("SELECT COUNT(*) FROM request_logs {}", where_clause);
                let count: i64 = sqlx::query_scalar(&count_query)
                    .fetch_one(&pool)
                    .await
                    .map_err(|e| e.to_string())?;

                // Data query with pagination
                let limit = query.limit.unwrap_or(100) as i64;
                let offset = query.offset.unwrap_or(0) as i64;
                let data_query = format!(
                    "SELECT trace_id, stage, model, provider, prompt_tokens, completion_tokens, total_tokens, error_message, created_at FROM request_logs {} ORDER BY created_at DESC LIMIT {} OFFSET {}",
                    where_clause, limit, offset
                );

                let rows: Vec<RequestLogRow> = sqlx::query_as(&data_query)
                    .fetch_all(&pool)
                    .await
                    .map_err(|e| e.to_string())?;

                let data: Vec<RequestLogEntry> = rows.into_iter().map(|r| RequestLogEntry {
                    trace_id: r.trace_id,
                    stage: r.stage,
                    model: r.model,
                    provider: r.provider,
                    prompt_tokens: r.prompt_tokens as u32,
                    completion_tokens: r.completion_tokens as u32,
                    total_tokens: r.total_tokens as u32,
                    error_message: r.error_message,
                    created_at: r.created_at,
                }).collect();

                Ok(RequestLogListResult {
                    data,
                    total: count as usize,
                })
            })
        })
    }

    fn delete(&self, query: MaintenanceQuery) -> Result<usize, String> {
        let pool = self.pool.clone();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                let mut conditions = Vec::new();
                let mut param_count = 1u32;

                if let Some(_model) = query.model {
                    conditions.push(format!("model = ${}", param_count));
                    param_count += 1;
                }
                if let Some(_provider) = query.provider {
                    conditions.push(format!("provider = ${}", param_count));
                    param_count += 1;
                }
                if let Some(_before) = query.before {
                    conditions.push(format!("created_at < ${}", param_count));
                    param_count += 1;
                }
                let _ = param_count;

                let where_clause = if conditions.is_empty() {
                    String::new()
                } else {
                    format!("WHERE {}", conditions.join(" AND "))
                };

                let result = sqlx::query(&format!("DELETE FROM request_logs {}", where_clause))
                    .execute(&pool)
                    .await
                    .map_err(|e| e.to_string())?;

                Ok(result.rows_affected() as usize)
            })
        })
    }

    fn count(&self) -> usize {
        let pool = self.pool.clone();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM request_logs")
                    .fetch_one(&pool)
                    .await
                    .map(|c| c as usize)
                    .unwrap_or(0)
            })
        })
    }
}

#[derive(sqlx::FromRow)]
struct RequestLogRow {
    trace_id: String,
    stage: String,
    model: String,
    provider: String,
    prompt_tokens: i32,
    completion_tokens: i32,
    total_tokens: i32,
    error_message: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
}

/// Postgres-backed usage store
pub struct PostgresUsageStore {
    pool: PgPool,
}

impl PostgresUsageStore {
    pub async fn new(database_url: &str) -> Result<Self, sqlx::Error> {
        let pool = sqlx::PgPool::connect(database_url).await?;
        Self::run_migrations(&pool).await?;
        Ok(Self { pool })
    }

    async fn run_migrations(pool: &PgPool) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS usage_records (
                id SERIAL PRIMARY KEY,
                request_id VARCHAR(255) NOT NULL UNIQUE,
                api_key_id VARCHAR(255),
                model VARCHAR(255) NOT NULL,
                provider VARCHAR(255) NOT NULL,
                prompt_tokens INTEGER NOT NULL DEFAULT 0,
                completion_tokens INTEGER NOT NULL DEFAULT 0,
                total_tokens INTEGER NOT NULL DEFAULT 0,
                cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0.0,
                latency_ms BIGINT NOT NULL DEFAULT 0,
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                success BOOLEAN NOT NULL DEFAULT true,
                error_message TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_usage_records_api_key_id ON usage_records(api_key_id);
            CREATE INDEX IF NOT EXISTS idx_usage_records_model ON usage_records(model);
            CREATE INDEX IF NOT EXISTS idx_usage_records_created_at ON usage_records(created_at);
            "#,
        )
        .execute(pool)
        .await?;
        Ok(())
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Get usage stats for an API key
    pub async fn get_key_stats(&self, api_key_id: &str) -> Result<UsageStats, sqlx::Error> {
        let row = sqlx::query_as::<_, UsageStatsRow>(
            r#"
            SELECT
                api_key_id,
                COUNT(*) as total_requests,
                COUNT(*) FILTER (WHERE success) as successful_requests,
                COUNT(*) FILTER (WHERE NOT success) as failed_requests,
                COALESCE(SUM(prompt_tokens), 0) as total_prompt_tokens,
                COALESCE(SUM(completion_tokens), 0) as total_completion_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COALESCE(SUM(cost_usd), 0.0) as total_cost_usd,
                COALESCE(AVG(latency_ms), 0.0) as avg_latency_ms,
                MAX(created_at) as last_request_at
            FROM usage_records
            WHERE api_key_id = $1
            "#,
        )
        .bind(api_key_id)
        .fetch_one(&self.pool)
        .await?;

        // Get models used
        let models: Vec<String> = sqlx::query_scalar::<_, String>(
            "SELECT DISTINCT model FROM usage_records WHERE api_key_id = $1",
        )
        .bind(api_key_id)
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();

        Ok(UsageStats {
            api_key_id: row.api_key_id,
            total_requests: row.total_requests as u64,
            successful_requests: row.successful_requests as u64,
            failed_requests: row.failed_requests as u64,
            total_prompt_tokens: row.total_prompt_tokens as u64,
            total_completion_tokens: row.total_completion_tokens as u64,
            total_tokens: row.total_tokens as u64,
            total_cost_usd: row.total_cost_usd,
            avg_latency_ms: row.avg_latency_ms,
            last_request_at: row.last_request_at,
            models_used: models,
        })
    }

    /// Get dashboard summary
    pub async fn get_dashboard(&self) -> Result<DashboardSummary, sqlx::Error> {
        let row = sqlx::query_as::<_, DashboardRow>(
            r#"
            SELECT
                COUNT(*) as total_requests,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COALESCE(SUM(cost_usd), 0.0) as total_cost_usd,
                COALESCE(AVG(latency_ms), 0.0) as avg_latency_ms,
                COUNT(*) FILTER (WHERE NOT success) as errors
            FROM usage_records
            "#,
        )
        .fetch_one(&self.pool)
        .await?;

        // Get top models
        let top_models: Vec<ModelUsageRow> = sqlx::query_as::<_, ModelUsageRow>(
            r#"
            SELECT model, COUNT(*) as requests, COALESCE(SUM(total_tokens), 0) as tokens, COALESCE(SUM(cost_usd), 0.0) as cost_usd
            FROM usage_records GROUP BY model ORDER BY requests DESC LIMIT 10
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();

        // Get top providers
        let top_providers: Vec<ProviderUsageRow> = sqlx::query_as::<_, ProviderUsageRow>(
            r#"
            SELECT provider, COUNT(*) as requests, COALESCE(SUM(total_tokens), 0) as tokens, COALESCE(SUM(cost_usd), 0.0) as cost_usd
            FROM usage_records GROUP BY provider ORDER BY requests DESC LIMIT 10
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();

        Ok(DashboardSummary {
            total_keys: 0, // Will be filled from key store
            total_requests: row.total_requests as u64,
            total_tokens: row.total_tokens as u64,
            total_cost_usd: row.total_cost_usd,
            avg_latency_ms: row.avg_latency_ms,
            error_rate: if row.total_requests > 0 {
                row.errors as f64 / row.total_requests as f64
            } else {
                0.0
            },
            top_models: top_models
                .into_iter()
                .map(|m| ModelUsage {
                    model: m.model,
                    requests: m.requests as u64,
                    tokens: m.tokens as u64,
                    cost_usd: m.cost_usd,
                })
                .collect(),
            top_providers: top_providers
                .into_iter()
                .map(|p| ProviderUsage {
                    provider: p.provider,
                    requests: p.requests as u64,
                    tokens: p.tokens as u64,
                    cost_usd: p.cost_usd,
                })
                .collect(),
            recent_errors: vec![],
        })
    }
}

#[derive(sqlx::FromRow)]
struct UsageStatsRow {
    api_key_id: String,
    total_requests: i64,
    successful_requests: i64,
    failed_requests: i64,
    total_prompt_tokens: i64,
    total_completion_tokens: i64,
    total_tokens: i64,
    total_cost_usd: f64,
    avg_latency_ms: f64,
    last_request_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(sqlx::FromRow)]
struct DashboardRow {
    total_requests: i64,
    total_tokens: i64,
    total_cost_usd: f64,
    avg_latency_ms: f64,
    errors: i64,
}

#[derive(sqlx::FromRow)]
struct ModelUsageRow {
    model: String,
    requests: i64,
    tokens: i64,
    cost_usd: f64,
}

#[derive(sqlx::FromRow)]
struct ProviderUsageRow {
    provider: String,
    requests: i64,
    tokens: i64,
    cost_usd: f64,
}

// Re-export types from usage_store
use crate::usage_store::{DashboardSummary, ModelUsage, ProviderUsage, UsageStats};
