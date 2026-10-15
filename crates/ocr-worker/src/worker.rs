use std::time::Duration;

use ocr_core::{
    domain::LeasedJob,
    metrics::{Event, JobOutcome, Stage, process_metrics},
    store::Store,
};
use tokio::{
    task::JoinSet,
    time::{Instant, MissedTickBehavior},
};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::{ProcessingError, inference::Recognizer, queue::RedisQueue, settings::Settings};

struct AbortDispatcherOnDrop(tokio::task::AbortHandle);

impl Drop for AbortDispatcherOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Redis Streams is the only claim transport. PostgreSQL remains authoritative:
/// SQL leases fence inference, while its recurring outbox repairs publish gaps,
/// Redis restarts, and expired worker leases. Shutdown stops reads and dispatch,
/// then drains active inference with its heartbeat and deadline intact.
pub async fn run(
    store: Store,
    settings: Settings,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    let recognizer = Recognizer::new(settings.clone())?;
    let queue = RedisQueue::new(&settings)?;
    let dispatcher = tokio::spawn(dispatch_outbox(
        store.clone(),
        queue.clone(),
        settings.clone(),
        shutdown.clone(),
    ));
    let _abort_dispatcher = AbortDispatcherOnDrop(dispatcher.abort_handle());
    let mut active = JoinSet::new();
    let poll = Duration::from_millis(settings.poll_interval_ms);
    let mut next_read = Instant::now();
    let mut next_reclaim = Instant::now();
    let mut reclaim_cursor = "0-0".to_string();
    let mut read_failures = 0_u32;
    info!(concurrency = settings.worker_concurrency, model = %settings.vllm_model, "OCR worker started with Redis Streams transport");
    'running: loop {
        if active.len() < settings.worker_concurrency && Instant::now() >= next_read {
            let capacity = settings.worker_concurrency - active.len();
            let messages = tokio::select! {
                biased;
                _ = shutdown.cancelled() => break 'running,
                messages = async {
                    // RedisQueue initializes a group at 0 only on NOGROUP.
                    // Normal reads must remain possible at Redis maxmemory.
                    let mut messages = Vec::new();
                    if Instant::now() >= next_reclaim {
                        let reclaimed = queue.reclaim(&reclaim_cursor, settings.lease_seconds, capacity).await?;
                        reclaim_cursor = reclaimed.next_stream_id;
                        messages = reclaimed.claimed;
                        next_reclaim = Instant::now() + Duration::from_secs(1);
                    }
                    if messages.len() < capacity {
                        messages.extend(queue.read(capacity - messages.len()).await?);
                    }
                    Ok::<_, crate::queue::QueueError>(messages)
                } => messages,
            };
            match messages {
                Ok(messages) => {
                    read_failures = 0;
                    next_read = Instant::now() + poll;
                    for message in messages {
                        if shutdown.is_cancelled() {
                            break 'running;
                        }
                        let Some(id) = message
                            .get::<String>("job_id")
                            .and_then(|id| uuid::Uuid::parse_str(&id).ok())
                        else {
                            process_metrics().event(Event::NotificationMalformed);
                            warn!(message_id = %message.id, "discarding malformed Redis job notification");
                            acknowledge(&queue, &message.id).await;
                            continue;
                        };
                        let claimed = tokio::select! {
                            biased;
                            _ = shutdown.cancelled() => break 'running,
                            claim = store.claim_by_id(id, settings.lease_seconds, settings.max_attempts) => claim,
                        };
                        match claimed {
                            Ok(Some(leased)) => {
                                let store = store.clone();
                                let recognizer = recognizer.clone();
                                let settings = settings.clone();
                                let queue = queue.clone();
                                active.spawn(async move {
                                    if process_job(store, recognizer, settings, leased).await {
                                        acknowledge(&queue, &message.id).await;
                                    }
                                });
                            }
                            // A successful SQL claim returning None proves this
                            // notification is missing, terminal, already leased,
                            // or waiting for a durable retry. Its recurring
                            // outbox remains responsible for future eligibility.
                            Ok(None) => acknowledge(&queue, &message.id).await,
                            Err(error) => {
                                warn!(job_id = %id, error = %error, "SQL claim failed; leaving Redis notification pending")
                            }
                        }
                    }
                }
                Err(error) => {
                    process_metrics().event(Event::RedisReadFailed);
                    read_failures = read_failures.saturating_add(1);
                    next_read = Instant::now() + backoff(read_failures, poll);
                    warn!(error = %error, "Redis read failed; backing off");
                }
            }
        }
        // Deletion belongs to a separately gated maintenance operation. Runtime
        // credentials deliberately lack DELETE, including for rollback workers.
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,
            result = active.join_next(), if !active.is_empty() => report_task(result),
            _ = tokio::time::sleep(poll) => {},
        }
    }
    info!(
        active_jobs = active.len(),
        "worker stopping claims and draining active jobs"
    );
    // Dispatch commands are bounded and cancellable, independent from inference.
    // Await it after draining so even a dispatcher fault cannot abort live jobs.
    let drain = async {
        while let Some(result) = active.join_next().await {
            report_task(Some(result));
        }
    };
    if tokio::time::timeout(
        Duration::from_secs(settings.job_timeout_seconds + 10),
        drain,
    )
    .await
    .is_err()
    {
        warn!(
            active_jobs = active.len(),
            "shutdown deadline reached; leases will recover remaining jobs"
        );
        active.abort_all();
        while let Some(result) = active.join_next().await {
            report_task(Some(result));
        }
    }
    recognizer.shutdown().await;
    dispatcher.await?;
    if let Err(error) = queue.retire_consumer().await {
        warn!(error = %error, "Redis consumer cleanup deferred");
    }
    info!("OCR worker stopped");
    Ok(())
}

