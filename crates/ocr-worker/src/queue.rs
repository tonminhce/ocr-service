//! Redis holds disposable job notifications; PostgreSQL holds jobs and their
//! recurring outbox records. Use one dedicated consumer group per stream:
//! acknowledged entries are deleted, never trimmed while pending.
use std::{sync::Arc, time::Duration};

use redis::{
    AsyncConnectionConfig, FromRedisValue,
    aio::MultiplexedConnection,
    streams::{StreamAutoClaimReply, StreamId, StreamReadReply},
};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::settings::Settings;

/// Deliberately contains no Redis error details, connection URLs, or credentials.
#[derive(Debug, thiserror::Error)]
pub enum QueueError {
    #[error("invalid Redis configuration")]
    Configuration,
    #[error("Redis connection unavailable")]
    Unavailable,
    #[error("Redis operation timed out")]
    Timeout,
    #[error("Redis command failed")]
    Command,
    #[error("Redis consumer group already exists")]
    GroupExists,
    #[error("Redis consumer group is missing")]
    NoGroup,
}

#[derive(Clone)]
pub struct RedisQueue {
    client: redis::Client,
    connection: Arc<Mutex<Option<MultiplexedConnection>>>,
    stream: String,
    group: String,
    consumer: String,
    timeout: Duration,
}

impl RedisQueue {
    pub fn new(settings: &Settings) -> Result<Self, QueueError> {
        Ok(Self {
            client: redis::Client::open(settings.redis_url.as_str())
                .map_err(|_| QueueError::Configuration)?,
            connection: Arc::default(),
            stream: settings.redis_stream.clone(),
            group: settings.redis_consumer_group.clone(),
            consumer: Uuid::new_v4().to_string(),
            timeout: Duration::from_millis(settings.redis_timeout_ms),
        })
    }

    async fn connection(&self) -> Result<MultiplexedConnection, QueueError> {
        // Bound both waiting for another reconnect and establishing a connection.
        tokio::time::timeout(self.timeout, async {
            let mut cached = self.connection.lock().await;
            if let Some(connection) = cached.as_ref() {
                return Ok(connection.clone());
            }
            let config = AsyncConnectionConfig::new()
                .set_connection_timeout(self.timeout)
                .set_response_timeout(self.timeout);
            let connection = self
                .client
                .get_multiplexed_async_connection_with_config(&config)
                .await
                .map_err(|_| QueueError::Unavailable)?;
            *cached = Some(connection.clone());
            Ok(connection)
        })
        .await
        .map_err(|_| QueueError::Timeout)?
    }

