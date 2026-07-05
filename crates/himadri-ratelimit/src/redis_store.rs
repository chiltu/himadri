/// Distributed rate limiter using Redis sliding window.
/// Falls back to in-memory when Redis is unavailable.
pub struct RedisRateLimiter {
    client: Option<redis::Client>,
    fallback: crate::ShardedRateLimiter,
    prefix: String,
    /// Requests allowed per 1-second window.
    rate: u64,
}

impl RedisRateLimiter {
    pub async fn new(redis_url: Option<&str>, rate: u64, capacity: u64) -> Self {
        let client = if let Some(url) = redis_url {
            match redis::Client::open(url) {
                Ok(client) => {
                    // Test connection
                    match client.get_multiplexed_async_connection().await {
                        Ok(_) => {
                            tracing::info!("Connected to Redis for rate limiting");
                            Some(client)
                        }
                        Err(e) => {
                            tracing::warn!(
                                "Failed to connect to Redis: {}, using in-memory fallback",
                                e
                            );
                            None
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("Invalid Redis URL: {}, using in-memory fallback", e);
                    None
                }
            }
        } else {
            tracing::info!("No REDIS_URL set, using in-memory rate limiter");
            None
        };

        Self {
            client,
            fallback: crate::ShardedRateLimiter::new(rate, capacity, 64),
            prefix: "himadri:ratelimit:".to_string(),
            rate,
        }
    }

    /// Check if a request is allowed for the given key.
    pub async fn allow(&self, key: &str) -> bool {
        if let Some(client) = &self.client {
            match self.allow_redis(client, key).await {
                Ok(allowed) => allowed,
                Err(e) => {
                    tracing::warn!("Redis rate limit error: {}, falling back to in-memory", e);
                    self.fallback.allow(key)
                }
            }
        } else {
            self.fallback.allow(key)
        }
    }

    async fn allow_redis(
        &self,
        client: &redis::Client,
        key: &str,
    ) -> Result<bool, redis::RedisError> {
        let mut conn = client.get_multiplexed_async_connection().await?;
        let redis_key = format!("{}{}", self.prefix, key);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        // Sliding window: remove old entries, count current, add new
        let window_start = now.saturating_sub(1000); // 1 second window

        // Remove expired entries
        redis::cmd("ZREMRANGEBYSCORE")
            .arg(&redis_key)
            .arg(0)
            .arg(window_start)
            .query_async::<_, ()>(&mut conn)
            .await?;

        // Count current entries
        let count: u64 = redis::cmd("ZCARD")
            .arg(&redis_key)
            .query_async(&mut conn)
            .await?;

        if count >= self.rate {
            return Ok(false);
        }

        // Add new entry
        redis::cmd("ZADD")
            .arg(&redis_key)
            .arg(now)
            .arg(format!("{}:{}", now, uuid::Uuid::new_v4()))
            .query_async::<_, ()>(&mut conn)
            .await?;

        // Set TTL
        redis::cmd("EXPIRE")
            .arg(&redis_key)
            .arg(2) // 2 seconds TTL
            .query_async::<_, ()>(&mut conn)
            .await?;

        Ok(true)
    }
}
