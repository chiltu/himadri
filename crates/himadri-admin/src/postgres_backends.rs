use himadri_core::Config;
use sqlx::PgPool;

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
        sqlx::migrate!("migrations/postgres")
            .run(&pool)
            .await
            .map_err(|e| sqlx::Error::Migrate(Box::new(e)))?;
        Ok(Self { pool })
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn save(&self, _config: &Config) -> Result<(), String> {
        Ok(())
    }

    pub async fn load(&self) -> Result<Option<Config>, String> {
        Ok(None)
    }

    pub async fn delete(&self) -> Result<(), String> {
        Ok(())
    }
}

/// Postgres-backed request log store.
///
/// `write` is called on the request hot path (including from the streaming
/// recorder's `Drop`), so inserts go through a bounded channel to a
/// background writer task instead of blocking the calling worker on a DB
/// round-trip. When the queue is full, entries are dropped (and counted)
/// rather than applying backpressure to requests.
pub struct PostgresRequestLogStore {
    pool: PgPool,
    sender: tokio::sync::mpsc::Sender<RequestLogEntry>,
    dropped: std::sync::atomic::AtomicU64,
}

const REQUEST_LOG_QUEUE_CAPACITY: usize = 8_192;

impl PostgresRequestLogStore {
    pub async fn new(database_url: &str) -> Result<Self, sqlx::Error> {
        let pool = sqlx::PgPool::connect(database_url).await?;
        sqlx::migrate!("migrations/postgres")
            .run(&pool)
            .await
            .map_err(|e| sqlx::Error::Migrate(Box::new(e)))?;

        let (sender, mut receiver) =
            tokio::sync::mpsc::channel::<RequestLogEntry>(REQUEST_LOG_QUEUE_CAPACITY);
        let writer_pool = pool.clone();
        tokio::spawn(async move {
            while let Some(entry) = receiver.recv().await {
                let result = sqlx::query(
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
                .execute(&writer_pool)
                .await;
                if let Err(e) = result {
                    tracing::warn!("Failed to persist request log entry: {}", e);
                }
            }
        });

        Ok(Self {
            pool,
            sender,
            dropped: std::sync::atomic::AtomicU64::new(0),
        })
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

impl RequestLogStore for PostgresRequestLogStore {
    fn write(&self, entry: RequestLogEntry) -> Result<(), String> {
        use std::sync::atomic::Ordering;
        if self.sender.try_send(entry).is_err() {
            let dropped = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            if dropped.is_power_of_two() {
                tracing::warn!(
                    "Request-log queue full; {} entr(ies) dropped so far",
                    dropped
                );
            }
            return Err("request log queue full".to_string());
        }
        Ok(())
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

                // Count query. Filter values are bound (never interpolated) in
                // the same order the `$N` placeholders were added above.
                let count_query = format!("SELECT COUNT(*) FROM request_logs {}", where_clause);
                let mut count_q = sqlx::query_scalar::<_, i64>(&count_query);
                if let Some(model) = &query.model {
                    count_q = count_q.bind(model.clone());
                }
                if let Some(provider) = &query.provider {
                    count_q = count_q.bind(provider.clone());
                }
                if let Some(since) = query.since {
                    count_q = count_q.bind(since);
                }
                let count: i64 = count_q.fetch_one(&pool).await.map_err(|e| e.to_string())?;

                // Data query with pagination. LIMIT/OFFSET are i64 (not
                // user strings), so formatting them in is injection-safe.
                let limit = query.limit.unwrap_or(100) as i64;
                let offset = query.offset.unwrap_or(0) as i64;
                let data_query = format!(
                    "SELECT trace_id, stage, model, provider, prompt_tokens, completion_tokens, total_tokens, error_message, created_at FROM request_logs {} ORDER BY created_at DESC LIMIT {} OFFSET {}",
                    where_clause, limit, offset
                );
                let mut data_q = sqlx::query_as::<_, RequestLogRow>(&data_query);
                if let Some(model) = &query.model {
                    data_q = data_q.bind(model.clone());
                }
                if let Some(provider) = &query.provider {
                    data_q = data_q.bind(provider.clone());
                }
                if let Some(since) = query.since {
                    data_q = data_q.bind(since);
                }
                let rows: Vec<RequestLogRow> =
                    data_q.fetch_all(&pool).await.map_err(|e| e.to_string())?;

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

                if query.model.is_some() {
                    conditions.push(format!("model = ${}", param_count));
                    param_count += 1;
                }
                if query.provider.is_some() {
                    conditions.push(format!("provider = ${}", param_count));
                    param_count += 1;
                }
                if query.before.is_some() {
                    conditions.push(format!("created_at < ${}", param_count));
                    param_count += 1;
                }
                let _ = param_count;

                let where_clause = if conditions.is_empty() {
                    String::new()
                } else {
                    format!("WHERE {}", conditions.join(" AND "))
                };

                // Filter values are bound in placeholder order. With no
                // filters this is an unconditional `DELETE` (delete-all), the
                // intended maintenance behavior.
                let delete_sql = format!("DELETE FROM request_logs {}", where_clause);
                let mut delete_q = sqlx::query(&delete_sql);
                if let Some(model) = &query.model {
                    delete_q = delete_q.bind(model.clone());
                }
                if let Some(provider) = &query.provider {
                    delete_q = delete_q.bind(provider.clone());
                }
                if let Some(before) = query.before {
                    delete_q = delete_q.bind(before);
                }
                let result = delete_q.execute(&pool).await.map_err(|e| e.to_string())?;

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
        sqlx::migrate!("migrations/postgres")
            .run(&pool)
            .await
            .map_err(|e| sqlx::Error::Migrate(Box::new(e)))?;
        Ok(Self { pool })
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
