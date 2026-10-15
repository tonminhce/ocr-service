use std::{env, fmt::Display, path::PathBuf, str::FromStr};

use anyhow::{Context, Result, bail};
use ocr_core::secrets;

/// Environment configuration. Deliberately does not implement Debug, since it
/// contains the upstream inference API key and database connection string.
#[derive(Clone)]
pub struct Settings {
    pub database_url: String,
    pub redis_url: String,
    pub redis_stream: String,
    pub redis_consumer_group: String,
    pub redis_timeout_ms: u64,
    pub redis_redispatch_seconds: i64,
    pub vllm_base_url: String,
    pub vllm_model: String,
    pub vllm_api_key: Option<String>,
    pub worker_concurrency: usize,
    pub lease_seconds: i64,
    pub max_attempts: i32,
    pub poll_interval_ms: u64,
    pub request_timeout_seconds: u64,
    pub job_timeout_seconds: u64,
    pub max_pages: u32,
    pub max_image_pixels: u64,
    pub max_image_edge: u32,
    pub table_crop_padding_pixels: u32,
    pub pdf_render_dpi: u32,
    pub page_max_edge: u32,
    pub strategy: OcrStrategy,
    pub layout_binary: PathBuf,
    pub layout_model_path: PathBuf,
    pub layout_model_sha256: String,
    pub layout_timeout_seconds: u64,
    pub layout_pool_size: usize,
    pub max_regions_per_page: usize,
    pub max_regions_per_document: usize,
    pub region_concurrency: usize,
    pub inference_revision: String,
    pub inference_deployment: Option<crate::telemetry::DeploymentDeclaration>,
    pub checkpoint_enabled: bool,
    pub recovery_max_regions: usize,
    pub recovery_expansion_pixels: u32,
    pub recognize_image_regions: bool,
    pub pdf_render_timeout_seconds: u64,
    pub max_output_tokens: u32,
    pub retention_days: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OcrStrategy {
    CourseLayout,
    FullPage,
}

impl Settings {
    pub fn from_env() -> Result<Self> {
        let settings = Self {
            database_url: secrets::required("DATABASE_URL")?,
            redis_url: secrets::with_default("REDIS_URL", "redis://localhost:6379")?,
            redis_stream: env::var("REDIS_STREAM").unwrap_or_else(|_| "ocr:jobs".into()),
            redis_consumer_group: env::var("REDIS_CONSUMER_GROUP")
                .unwrap_or_else(|_| "ocr-workers".into()),
            redis_timeout_ms: env_number("REDIS_TIMEOUT_MS", 2000)?,
            redis_redispatch_seconds: env_number("REDIS_REDISPATCH_SECONDS", 30)?,
            vllm_base_url: env::var("VLLM_BASE_URL")
                .unwrap_or_else(|_| "http://localhost:8000/v1".into()),
            vllm_model: env::var("VLLM_MODEL").unwrap_or_else(|_| "Qwen/Qwen3.5-4B".into()),
            vllm_api_key: secrets::optional("VLLM_API_KEY")?,
            worker_concurrency: env_number("WORKER_CONCURRENCY", 2)?,
            lease_seconds: env_number("LEASE_SECONDS", 60)?,
            max_attempts: env_number("MAX_ATTEMPTS", 3)?,
            poll_interval_ms: env_number("POLL_INTERVAL_MS", 500)?,
            request_timeout_seconds: env_number("REQUEST_TIMEOUT_SECONDS", 120)?,
            job_timeout_seconds: env_number("JOB_TIMEOUT_SECONDS", 600)?,
            max_pages: env_number("MAX_PAGES", 20)?,
            max_image_pixels: env_number("MAX_IMAGE_PIXELS", 40_000_000)?,
            max_image_edge: env_number("MAX_IMAGE_EDGE", 2000)?,
            table_crop_padding_pixels: env_number("TABLE_CROP_PADDING_PIXELS", 0)?,
            pdf_render_dpi: env_number("PDF_RENDER_DPI", 200)?,
            page_max_edge: env_number("PAGE_MAX_EDGE", 3500)?,
            strategy: match env::var("OCR_STRATEGY")
                .unwrap_or_else(|_| "course_layout_vlm".into())
                .as_str()
            {
                "course_layout_vlm" => OcrStrategy::CourseLayout,
                "full_page_vlm" => OcrStrategy::FullPage,
                _ => bail!("OCR_STRATEGY must be course_layout_vlm or full_page_vlm"),
            },
            layout_binary: env::var("LAYOUT_BINARY")
                .unwrap_or_else(|_| "layout-detect".into())
                .into(),
            layout_model_path: env::var("LAYOUT_MODEL_PATH")
                .unwrap_or_else(|_| "/models/layout/model.onnx".into())
                .into(),
            layout_model_sha256: env::var("LAYOUT_MODEL_SHA256").unwrap_or_default(),
            layout_timeout_seconds: env_number("LAYOUT_TIMEOUT_SECONDS", 60)?,
            layout_pool_size: env_number("LAYOUT_POOL_SIZE", 1)?,
            max_regions_per_page: env_number("MAX_REGIONS_PER_PAGE", 128)?,
            max_regions_per_document: env_number("MAX_REGIONS_PER_DOCUMENT", 2048)?,
            region_concurrency: env_number("REGION_CONCURRENCY", 4)?,
            inference_revision: env::var("INFERENCE_REVISION").unwrap_or_default(),
            inference_deployment: match env::var("INFERENCE_DEPLOYMENT_JSON") {
                Ok(value) if value.trim().is_empty() => None,
                Ok(value) => Some(crate::telemetry::DeploymentDeclaration::parse(&value)?),
                Err(env::VarError::NotPresent) => None,
                Err(_) => bail!("INFERENCE_DEPLOYMENT_JSON must be valid Unicode"),
            },
            checkpoint_enabled: env::var("REGION_CHECKPOINTS_ENABLED")
                .unwrap_or_else(|_| "false".into())
                .parse()
                .context("REGION_CHECKPOINTS_ENABLED must be true or false")?,
            recovery_max_regions: env_number("REGION_RECOVERY_MAX_REGIONS", 0)?,
            recovery_expansion_pixels: env_number("REGION_RECOVERY_EXPANSION_PIXELS", 16)?,
            recognize_image_regions: env::var("OCR_RECOGNIZE_IMAGES")
                .unwrap_or_else(|_| "false".into())
                .parse()
                .context("OCR_RECOGNIZE_IMAGES must be true or false")?,
            pdf_render_timeout_seconds: env_number("PDF_RENDER_TIMEOUT_SECONDS", 30)?,
            max_output_tokens: env_number("MAX_OUTPUT_TOKENS", 8192)?,
            retention_days: env_number("RETENTION_DAYS", 7)?,
        };
        settings.validate()?;
        Ok(settings)
    }

