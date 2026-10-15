//! Fixed-size per-attempt telemetry. No crop/text/IDs/URLs are retained here.
use std::{
    io::Cursor,
    sync::{Arc, Mutex, MutexGuard},
};

use ocr_core::metrics::{Event, ModelFinish, ModelUsage, process_metrics};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Default, Serialize)]
pub struct ScalarSummary {
    pub observations: u64,
    pub sum: u64,
    pub min: Option<u64>,
    pub max: Option<u64>,
}
impl ScalarSummary {
    fn observe(&mut self, value: u64) {
        self.observations = self.observations.saturating_add(1);
        self.sum = self.sum.saturating_add(value);
        self.min = Some(self.min.map_or(value, |old| old.min(value)));
        self.max = Some(self.max.map_or(value, |old| old.max(value)));
    }
}

#[derive(Clone, Default, Serialize)]
pub struct InputSummary {
    pub encoded_jpeg_bytes: ScalarSummary,
    pub base64_payload_bytes: ScalarSummary,
    pub width_pixels: ScalarSummary,
    pub height_pixels: ScalarSummary,
    pub pixels: ScalarSummary,
    pub dimensions_unavailable: u64,
}

#[derive(Clone, Default, Serialize)]
pub struct UsageSummary {
    pub present_responses: u64,
    pub missing_responses: u64,
    pub invalid_responses: u64,
    pub prompt_tokens: ScalarSummary,
    pub completion_tokens: ScalarSummary,
    pub total_tokens: ScalarSummary,
}

