//! Rust document preparation, OpenAI-compatible vLLM client, and leased worker.
//!
//! Course-layout processing uses PP-DocLayoutV3, learned reading order,
//! polygon crops, task-specific recognition and a ported Markdown formatter.

mod child_process;
mod extraction_validation;
pub mod formatting;
pub mod inference;
pub mod layout_runner;
pub mod post_processing;
pub mod preparation;
pub mod queue;
pub mod settings;
mod subprocess_policy;
pub mod telemetry;
pub mod worker;

use thiserror::Error;

#[derive(Debug, Error)]
#[error("{message}")]
pub struct ProcessingError {
    pub message: String,
    pub retryable: bool,
}

impl ProcessingError {
    pub fn permanent(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
        }
    }

    pub fn transient(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
        }
    }
}