    pub fn validate(&self) -> Result<()> {
        if let Some(deployment) = &self.inference_deployment {
            deployment.validate()?;
        }
        // Redis parser errors may contain the URL, including its password.
        redis::Client::open(self.redis_url.as_str())
            .map_err(|_| anyhow::anyhow!("REDIS_URL must be a valid Redis connection URL"))?;
        if self.redis_stream.is_empty() || self.redis_consumer_group.is_empty() {
            bail!("REDIS_STREAM and REDIS_CONSUMER_GROUP cannot be empty");
        }
        if !(50..=30_000).contains(&self.redis_timeout_ms)
            || !(1..=3600).contains(&self.redis_redispatch_seconds)
        {
            bail!(
                "REDIS_TIMEOUT_MS must be 50..=30000 and REDIS_REDISPATCH_SECONDS must be 1..=3600"
            );
        }
        let base = reqwest::Url::parse(&self.vllm_base_url)
            .context("VLLM_BASE_URL must be an absolute URL")?;
        if !matches!(base.scheme(), "http" | "https") || base.host_str().is_none() {
            bail!("VLLM_BASE_URL must use http or https and include a host");
        }
        if !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            bail!("VLLM_BASE_URL cannot contain credentials, a query, or a fragment");
        }
        if self.database_url.is_empty() || self.vllm_model.trim().is_empty() {
            bail!("DATABASE_URL and VLLM_MODEL cannot be empty");
        }
        if !(1..=64).contains(&self.worker_concurrency) {
            bail!("WORKER_CONCURRENCY must be between 1 and 64");
        }
        if !(1..=8).contains(&self.layout_pool_size)
            || self.layout_pool_size > self.worker_concurrency
        {
            bail!("LAYOUT_POOL_SIZE must be 1..=8 and cannot exceed WORKER_CONCURRENCY");
        }
        if self.inference_revision.len() > 128
            || !self.inference_revision.is_ascii()
            || self.inference_revision.chars().any(char::is_control)
            || (self.checkpoint_enabled && self.inference_revision.trim().is_empty())
        {
            bail!(
                "INFERENCE_REVISION must be bounded ASCII and is required for region checkpoints"
            );
        }
        if self.recovery_max_regions > 16 || !(1..=128).contains(&self.recovery_expansion_pixels) {
            bail!(
                "REGION_RECOVERY_MAX_REGIONS must be 0..=16 and REGION_RECOVERY_EXPANSION_PIXELS must be 1..=128"
            );
        }
        if !(3..=3600).contains(&self.lease_seconds) || !(1..=20).contains(&self.max_attempts) {
            bail!("LEASE_SECONDS must be 3..=3600 and MAX_ATTEMPTS must be 1..=20");
        }
        if !(10..=60_000).contains(&self.poll_interval_ms) {
            bail!("POLL_INTERVAL_MS must be 10..=60000");
        }
        if self.request_timeout_seconds == 0
            || self.request_timeout_seconds > 3600
            || self.job_timeout_seconds == 0
            || self.job_timeout_seconds > 86_400
            || self.pdf_render_timeout_seconds == 0
            || self.pdf_render_timeout_seconds > 600
        {
            bail!("request, job, and PDF timeouts must be positive and bounded");
        }
        if !(1..=1000).contains(&self.max_pages)
            || !(1..=100_000_000).contains(&self.max_image_pixels)
            || !(32..=8192).contains(&self.max_image_edge)
            || !(1..=65_536).contains(&self.max_output_tokens)
            || !(1..=3650).contains(&self.retention_days)
        {
            bail!("page, image, token, or retention limits are outside the supported range");
        }
        if !(36..=600).contains(&self.pdf_render_dpi)
            || !(32..=8192).contains(&self.page_max_edge)
            || !(1..=600).contains(&self.layout_timeout_seconds)
            || !(1..=300).contains(&self.max_regions_per_page)
            || !(1..=10_000).contains(&self.max_regions_per_document)
            || !(1..=64).contains(&self.region_concurrency)
        {
            bail!("layout/page/region limits are outside the supported range");
        }
        if self.strategy == OcrStrategy::CourseLayout && self.max_image_edge < 112 {
            bail!("course layout requires MAX_IMAGE_EDGE >= 112 for the SDK minimum crop area");
        }
        if self.table_crop_padding_pixels > 512
            || self
                .max_image_edge
                .saturating_add(2 * self.table_crop_padding_pixels)
                > 8192
        {
            bail!("TABLE_CROP_PADDING_PIXELS must be 0..=512 and padded image edge must be <=8192");
        }
        if self.strategy == OcrStrategy::CourseLayout
            && (self.layout_model_sha256.len() != 64
                || !self
                    .layout_model_sha256
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit())
                || self.layout_binary.as_os_str().is_empty()
                || self.layout_model_path.as_os_str().is_empty())
        {
            bail!(
                "course layout requires a detector binary, model path and pinned LAYOUT_MODEL_SHA256"
            );
        }
        Ok(())
    }
}

fn env_number<T>(key: &str, default: T) -> Result<T>
where
    T: FromStr,
    T::Err: Display,
{
    match env::var(key) {
        Ok(value) => value
            .parse()
            .map_err(|e| anyhow::anyhow!("{key} must be a number: {e}")),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(e) => Err(e).with_context(|| format!("cannot read {key}")),
    }
}
