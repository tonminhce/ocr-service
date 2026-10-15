use std::{env, net::SocketAddr};

use anyhow::{Context, Result, ensure};
use ocr_core::{secrets, store::SubmitDeadlines};
use std::time::Duration;

#[derive(Clone)]
pub struct Settings {
    pub database_url: String,
    pub bind_addr: SocketAddr,
    pub api_token: String,
    /// Separate privileged credential. None disables all review endpoints.
    pub review_token: Option<String>,
    pub review_principal: Option<String>,
    pub max_upload_bytes: usize,
    pub upload_timeout_seconds: u64,
    pub upload_concurrency: usize,
    pub max_pending_jobs: i64,
    pub enqueue_deadlines: SubmitDeadlines,
    pub responses: ResponseSettings,
    pub transport: TransportSettings,
}

impl Settings {
    pub fn from_env() -> Result<Self> {
        let settings = Self {
            database_url: secrets::required("DATABASE_URL")?,
            bind_addr: env::var("API_BIND_ADDR")
                .unwrap_or_else(|_| "0.0.0.0:8080".into())
                .parse()
                .context("API_BIND_ADDR must be a socket address")?,
            api_token: secrets::required("OCR_API_TOKEN")?,
            review_token: secrets::optional("OCR_REVIEW_TOKEN")?,
            review_principal: env::var("OCR_REVIEW_PRINCIPAL")
                .ok()
                .filter(|value| !value.is_empty()),
            max_upload_bytes: number("MAX_UPLOAD_BYTES", 10 * 1024 * 1024)?,
            upload_timeout_seconds: number("UPLOAD_TIMEOUT_SECONDS", 30)?,
            upload_concurrency: number("UPLOAD_CONCURRENCY", 8)?,
            max_pending_jobs: number("MAX_PENDING_JOBS", 1000)?,
            enqueue_deadlines: SubmitDeadlines::new(
                Duration::from_millis(number("ENQUEUE_TIMEOUT_MS", 10000)?),
                Duration::from_millis(number("ENQUEUE_LOCK_TIMEOUT_MS", 2000)?),
                Duration::from_millis(number("ENQUEUE_STATEMENT_TIMEOUT_MS", 5000)?),
            )?,
            responses: ResponseSettings::from_env()?,
            transport: TransportSettings::from_env()?,
        };
        ensure!(
            settings.api_token.len() >= 16
                && settings.api_token.bytes().all(|b| b.is_ascii_graphic()),
            "OCR_API_TOKEN must contain at least 16 printable ASCII characters without spaces"
        );
        match (&settings.review_token, &settings.review_principal) {
            (None, None) => {}
            (Some(token), Some(principal)) => {
                ensure!(
                    token.len() >= 32
                        && token.bytes().all(|b| b.is_ascii_graphic())
                        && token != &settings.api_token,
                    "OCR_REVIEW_TOKEN must be a distinct token with at least 32 printable ASCII characters without spaces"
                );
                ensure!(
                    !principal.trim().is_empty()
                        && principal.len() <= 128
                        && !principal.chars().any(char::is_control),
                    "OCR_REVIEW_PRINCIPAL must contain 1..128 UTF-8 bytes without controls"
                );
            }
            _ => anyhow::bail!(
                "OCR_REVIEW_TOKEN and OCR_REVIEW_PRINCIPAL must be configured together"
            ),
        }
        ensure!(
            (1..=50 * 1024 * 1024).contains(&settings.max_upload_bytes),
            "MAX_UPLOAD_BYTES must be between 1 and 52428800"
        );
        ensure!(
            (1..=300).contains(&settings.upload_timeout_seconds),
            "UPLOAD_TIMEOUT_SECONDS must be between 1 and 300"
        );
        ensure!(
            (1..=128).contains(&settings.upload_concurrency),
            "UPLOAD_CONCURRENCY must be 1..=128"
        );
        ensure!(
            (1..=1_000_000).contains(&settings.max_pending_jobs),
            "MAX_PENDING_JOBS must be 1..=1000000"
        );
        Ok(settings)
    }
}

fn number<T: std::str::FromStr>(name: &str, default: T) -> Result<T> {
    match env::var(name) {
        Ok(value) => value.parse().map_err(|_| anyhow::anyhow!("invalid {name}")),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(error) => Err(error.into()),
    }
}

/// Independent small-polling, large-body, CPU and reserved output-byte budgets.
#[derive(Clone, Debug)]
pub struct ResponseSettings {
    pub small_concurrency: usize,
    pub large_concurrency: usize,
    pub serialization_concurrency: usize,
    pub max_small_bytes: usize,
    pub max_json_bytes: usize,
    pub max_source_bytes: usize,
    pub large_reserved_bytes: usize,
    pub body_timeout: Duration,
    pub read_timeout: Duration,
}

impl Default for ResponseSettings {
    fn default() -> Self {
        Self {
            small_concurrency: 32,
            large_concurrency: 8,
            serialization_concurrency: 4,
            max_small_bytes: 64 * 1024,
            max_json_bytes: 16 * 1024 * 1024,
            max_source_bytes: 50 * 1024 * 1024,
            large_reserved_bytes: 128 * 1024 * 1024,
            body_timeout: Duration::from_secs(30),
            read_timeout: Duration::from_secs(10),
        }
    }
}

