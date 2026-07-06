use sqlx::PgPool;

use crate::request_log::{
    MaintenanceQuery, RequestLogEntry, RequestLogListResult, RequestLogQuery, RequestLogStore,
};

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