async fn acknowledge(queue: &RedisQueue, message_id: &str) {
    if let Err(error) = queue.acknowledge(message_id).await {
        process_metrics().event(Event::AcknowledgementFailed);
        warn!(message_id, error = %error, "Redis acknowledgment failed; pending recovery will retry");
    }
}

fn backoff(failures: u32, poll: Duration) -> Duration {
    poll.max(Duration::from_millis(250))
        .saturating_mul(2_u32.saturating_pow(failures.saturating_sub(1).min(7)))
        .min(Duration::from_secs(30))
}

/// This task continues publishing while every inference slot is occupied. A
/// publish is marked only after XADD succeeds, using a fenced outbox token. A
/// crash in either half causes a safe duplicate after the dispatch lease expires.
async fn dispatch_outbox(
    store: Store,
    queue: RedisQueue,
    settings: Settings,
    shutdown: CancellationToken,
) {
    let poll = Duration::from_millis(settings.poll_interval_ms);
    let mut failures = 0_u32;
    loop {
        let dispatch = async {
            const BATCH: i64 = 16;
            let dispatch_lease =
                ((settings.redis_timeout_ms * BATCH as u64) / 1000 + 10).clamp(30, 600) as i64;
            let deliveries = store
                .claim_outbox(BATCH, dispatch_lease)
                .await
                .map_err(|_| "outbox claim failed")?;
            for delivery in deliveries {
                queue
                    .publish(delivery.job_id)
                    .await
                    .map_err(|_| "Redis publish failed")?;
                process_metrics().event(Event::NotificationPublished);
                store
                    .mark_dispatched(
                        delivery.job_id,
                        delivery.token,
                        settings.redis_redispatch_seconds,
                    )
                    .await
                    .map_err(|_| "outbox dispatch commit failed")?;
            }
            Ok::<_, &'static str>(())
        };
        let result = tokio::select! {
            biased;
            _ = shutdown.cancelled() => return,
            result = dispatch => result,
        };
        let delay = match result {
            Ok(()) => {
                failures = 0;
                poll
            }
            Err(error) => {
                process_metrics().event(Event::OutboxFailed);
                failures = failures.saturating_add(1);
                warn!(error, "outbox dispatch failed; backing off");
                backoff(failures, poll)
            }
        };
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => return,
            _ = tokio::time::sleep(delay) => {},
        }
    }
}

fn report_task(result: Option<Result<(), tokio::task::JoinError>>) {
    if let Some(Err(error)) = result
        && !error.is_cancelled()
    {
        process_metrics().event(Event::TaskPanicked);
        error!(error = %error, "job task panicked; expired lease will recover it");
    }
}