impl ResponseSettings {
    fn from_env() -> Result<Self> {
        let defaults = Self::default();
        let settings = Self {
            small_concurrency: number("RESPONSE_SMALL_CONCURRENCY", defaults.small_concurrency)?,
            large_concurrency: number("RESPONSE_LARGE_CONCURRENCY", defaults.large_concurrency)?,
            serialization_concurrency: number(
                "RESPONSE_SERIALIZATION_CONCURRENCY",
                defaults.serialization_concurrency,
            )?,
            max_small_bytes: number("RESPONSE_MAX_SMALL_BYTES", defaults.max_small_bytes)?,
            max_json_bytes: number("RESPONSE_MAX_JSON_BYTES", defaults.max_json_bytes)?,
            max_source_bytes: number("RESPONSE_MAX_SOURCE_BYTES", defaults.max_source_bytes)?,
            large_reserved_bytes: number(
                "RESPONSE_LARGE_RESERVED_BYTES",
                defaults.large_reserved_bytes,
            )?,
            body_timeout: Duration::from_secs(number("RESPONSE_BODY_TIMEOUT_SECONDS", 30)?),
            read_timeout: Duration::from_secs(number("RESPONSE_READ_TIMEOUT_SECONDS", 10)?),
        };
        ensure!(
            (1..=128).contains(&settings.small_concurrency),
            "RESPONSE_SMALL_CONCURRENCY must be 1..=128"
        );
        ensure!(
            (1..=32).contains(&settings.large_concurrency),
            "RESPONSE_LARGE_CONCURRENCY must be 1..=32"
        );
        ensure!(
            (1..=32).contains(&settings.serialization_concurrency),
            "RESPONSE_SERIALIZATION_CONCURRENCY must be 1..=32"
        );
        ensure!(
            (1024..=256 * 1024).contains(&settings.max_small_bytes),
            "RESPONSE_MAX_SMALL_BYTES must be 1024..=262144"
        );
        ensure!(
            (1024..=64 * 1024 * 1024).contains(&settings.max_json_bytes),
            "RESPONSE_MAX_JSON_BYTES must be 1024..=67108864"
        );
        ensure!(
            (1024..=50 * 1024 * 1024).contains(&settings.max_source_bytes),
            "RESPONSE_MAX_SOURCE_BYTES must be 1024..=52428800"
        );
        ensure!(
            (settings.max_json_bytes.max(settings.max_source_bytes)..=512 * 1024 * 1024)
                .contains(&settings.large_reserved_bytes),
            "RESPONSE_LARGE_RESERVED_BYTES must cover the largest response and be <=536870912"
        );
        ensure!(
            (1..=300).contains(&settings.body_timeout.as_secs()),
            "RESPONSE_BODY_TIMEOUT_SECONDS must be 1..=300"
        );
        ensure!(
            (1..=60).contains(&settings.read_timeout.as_secs()),
            "RESPONSE_READ_TIMEOUT_SECONDS must be 1..=60"
        );
        Ok(settings)
    }
}

/// HTTP/1 only: no unbounded per-connection HTTP/2 stream multiplexing.
#[derive(Clone, Debug)]
pub struct TransportSettings {
    pub max_connections: usize,
    pub write_buffer_bytes: usize,
    pub header_timeout: Duration,
    pub write_stall_timeout: Duration,
    pub connection_lifetime: Duration,
}

impl Default for TransportSettings {
    fn default() -> Self {
        Self {
            max_connections: 128,
            write_buffer_bytes: 64 * 1024,
            header_timeout: Duration::from_secs(10),
            write_stall_timeout: Duration::from_secs(10),
            connection_lifetime: Duration::from_secs(120),
        }
    }
}

impl TransportSettings {
    fn from_env() -> Result<Self> {
        let settings = Self {
            max_connections: number("HTTP_MAX_CONNECTIONS", 128)?,
            write_buffer_bytes: number("HTTP_WRITE_BUFFER_BYTES", 64 * 1024)?,
            header_timeout: Duration::from_secs(number("HTTP_HEADER_TIMEOUT_SECONDS", 10)?),
            write_stall_timeout: Duration::from_secs(number(
                "HTTP_WRITE_STALL_TIMEOUT_SECONDS",
                10,
            )?),
            connection_lifetime: Duration::from_secs(number(
                "HTTP_CONNECTION_LIFETIME_SECONDS",
                120,
            )?),
        };
        ensure!(
            (8..=1024).contains(&settings.max_connections),
            "HTTP_MAX_CONNECTIONS must be 8..=1024"
        );
        ensure!(
            (8192..=256 * 1024).contains(&settings.write_buffer_bytes),
            "HTTP_WRITE_BUFFER_BYTES must be 8192..=262144"
        );
        ensure!(
            (1..=60).contains(&settings.header_timeout.as_secs()),
            "HTTP_HEADER_TIMEOUT_SECONDS must be 1..=60"
        );
        ensure!(
            (1..=60).contains(&settings.write_stall_timeout.as_secs()),
            "HTTP_WRITE_STALL_TIMEOUT_SECONDS must be 1..=60"
        );
        ensure!(
            (1..=600).contains(&settings.connection_lifetime.as_secs()),
            "HTTP_CONNECTION_LIFETIME_SECONDS must be 1..=600"
        );
        Ok(settings)
    }
}
