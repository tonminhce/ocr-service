//! Rust document preparation, OpenAI-compatible vLLM client, and leased worker.
//!
//! Course-layout processing uses PP-DocLayoutV3, learned reading order,
//! polygon crops, task-specific recognition and a ported Markdown formatter.

mod child_process;
mod extraction_validation;
pub mod formatting;
pub mod inference;
mod json_budget;
pub mod layout_runner;
pub mod observer;
pub mod parser_client;
pub mod parser_protocol;
pub mod parser_service;
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

/// Canonical cancellation text used by the parser protocol and worker callers.
pub const CANCELLED_MESSAGE: &str = "job processing cancelled";

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

    /// A retryable job cancellation. The lease is fenced by the caller, so this
    /// only ever reports that local work stopped, never that a job failed.
    pub fn cancelled() -> Self {
        Self::transient(CANCELLED_MESSAGE)
    }
}

/// Cooperative cancellation check, shared by every stage that polls between
/// bounded chunks of work.
pub(crate) fn cancelled_error(
    cancelled: &tokio_util::sync::CancellationToken,
) -> Option<ProcessingError> {
    cancelled.is_cancelled().then(ProcessingError::cancelled)
}

/// Callers move CPU permits into blocking closures so waiter cancellation
/// cannot release admission while the underlying work is still running.
pub(crate) async fn permit_or_cancelled(
    slots: &std::sync::Arc<tokio::sync::Semaphore>,
    cancelled: &tokio_util::sync::CancellationToken,
    stopping: &'static str,
) -> Result<tokio::sync::OwnedSemaphorePermit, ProcessingError> {
    if let Some(error) = self::cancelled_error(cancelled) {
        return Err(error);
    }
    tokio::select! {
        biased;
        _ = cancelled.cancelled() => Err(ProcessingError::cancelled()),
        permit = slots.clone().acquire_owned() => permit.map_err(|_| ProcessingError::transient(stopping)),
    }
}

#[cfg(test)]
mod cancellation_tests {
    use std::sync::Arc;

    use tokio::sync::Semaphore;
    use tokio_util::sync::CancellationToken;

    use super::*;

    #[tokio::test]
    async fn cancellation_wins_over_available_or_closed_admission() {
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        for closed in [false, true] {
            let slots = Arc::new(Semaphore::new(1));
            if closed {
                slots.close();
            }
            let error = permit_or_cancelled(&slots, &cancelled, "stage is stopping")
                .await
                .unwrap_err();
            assert_eq!(error.message, CANCELLED_MESSAGE);
            assert!(error.retryable);
            assert_eq!(slots.available_permits(), 1);
        }
    }

    #[tokio::test]
    async fn cancelled_waiter_does_not_consume_the_next_permit() {
        let slots = Arc::new(Semaphore::new(1));
        let held = slots.clone().acquire_owned().await.unwrap();
        let cancelled = CancellationToken::new();
        let waiting = {
            let slots = slots.clone();
            let cancelled = cancelled.clone();
            tokio::spawn(async move {
                permit_or_cancelled(&slots, &cancelled, "stage is stopping").await
            })
        };
        tokio::task::yield_now().await;
        cancelled.cancel();
        assert_eq!(
            waiting.await.unwrap().unwrap_err().message,
            CANCELLED_MESSAGE
        );
        drop(held);
        let permit = permit_or_cancelled(&slots, &CancellationToken::new(), "stage is stopping")
            .await
            .unwrap();
        assert_eq!(slots.available_permits(), 0);
        drop(permit);
        assert_eq!(slots.available_permits(), 1);
        slots.close();
        let error = permit_or_cancelled(&slots, &CancellationToken::new(), "stage is stopping")
            .await
            .unwrap_err();
        assert_eq!(error.message, "stage is stopping");
        assert!(error.retryable);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn aborted_waiter_keeps_blocking_work_admitted_until_it_stops() {
        let slots = Arc::new(Semaphore::new(1));
        let (started, entered) = tokio::sync::oneshot::channel();
        let (release, waiting) = std::sync::mpsc::channel();
        let task = {
            let slots = slots.clone();
            tokio::spawn(async move {
                let permit =
                    permit_or_cancelled(&slots, &CancellationToken::new(), "stage is stopping")
                        .await
                        .unwrap();
                tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    let _ = started.send(());
                    waiting
                        .recv_timeout(std::time::Duration::from_secs(3))
                        .unwrap();
                })
                .await
                .unwrap();
            })
        };
        entered.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(slots.clone().try_acquire_owned().is_err());
        release.send(()).unwrap();
        let permit = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            slots.clone().acquire_owned(),
        )
        .await
        .unwrap()
        .unwrap();
        drop(permit);
        assert_eq!(slots.available_permits(), 1);
    }
}