    async fn query<T: FromRedisValue>(&self, command: &redis::Cmd) -> Result<T, QueueError> {
        let mut connection = self.connection().await?;
        let result = tokio::time::timeout(self.timeout, command.query_async(&mut connection)).await;
        match result {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) if error.code() == Some("BUSYGROUP") => Err(QueueError::GroupExists),
            Ok(Err(error)) if error.code() == Some("NOGROUP") => Err(QueueError::NoGroup),
            error => {
                // A timed-out command can have taken effect. The outbox and
                // fenced SQL claims make a later duplicate safe.
                *self.connection.lock().await = None;
                if error.is_err() {
                    Err(QueueError::Timeout)
                } else {
                    Err(QueueError::Command)
                }
            }
        }
    }

    async fn query_group<T: FromRedisValue>(&self, command: &redis::Cmd) -> Result<T, QueueError> {
        match self.query(command).await {
            Err(QueueError::NoGroup) => {
                // XGROUP CREATE is DENYOOM even when the group already exists.
                // Initialize only after NOGROUP so existing consumers can drain
                // and delete notifications when Redis reaches its memory cap.
                self.ensure_group().await?;
                self.query(command).await
            }
            result => result,
        }
    }

    pub async fn ensure_group(&self) -> Result<(), QueueError> {
        let result: Result<String, _> = self
            .query(
                redis::cmd("XGROUP")
                    .arg("CREATE")
                    .arg(&self.stream)
                    .arg(&self.group)
                    .arg("0")
                    .arg("MKSTREAM"),
            )
            .await;
        match result {
            Ok(_) | Err(QueueError::GroupExists) => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Notifications contain only a job ID. Original bytes and result payloads
    /// never enter Redis. No MAXLEN trim can remove a pending notification.
    pub async fn publish(&self, job_id: Uuid) -> Result<String, QueueError> {
        self.query(
            redis::cmd("XADD")
                .arg(&self.stream)
                .arg("*")
                .arg("job_id")
                .arg(job_id.to_string()),
        )
        .await
    }

    pub async fn read(&self, count: usize) -> Result<Vec<StreamId>, QueueError> {
        let reply: StreamReadReply = self
            .query_group(
                redis::cmd("XREADGROUP")
                    .arg("GROUP")
                    .arg(&self.group)
                    .arg(&self.consumer)
                    .arg("COUNT")
                    .arg(count.clamp(1, 64))
                    .arg("STREAMS")
                    .arg(&self.stream)
                    .arg(">"),
            )
            .await?;
        Ok(reply.keys.into_iter().flat_map(|key| key.ids).collect())
    }

    /// Cursor scans are bounded. Reclaim only after the SQL lease duration;
    /// claim_by_id still enforces the current lease and prevents double work.
    pub async fn reclaim(
        &self,
        cursor: &str,
        lease_seconds: i64,
        count: usize,
    ) -> Result<StreamAutoClaimReply, QueueError> {
        self.query_group(
            redis::cmd("XAUTOCLAIM")
                .arg(&self.stream)
                .arg(&self.group)
                .arg(&self.consumer)
                .arg(lease_seconds.max(1).saturating_mul(1000))
                .arg(cursor)
                .arg("COUNT")
                .arg(count.clamp(1, 64)),
        )
        .await
    }

    /// Execute acknowledgment and deletion atomically. Retrying after a lost
    /// response is safe, and no crash can leave an acknowledged stream entry.
    pub async fn acknowledge(&self, message_id: &str) -> Result<(), QueueError> {
        let _: i64 = self
            .query(
                redis::cmd("EVAL")
                    .arg("redis.call('XACK', KEYS[1], ARGV[1], ARGV[2]); return redis.call('XDEL', KEYS[1], ARGV[2])")
                    .arg(1)
                    .arg(&self.stream)
                    .arg(&self.group)
                    .arg(message_id),
            )
            .await?;
        Ok(())
    }

    /// Graceful shutdown removes empty consumer metadata. Never delete a
    /// consumer that still owns pending work: a successor must reclaim it.
    pub async fn retire_consumer(&self) -> Result<(), QueueError> {
        let _: i64 = self
            .query(
                redis::cmd("EVAL")
                    .arg("if #redis.call('XPENDING', KEYS[1], ARGV[1], '-', '+', 1, ARGV[2]) == 0 then return redis.call('XGROUP', 'DELCONSUMER', KEYS[1], ARGV[1], ARGV[2]) end; return 0")
                    .arg(1)
                    .arg(&self.stream)
                    .arg(&self.group)
                    .arg(&self.consumer),
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unresponsive_redis_is_bounded_and_errors_do_not_expose_credentials() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });
        let queue = RedisQueue {
            client: redis::Client::open(format!(
                "redis://test-user:private-password@127.0.0.1:{port}"
            ))
            .unwrap(),
            connection: Arc::default(),
            stream: "isolated-timeout-test".into(),
            group: "isolated-timeout-test".into(),
            consumer: Uuid::new_v4().to_string(),
            timeout: Duration::from_millis(50),
        };
        let start = tokio::time::Instant::now();
        let error = queue.ensure_group().await.unwrap_err().to_string();
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(!error.contains("private-password"));
        assert!(!error.contains("test-user"));
        assert!(!error.contains("redis://"));
        server.abort();
    }
}