#[derive(Clone, Serialize)]
pub struct RunTelemetry {
    pub schema_version: &'static str,
    pub attempts_started: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub cancelled: u64,
    pub checkpoint_hits: u64,
    pub checkpoint_misses: u64,
    /// Order is fixed: full_page, text (including image OCR), table, formula.
    pub inputs_by_task: [InputSummary; 4],
    pub task_order: [&'static str; 4],
    pub finish_reasons: [u64; ModelFinish::COUNT],
    pub finish_reason_order: [&'static str; ModelFinish::COUNT],
    pub usage: UsageSummary,
    /// Non-streaming completions cannot measure time-to-first-token.
    pub ttft_seconds: Option<f64>,
}
impl Default for RunTelemetry {
    fn default() -> Self {
        Self {
            schema_version: "ocr-inference-telemetry/1.0.0",
            attempts_started: 0,
            succeeded: 0,
            failed: 0,
            cancelled: 0,
            checkpoint_hits: 0,
            checkpoint_misses: 0,
            inputs_by_task: std::array::from_fn(|_| InputSummary::default()),
            task_order: ["full_page", "text", "table", "formula"],
            finish_reasons: [0; ModelFinish::COUNT],
            finish_reason_order: std::array::from_fn(|i| ModelFinish::ALL[i].as_str()),
            usage: UsageSummary::default(),
            ttft_seconds: None,
        }
    }
}
pub type Recorder = Arc<Mutex<RunTelemetry>>;
pub fn lock(recorder: &Recorder) -> MutexGuard<'_, RunTelemetry> {
    // A panic in instrumentation must not poison the OCR/retry path.
    recorder.lock().unwrap_or_else(|error| error.into_inner())
}

pub struct CallTrace {
    recorder: Recorder,
    finished: bool,
}
impl CallTrace {
    /// Begins after acquiring shared R. Attempts queued for R are not calls.
    pub fn start(recorder: Recorder, jpeg: &[u8], task_index: usize) -> Self {
        {
            let mut run = lock(&recorder);
            run.attempts_started = run.attempts_started.saturating_add(1);
            let input = &mut run.inputs_by_task[task_index];
            input.encoded_jpeg_bytes.observe(jpeg.len() as u64);
            input
                .base64_payload_bytes
                .observe((jpeg.len() as u64).div_ceil(3).saturating_mul(4));
            match image::ImageReader::with_format(Cursor::new(jpeg), image::ImageFormat::Jpeg)
                .into_dimensions()
            {
                Ok((width, height)) => {
                    input.width_pixels.observe(u64::from(width));
                    input.height_pixels.observe(u64::from(height));
                    input.pixels.observe(u64::from(width) * u64::from(height));
                }
                Err(_) => {
                    input.dimensions_unavailable = input.dimensions_unavailable.saturating_add(1)
                }
            }
        }
        process_metrics().event(Event::ModelAttemptStarted);
        Self {
            recorder,
            finished: false,
        }
    }
    pub fn metadata(&self, finish: ModelFinish, usage: &ParsedUsage) {
        let mut run = lock(&self.recorder);
        let index = finish as usize;
        run.finish_reasons[index] = run.finish_reasons[index].saturating_add(1);
        let summary = &mut run.usage;
        match usage.state {
            ModelUsage::Missing => {
                summary.missing_responses = summary.missing_responses.saturating_add(1)
            }
            ModelUsage::Invalid => {
                summary.invalid_responses = summary.invalid_responses.saturating_add(1)
            }
            ModelUsage::Present => {
                summary.present_responses = summary.present_responses.saturating_add(1);
                for (target, value) in [
                    &mut summary.prompt_tokens,
                    &mut summary.completion_tokens,
                    &mut summary.total_tokens,
                ]
                .into_iter()
                .zip(usage.tokens)
                {
                    if let Some(value) = value {
                        target.observe(value);
                    }
                }
            }
        }
        process_metrics().model_metadata(finish, usage.state, usage.tokens);
    }
    pub fn finish(mut self, succeeded: bool) {
        let mut run = lock(&self.recorder);
        let target = if succeeded {
            &mut run.succeeded
        } else {
            &mut run.failed
        };
        *target = target.saturating_add(1);
        self.finished = true;
    }
}
impl Drop for CallTrace {
    fn drop(&mut self) {
        if !self.finished {
            let mut run = lock(&self.recorder);
            run.cancelled = run.cancelled.saturating_add(1);
            process_metrics().event(Event::ModelCancelled);
        }
    }
}

pub struct ParsedUsage {
    pub state: ModelUsage,
    pub tokens: [Option<u64>; 3],
}
impl ParsedUsage {
    /// Accept partial genuine counts. Reject negative/fraction/string/overflow
    /// and inconsistent totals; never estimate missing components.
    pub fn parse(value: Option<&Value>) -> Self {
        let missing = Self {
            state: ModelUsage::Missing,
            tokens: [None; 3],
        };
        let invalid = || Self {
            state: ModelUsage::Invalid,
            tokens: [None; 3],
        };
        let Some(value) = value.filter(|v| !v.is_null()) else {
            return missing;
        };
        let Some(object) = value.as_object() else {
            return invalid();
        };
        let mut tokens = [None; 3];
        for (index, name) in ["prompt_tokens", "completion_tokens", "total_tokens"]
            .into_iter()
            .enumerate()
        {
            if let Some(value) = object.get(name) {
                let Some(count) = value.as_u64() else {
                    return invalid();
                };
                tokens[index] = Some(count);
            }
        }
        if tokens.iter().all(Option::is_none) {
            return missing;
        }
        if let Some(total) = tokens[2] {
            // Even partial usage cannot report a component greater than total.
            if tokens[..2].iter().flatten().any(|count| *count > total) {
                return invalid();
            }
        }
        if let [Some(prompt), Some(completion), Some(total)] = tokens
            && prompt.checked_add(completion) != Some(total)
        {
            return invalid();
        }
        Self {
            state: ModelUsage::Present,
            tokens,
        }
    }
}

pub fn finish_reason(value: Option<&str>) -> ModelFinish {
    match value {
        Some("stop") => ModelFinish::Stop,
        Some("length") => ModelFinish::Length,
        Some("content_filter") => ModelFinish::ContentFilter,
        Some("tool_calls") => ModelFinish::ToolCalls,
        Some("function_call") => ModelFinish::FunctionCall,
        Some(_) => ModelFinish::Other,
        None => ModelFinish::Missing,
    }
}

/// Optional operator declaration, never scheduler/weights attestation. Reject
/// unknown fields and never echo input in errors. No URLs or credentials needed.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeploymentDeclaration {
    pub runtime: Option<String>,
    pub runtime_version: Option<String>,
    pub weights_revision: Option<String>,
    pub weights_sha256: Option<String>,
    pub tokenizer_revision: Option<String>,
    pub quantization: Option<String>,
    pub image_digest: Option<String>,
    pub max_sequences: Option<u32>,
    pub max_pixels: Option<u64>,
    pub max_model_len: Option<u64>,
}
impl DeploymentDeclaration {
    pub fn parse(input: &str) -> anyhow::Result<Self> {
        anyhow::ensure!(
            input.len() <= 8192,
            "INFERENCE_DEPLOYMENT_JSON exceeds 8192 bytes"
        );
        let declaration: Self = serde_json::from_str(input).map_err(|_| {
            anyhow::anyhow!(
                "INFERENCE_DEPLOYMENT_JSON must match the deployment declaration schema"
            )
        })?;
        declaration.validate()?;
        Ok(declaration)
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        for value in [
            &self.runtime,
            &self.runtime_version,
            &self.weights_revision,
            &self.weights_sha256,
            &self.tokenizer_revision,
            &self.quantization,
            &self.image_digest,
        ]
        .into_iter()
        .flatten()
        {
            anyhow::ensure!(
                !value.is_empty()
                    && value.len() <= 128
                    && value
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"._:-+/".contains(&c)),
                "INFERENCE_DEPLOYMENT_JSON identifiers must contain 1..128 safe ASCII characters"
            );
        }
        if let Some(hash) = &self.weights_sha256 {
            anyhow::ensure!(
                hash.len() == 64 && hash.bytes().all(|c| c.is_ascii_hexdigit()),
                "INFERENCE_DEPLOYMENT_JSON weights_sha256 must be a SHA-256 hex digest"
            );
        }
        anyhow::ensure!(
            self.max_sequences.is_none_or(|v| (1..=1024).contains(&v))
                && self
                    .max_pixels
                    .is_none_or(|v| (1..=1_000_000_000).contains(&v))
                && self
                    .max_model_len
                    .is_none_or(|v| (1..=10_000_000).contains(&v)),
            "INFERENCE_DEPLOYMENT_JSON runtime limits are out of range"
        );
        Ok(())
    }
}