async fn process_job(
    store: Store,
    recognizer: Recognizer,
    settings: Settings,
    leased: LeasedJob,
) -> bool {
    let id = leased.job.id;
    let token = leased.lease_token;
    let attempt = leased.job.attempts;
    let recognizer = recognizer.for_job(store.clone(), id, token);
    let metrics = process_metrics();
    let attempt_metrics = metrics.job_attempt();
    if attempt > 1 {
        metrics.event(Event::JobRetry);
    } else if let Ok(queue_wait) = leased
        .job
        .updated_at
        .signed_duration_since(leased.job.created_at)
        .to_std()
    {
        // Both timestamps come from PostgreSQL, avoiding cross-host clock skew.
        // Retries are excluded: this measures submission-to-first-claim delay.
        metrics.observe(Stage::QueueWait, queue_wait);
    }
    let cancelled = CancellationToken::new();
    let _cancel_on_drop = cancelled.clone().drop_guard();
    info!(job_id = %id, attempt, "processing document");
    let (outcome, maintenance_stopped) = {
        let processing = recognizer.recognize_with_profile(
            leased.input,
            &leased.job.media_type,
            &leased.job.extraction_profile,
            cancelled.clone(),
        );
        tokio::pin!(processing);
        let deadline = tokio::time::sleep(Duration::from_secs(settings.job_timeout_seconds));
        tokio::pin!(deadline);
        let mut heartbeat = tokio::time::interval(Duration::from_secs(
            (settings.lease_seconds / 3).max(1) as u64,
        ));
        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);
        // Poll renewal and processing independently. A checkpoint transaction
        // can hold the job row while awaiting another DB round trip; awaiting
        // renewal inside a select branch would stop polling that transaction
        // and deadlock it against our own heartbeat until timeout.
        let maintenance = async {
            // Skip the interval's immediate tick after the fresh SQL claim.
            heartbeat.tick().await;
            loop {
                heartbeat.tick().await;
                let budget = Duration::from_secs((settings.lease_seconds / 3).clamp(1, 5) as u64);
                match tokio::time::timeout(budget, store.renew(id, token, settings.lease_seconds))
                    .await
                {
                    Ok(Ok(true)) => {}
                    Ok(Ok(false)) => return "lease_lost",
                    Ok(Err(_)) => return "database_error",
                    Err(_) => return "renewal_timeout",
                }
            }
        };
        tokio::pin!(maintenance);
        tokio::select! {
            biased;
            _ = &mut deadline => {
                metrics.event(Event::JobDeadline);
                (Err(ProcessingError::transient("job processing exceeded its deadline")), false)
            },
            reason = &mut maintenance => {
                if reason != "lease_lost" {
                    metrics.event(Event::LeaseRenewalFailed);
                }
                cancelled.cancel();
                warn!(job_id = %id, reason, "lease maintenance stopped; cancelling local processing for recovery");
                (Err(ProcessingError::transient("lease maintenance stopped")), true)
            },
            outcome = &mut processing => (outcome, false),
        }
    }; // Drop pending HTTP/subprocess futures before committing a timeout.
    cancelled.cancel();
    info!(job_id = %id, attempt, telemetry = %serde_json::json!(recognizer.telemetry_snapshot()), "bounded inference attempt trace (server metadata, not OCR confidence)");
    if maintenance_stopped {
        // Pending inference has been dropped before the snapshot above. Do not
        // attempt a SQL mutation under a lost/uncertain lease.
        attempt_metrics.finish(JobOutcome::LeaseLost);
        return false;
    }
    let outcome_metric = match &outcome {
        Ok(_) => JobOutcome::Succeeded,
        Err(error) if error.retryable && attempt < settings.max_attempts => {
            JobOutcome::RetryScheduled
        }
        Err(_) => JobOutcome::Failed,
    };
    // Fenced SQL updates are conditional on this token and an unexpired lease.
    // A stale worker can spend inference time but can never overwrite a newer
    // attempt's result. Transient errors use bounded exponential retry delay.
    let persistence_timer = metrics.timer(Stage::Persistence);
    let commit = match outcome {
        Ok(mut result) => {
            let retain = settings.strategy == crate::settings::OcrStrategy::CourseLayout
                || leased.job.extraction_profile != "none";
            result["source"] = serde_json::json!({"sha256":leased.job.sha256,"media_type":leased.job.media_type,
                "original_retained":retain,"retention_days":settings.retention_days,
                "automatic_pruning":false,"retention_policy":"awaiting_maintenance_policy"});
            result["execution"] = recognizer.execution_manifest(&leased.job.extraction_profile);
            if retain {
                store.complete_retaining_input(id, token, result).await
            } else {
                store.complete(id, token, result).await
            }
        }
        Err(error) => {
            warn!(job_id = %id, retryable = error.retryable, error = %error, "document processing failed");
            let retry_delay_seconds = 2_i64.saturating_pow(attempt.clamp(1, 6) as u32).min(60);
            store
                .fail(
                    id,
                    token,
                    &error.message,
                    error.retryable,
                    settings.max_attempts,
                    retry_delay_seconds,
                )
                .await
        }
    };
    drop(persistence_timer);
    match commit {
        Ok(true) => {
            attempt_metrics.finish(outcome_metric);
            info!(job_id = %id, "job state committed");
            true
        }
        Ok(false) => {
            attempt_metrics.finish(JobOutcome::CommitStale);
            warn!(job_id = %id, "job commit rejected because its lease is stale");
            false
        }
        Err(error) => {
            attempt_metrics.finish(JobOutcome::CommitFailed);
            warn!(job_id = %id, error = %error, "job commit failed; expired lease will recover it");
            false
        }
    }
}
