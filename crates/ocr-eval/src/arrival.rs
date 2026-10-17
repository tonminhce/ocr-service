use std::time::Duration;

use anyhow::{Result, ensure};
use futures_util::{StreamExt, stream::FuturesUnordered};
use serde::Serialize;
use tokio::time::Instant;
use uuid::Uuid;

use crate::{Dataset, Split, benchmark};

const MAX_JOBS: usize = 10_000;
const MAX_PENDING_TASKS: usize = 4;
const MAX_TOTAL_UPLOAD_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_OUTSTANDING_JOBS: usize = 1_000;
const MAX_DRAIN_SECONDS: u64 = 3_600;
const DEFAULT_OUTSTANDING_JOBS: usize = 100;
const DEFAULT_DRAIN_SECONDS: u64 = 300;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArrivalPattern {
    FixedRate,
    SeededPoisson,
}

#[derive(Clone, Debug)]
pub struct ArrivalConfig {
    pub pattern: ArrivalPattern,
    pub target_per_second: f64,
    pub duration: Duration,
    pub max_jobs: usize,
    pub max_total_upload_bytes: u64,
    pub max_pending_tasks: usize,
    pub max_outstanding_jobs: usize,
    pub drain_deadline: Duration,
    pub seed: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScheduledArrival {
    pub sequence: u64,
    pub offset: Duration,
}

#[derive(Clone, Debug)]
pub struct ArrivalSchedule {
    pub arrivals: Vec<ScheduledArrival>,
    pub window: Duration,
}

impl ArrivalConfig {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pattern: ArrivalPattern,
        target_per_second: f64,
        duration_seconds: u64,
        max_jobs: usize,
        max_total_upload_bytes: u64,
        max_pending_tasks: usize,
        seed: u64,
    ) -> Result<Self> {
        ensure!(
            target_per_second.is_finite() && (0.001..=100.0).contains(&target_per_second),
            "arrival rate must be finite and between 0.001 and 100 documents per second"
        );
        ensure!(
            (1..=86_400).contains(&duration_seconds),
            "arrival duration must be 1..86400 seconds"
        );
        ensure!(
            (1..=MAX_JOBS).contains(&max_jobs),
            "maximum scheduled jobs must be 1..10000"
        );
        ensure!(
            (1..=MAX_TOTAL_UPLOAD_BYTES).contains(&max_total_upload_bytes),
            "maximum total upload bytes must be 1..4 GiB"
        );
        ensure!(
            (1..=MAX_PENDING_TASKS).contains(&max_pending_tasks),
            "maximum pending tasks must be 1..4"
        );
        Ok(Self {
            pattern,
            target_per_second,
            duration: Duration::from_secs(duration_seconds),
            max_jobs,
            max_total_upload_bytes,
            max_pending_tasks,
            max_outstanding_jobs: DEFAULT_OUTSTANDING_JOBS,
            drain_deadline: Duration::from_secs(DEFAULT_DRAIN_SECONDS),
            seed,
        })
    }

    pub fn with_registry_limits(
        mut self,
        max_outstanding_jobs: usize,
        drain_deadline_seconds: u64,
    ) -> Result<Self> {
        ensure!(
            (1..=MAX_OUTSTANDING_JOBS).contains(&max_outstanding_jobs),
            "maximum outstanding jobs must be 1..1000"
        );
        ensure!(
            (1..=MAX_DRAIN_SECONDS).contains(&drain_deadline_seconds),
            "drain deadline must be 1..3600 seconds"
        );
        self.max_outstanding_jobs = max_outstanding_jobs;
        self.drain_deadline = Duration::from_secs(drain_deadline_seconds);
        Ok(self)
    }

    pub fn schedule(&self) -> ArrivalSchedule {
        let mut arrivals = Vec::with_capacity(self.max_jobs.min(256));
        let mut offset = Duration::ZERO;
        let mut random = SeededRandom::new(self.seed);
        let mut window = self.duration;
        for sequence in 0..self.max_jobs {
            if sequence > 0 || self.pattern == ArrivalPattern::SeededPoisson {
                let interval = next_interval(self.pattern, self.target_per_second, &mut random);
                offset = offset.saturating_add(interval);
            }
            if offset >= self.duration {
                break;
            }
            arrivals.push(ScheduledArrival {
                sequence: sequence as u64,
                offset,
            });
            if sequence + 1 == self.max_jobs {
                window = offset
                    .saturating_add(next_interval(
                        self.pattern,
                        self.target_per_second,
                        &mut random,
                    ))
                    .min(self.duration);
            }
        }
        ArrivalSchedule { arrivals, window }
    }
}

fn next_interval(
    pattern: ArrivalPattern,
    target_per_second: f64,
    random: &mut SeededRandom,
) -> Duration {
    match pattern {
        ArrivalPattern::FixedRate => Duration::from_secs_f64(1.0 / target_per_second),
        ArrivalPattern::SeededPoisson => {
            let unit = random.unit_interval();
            Duration::from_secs_f64(-(1.0 - unit).ln() / target_per_second)
        }
    }
    .max(Duration::from_nanos(1))
}

struct SeededRandom(u64);

impl SeededRandom {
    fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9e37_79b9_7f4a_7c15
        } else {
            seed
        })
    }

    fn unit_interval(&mut self) -> f64 {
        let mut value = self.0;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.0 = value;
        let mantissa = value >> 12;
        (mantissa as f64 + 0.5) / 4_503_599_627_370_496.0
    }
}

#[derive(Debug, Serialize)]
pub struct ArrivalOccurrence {
    pub sequence: u64,
    pub document_id: String,
    pub scheduled_offset_ms: f64,
    pub scheduler_lag_ms: f64,
    pub offered: bool,
    pub skip_reason: Option<&'static str>,
    pub submit_latency_ms: Option<f64>,
    pub accepted_to_terminal_latency_ms: Option<f64>,
    pub result: Option<benchmark::BenchmarkDocument>,
}

#[derive(Debug, Serialize)]
pub struct ArrivalReport {
    pub schema_version: &'static str,
    pub run_id: Uuid,
    pub dataset_name: String,
    pub synthetic: bool,
    pub split: Split,
    pub pattern: ArrivalPattern,
    pub seed: u64,
    pub target_per_second: f64,
    pub configured_duration_seconds: f64,
    pub scheduled_window_seconds: f64,
    pub schedule_elapsed_seconds: f64,
    pub client_drain_seconds: f64,
    pub elapsed_seconds: f64,
    pub scheduled: usize,
    pub offered: usize,
    pub submission_attempted: usize,
    pub accepted: usize,
    pub rejected: usize,
    pub not_submitted: usize,
    pub acceptance_unknown: usize,
    pub completed: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub timed_out: usize,
    pub outstanding: usize,
    pub accepted_outstanding: usize,
    pub client_skipped_or_late: usize,
    pub upload_budget_skipped: usize,
    pub source_bytes_budgeted: u64,
    pub offered_per_second: f64,
    pub submission_attempts_per_second: f64,
    pub submission_starts_in_window: usize,
    pub submission_starts_after_window: usize,
    pub submission_starts_in_window_per_second: f64,
    pub submission_scheduling_lag_samples: usize,
    pub submission_scheduling_lag_p50_ms: Option<f64>,
    pub submission_scheduling_lag_p95_ms: Option<f64>,
    pub scheduler_lag_p50_ms: Option<f64>,
    pub scheduler_lag_p95_ms: Option<f64>,
    pub submission_latency_samples: usize,
    pub submission_latency_p50_ms: Option<f64>,
    pub submission_latency_p95_ms: Option<f64>,
    pub accepted_to_terminal_samples: usize,
    pub accepted_to_terminal_p50_ms: Option<f64>,
    pub accepted_to_terminal_p95_ms: Option<f64>,
    pub successful_completion_samples: usize,
    pub successful_completion_p50_ms: Option<f64>,
    pub successful_completion_p95_ms: Option<f64>,
    pub max_pending_tasks: usize,
    pub maximum_in_flight: usize,
    pub max_outstanding_jobs: usize,
    pub maximum_registry_occupancy: usize,
    pub outstanding_registry_skipped: usize,
    pub drain_deadline_seconds: f64,
    pub documents: Vec<ArrivalOccurrence>,
    pub limitations: Vec<String>,
}

type SubmitTask =
    std::pin::Pin<Box<tokio::task::JoinHandle<(SubmittedBase, benchmark::Submission)>>>;

struct SubmittedBase {
    sequence: u64,
    document_id: String,
    scheduled_offset_ms: f64,
    scheduler_lag_ms: f64,
}

struct RegistryEntry {
    base: SubmittedBase,
    job_id: Uuid,
    progress: benchmark::Progress,
}

fn failed_document(
    base: &SubmittedBase,
    submission: benchmark::Submission,
) -> benchmark::BenchmarkDocument {
    let progress = &submission.progress;
    benchmark::BenchmarkDocument {
        document_id: base.document_id.clone(),
        accepted: progress.submission_outcome == benchmark::SubmissionOutcome::Accepted,
        submission_outcome: progress.submission_outcome,
        idempotency_key: progress.idempotency_key.clone(),
        submission_started_offset_ms: progress
            .submission_started_offset
            .map(|offset| offset.as_secs_f64() * 1000.0),
        submission_started_offset: progress.submission_started_offset,
        job_id: progress.job_id,
        status: "failed".into(),
        latency_ms: 0.0,
        submit_latency_ms: progress.submit_latency_ms,
        accepted_to_terminal_latency_ms: None,
        attempts: None,
        status_polls: progress.polls,
        error_code: submission.outcome.failed_error(),
    }
}

fn accepted_occurrence(
    base: SubmittedBase,
    job_id: Uuid,
    progress: &benchmark::Progress,
) -> ArrivalOccurrence {
    ArrivalOccurrence {
        sequence: base.sequence,
        document_id: base.document_id.clone(),
        scheduled_offset_ms: base.scheduled_offset_ms,
        scheduler_lag_ms: base.scheduler_lag_ms,
        offered: true,
        skip_reason: None,
        submit_latency_ms: progress.submit_latency_ms,
        accepted_to_terminal_latency_ms: None,
        result: Some(benchmark::BenchmarkDocument {
            document_id: base.document_id,
            accepted: true,
            submission_outcome: progress.submission_outcome,
            idempotency_key: progress.idempotency_key.clone(),
            submission_started_offset_ms: progress
                .submission_started_offset
                .map(|offset| offset.as_secs_f64() * 1000.0),
            submission_started_offset: progress.submission_started_offset,
            job_id: Some(job_id),
            status: "outstanding".into(),
            latency_ms: 0.0,
            submit_latency_ms: progress.submit_latency_ms,
            accepted_to_terminal_latency_ms: None,
            attempts: None,
            status_polls: progress.polls,
            error_code: None,
        }),
    }
}

fn collect_submitted(
    base: SubmittedBase,
    submission: benchmark::Submission,
    registry: &mut Vec<RegistryEntry>,
    results: &mut Vec<ArrivalOccurrence>,
    max_outstanding_jobs: usize,
    outstanding_registry_skipped: &mut usize,
    maximum_registry_occupancy: &mut usize,
) {
    match submission.outcome {
        benchmark::SubmitOutcome::Submitted(job_id) => {
            if registry.len() >= max_outstanding_jobs {
                *outstanding_registry_skipped += 1;
                let mut occurrence = accepted_occurrence(base, job_id, &submission.progress);
                occurrence.skip_reason = Some("outstanding_registry_limit");
                results.push(occurrence);
            } else {
                registry.push(RegistryEntry {
                    base,
                    job_id,
                    progress: submission.progress,
                });
                *maximum_registry_occupancy = (*maximum_registry_occupancy).max(registry.len());
            }
        }
        benchmark::SubmitOutcome::Failed(_) => {
            let mut document = failed_document(&base, submission);
            if document.error_code.as_deref() == Some("document_deadline_exceeded") {
                document.status = "timeout".into();
            }
            results.push(ArrivalOccurrence {
                sequence: base.sequence,
                document_id: base.document_id.clone(),
                scheduled_offset_ms: base.scheduled_offset_ms,
                scheduler_lag_ms: base.scheduler_lag_ms,
                offered: true,
                skip_reason: None,
                submit_latency_ms: document.submit_latency_ms,
                accepted_to_terminal_latency_ms: None,
                result: Some(document),
            });
        }
    }
}

fn tracked_occurrence(
    entry: RegistryEntry,
    outcome: std::result::Result<benchmark::Status, String>,
) -> ArrivalOccurrence {
    let RegistryEntry {
        base,
        job_id,
        progress,
    } = entry;
    let (status, attempts, error_code, accepted_to_terminal) = match outcome {
        Ok(status) => {
            let error = (status.status == "failed").then(|| "server_job_failed".to_string());
            (
                status.status,
                Some(status.attempts),
                error,
                progress.accepted_to_terminal_latency_ms,
            )
        }
        Err(error) => ("failed".to_string(), None, Some(error), None),
    };
    let accepted_to_terminal_latency_ms = accepted_to_terminal;
    ArrivalOccurrence {
        sequence: base.sequence,
        document_id: base.document_id.clone(),
        scheduled_offset_ms: base.scheduled_offset_ms,
        scheduler_lag_ms: base.scheduler_lag_ms,
        offered: true,
        skip_reason: None,
        submit_latency_ms: progress.submit_latency_ms,
        accepted_to_terminal_latency_ms,
        result: Some(benchmark::BenchmarkDocument {
            document_id: base.document_id,
            accepted: true,
            submission_outcome: benchmark::SubmissionOutcome::Accepted,
            idempotency_key: progress.idempotency_key,
            submission_started_offset_ms: progress
                .submission_started_offset
                .map(|offset| offset.as_secs_f64() * 1000.0),
            submission_started_offset: progress.submission_started_offset,
            job_id: Some(job_id),
            status,
            latency_ms: 0.0,
            submit_latency_ms: progress.submit_latency_ms,
            accepted_to_terminal_latency_ms,
            attempts,
            status_polls: progress.polls,
            error_code,
        }),
    }
}

enum TrackedOutcome {
    Observed(
        RegistryEntry,
        std::result::Result<benchmark::Status, String>,
    ),
    Deadline(RegistryEntry),
}

fn timeout_occurrence(entry: RegistryEntry) -> ArrivalOccurrence {
    let RegistryEntry {
        base,
        job_id,
        progress,
    } = entry;
    let mut occurrence = accepted_occurrence(base, job_id, &progress);
    if let Some(result) = occurrence.result.as_mut() {
        result.status = "timeout".into();
        result.error_code = Some("document_deadline_exceeded".into());
    }
    occurrence
}

pub async fn run(
    dataset: &Dataset,
    split: Split,
    benchmark_config: &benchmark::BenchmarkConfig,
    arrival_config: &ArrivalConfig,
) -> Result<ArrivalReport> {
    ensure!(
        arrival_config.max_pending_tasks <= benchmark_config.clients,
        "maximum pending tasks cannot exceed configured clients"
    );
    let documents = dataset.selected(split)?;
    let schedule = arrival_config.schedule();
    let run_id = Uuid::new_v4();
    let client = benchmark::build_client(benchmark_config)?;
    let dataset_arc = std::sync::Arc::new(dataset.clone());
    let started = Instant::now();
    let mut pending: FuturesUnordered<SubmitTask> = FuturesUnordered::new();
    let mut results = Vec::with_capacity(schedule.arrivals.len());
    let mut scheduler_lags = Vec::with_capacity(schedule.arrivals.len());
    let mut source_bytes_budgeted = 0_u64;
    let mut upload_budget_skipped = 0;
    let mut maximum_in_flight = 0;
    let mut outstanding_registry_skipped = 0;
    let mut registry: Vec<RegistryEntry> = Vec::new();
    let mut maximum_registry_occupancy = 0;

    for arrival in &schedule.arrivals {
        let due = started + arrival.offset;
        loop {
            tokio::select! {
                biased;
                Some(submitted) = pending.next(), if !pending.is_empty() => {
                    let (base, submission) = submitted.expect("submit task must not panic");
                    collect_submitted(
                        base,
                        submission,
                        &mut registry,
                        &mut results,
                        arrival_config.max_outstanding_jobs,
                        &mut outstanding_registry_skipped,
                        &mut maximum_registry_occupancy,
                    );
                }
                _ = tokio::time::sleep_until(due) => break,
            }
        }

        let lag = Instant::now().saturating_duration_since(due);
        let lag_ms = lag.as_secs_f64() * 1000.0;
        scheduler_lags.push(lag_ms);
        let document = documents[arrival.sequence as usize % documents.len()];
        let offset_ms = arrival.offset.as_secs_f64() * 1000.0;
        if pending.len() >= arrival_config.max_pending_tasks {
            results.push(ArrivalOccurrence {
                sequence: arrival.sequence,
                document_id: document.id.clone(),
                scheduled_offset_ms: offset_ms,
                scheduler_lag_ms: lag_ms,
                offered: false,
                skip_reason: Some("pending_task_limit"),
                submit_latency_ms: None,
                accepted_to_terminal_latency_ms: None,
                result: None,
            });
            continue;
        }
        let source_size = match dataset.source_size(document) {
            Ok(size) => size,
            Err(_) => {
                results.push(ArrivalOccurrence {
                    sequence: arrival.sequence,
                    document_id: document.id.clone(),
                    scheduled_offset_ms: offset_ms,
                    scheduler_lag_ms: lag_ms,
                    offered: false,
                    skip_reason: Some("source_unavailable"),
                    submit_latency_ms: None,
                    accepted_to_terminal_latency_ms: None,
                    result: None,
                });
                continue;
            }
        };
        if source_size > arrival_config.max_total_upload_bytes - source_bytes_budgeted {
            upload_budget_skipped += 1;
            results.push(ArrivalOccurrence {
                sequence: arrival.sequence,
                document_id: document.id.clone(),
                scheduled_offset_ms: offset_ms,
                scheduler_lag_ms: lag_ms,
                offered: false,
                skip_reason: Some("total_upload_byte_limit"),
                submit_latency_ms: None,
                accepted_to_terminal_latency_ms: None,
                result: None,
            });
            continue;
        }

        source_bytes_budgeted += source_size;
        let submit_client = client.clone();
        let submit_config = benchmark_config.clone();
        let submit_dataset = dataset_arc.clone();
        let submit_document = document.clone();
        let occurrence = benchmark::Occurrence {
            run_id,
            run_started: started.into_std(),
            sequence: Some(arrival.sequence),
            expected_source_size: Some(source_size),
        };
        let spawn_offset_ms = offset_ms;
        let spawn_lag_ms = lag_ms;
        pending.push(Box::pin(tokio::spawn(async move {
            let submission = benchmark::submit_document_deadlined(
                &submit_client,
                &submit_dataset,
                &submit_document,
                &submit_config,
                occurrence,
                submit_config.deadline,
            )
            .await;
            (
                SubmittedBase {
                    sequence: occurrence.sequence.unwrap_or_default(),
                    document_id: submit_document.id,
                    scheduled_offset_ms: spawn_offset_ms,
                    scheduler_lag_ms: spawn_lag_ms,
                },
                submission,
            )
        })));
        maximum_in_flight = maximum_in_flight.max(pending.len());
    }

    let schedule_finished = Instant::now();
    let schedule_elapsed_seconds = schedule_finished.duration_since(started).as_secs_f64();
    while let Some(submitted) = pending.next().await {
        let (base, submission) = submitted.expect("submit task must not panic");
        collect_submitted(
            base,
            submission,
            &mut registry,
            &mut results,
            arrival_config.max_outstanding_jobs,
            &mut outstanding_registry_skipped,
            &mut maximum_registry_occupancy,
        );
    }
    let mut tracked: Vec<(
        RegistryEntry,
        std::result::Result<benchmark::Status, String>,
    )> = Vec::with_capacity(registry.len());
    let drain = async {
        let mut trackers: FuturesUnordered<
            std::pin::Pin<Box<tokio::task::JoinHandle<TrackedOutcome>>>,
        > = FuturesUnordered::new();
        for entry in registry.drain(..) {
            let track_client = client.clone();
            let track_config = benchmark_config.clone();
            let document_deadline = benchmark_config.deadline;
            trackers.push(Box::pin(tokio::spawn(async move {
                let submit_elapsed = entry.progress.submit_deadline_consumed();
                let remaining = document_deadline.saturating_sub(submit_elapsed);
                let mut progress = entry.progress.clone();
                match tokio::time::timeout(
                    remaining,
                    benchmark::track_document(
                        &track_client,
                        entry.job_id,
                        &track_config,
                        &mut progress,
                    ),
                )
                .await
                {
                    Ok(outcome) => {
                        TrackedOutcome::Observed(RegistryEntry { progress, ..entry }, outcome)
                    }
                    Err(_) => TrackedOutcome::Deadline(entry),
                }
            })));
        }
        while let Some(tracked_pair) = trackers.next().await {
            match tracked_pair.expect("track task must not panic") {
                TrackedOutcome::Observed(entry, outcome) => {
                    tracked.push((entry, outcome));
                }
                TrackedOutcome::Deadline(entry) => {
                    results.push(timeout_occurrence(entry));
                }
            }
        }
    };
    if tokio::time::timeout(arrival_config.drain_deadline, drain)
        .await
        .is_err()
    {
        for entry in registry.drain(..) {
            results.push(timeout_occurrence(entry));
        }
    }
    for (entry, outcome) in tracked {
        results.push(tracked_occurrence(entry, outcome));
    }
    let client_drain_seconds = Instant::now()
        .duration_since(schedule_finished)
        .as_secs_f64();
    results.sort_by_key(|result| result.sequence);
    let elapsed_seconds = started.elapsed().as_secs_f64();
    let offered_results: Vec<_> = results
        .iter()
        .filter_map(|occurrence| occurrence.result.as_ref())
        .collect();
    let accepted = offered_results
        .iter()
        .filter(|result| result.accepted)
        .count();
    let rejected = offered_results
        .iter()
        .filter(|result| result.submission_outcome == benchmark::SubmissionOutcome::Rejected)
        .count();
    let not_submitted = offered_results
        .iter()
        .filter(|result| result.submission_outcome == benchmark::SubmissionOutcome::NotSubmitted)
        .count();
    let acceptance_unknown = offered_results
        .iter()
        .filter(|result| {
            result.submission_outcome == benchmark::SubmissionOutcome::AcceptanceUnknown
        })
        .count();
    let submit_latencies: Vec<_> = offered_results
        .iter()
        .filter_map(|result| result.submit_latency_ms)
        .collect();
    let accepted_to_terminal_latencies: Vec<_> = offered_results
        .iter()
        .filter_map(|result| result.accepted_to_terminal_latency_ms)
        .collect();
    let successful_completion_latencies: Vec<_> = offered_results
        .iter()
        .filter(|result| result.status == "succeeded")
        .map(|result| result.latency_ms)
        .collect();
    let succeeded = offered_results
        .iter()
        .filter(|result| result.status == "succeeded")
        .count();
    let failed = offered_results
        .iter()
        .filter(|result| result.status == "failed")
        .count();
    let timed_out = offered_results
        .iter()
        .filter(|result| result.status == "timeout")
        .count();
    let completed = offered_results
        .iter()
        .filter(|result| {
            result.accepted
                && (result.status == "succeeded"
                    || result.error_code.as_deref() == Some("server_job_failed"))
        })
        .count();
    let accepted_outstanding = accepted - completed;
    let outstanding = accepted_outstanding + acceptance_unknown;
    let offered = offered_results.len();
    let submission_attempted = offered - not_submitted;
    let client_skipped_or_late = results
        .iter()
        .filter(|result| !result.offered && result.skip_reason != Some("total_upload_byte_limit"))
        .count();
    let offered_per_second = offered as f64 / schedule.window.as_secs_f64();
    let timing = submission_timing(
        results.iter().filter_map(|occurrence| {
            let started = occurrence.result.as_ref()?.submission_started_offset?;
            let scheduled = schedule.arrivals[occurrence.sequence as usize].offset;
            Some((scheduled, started))
        }),
        schedule.window,
    );
    Ok(ArrivalReport {
        schema_version: "ocr-arrival-benchmark/1.1.0",
        run_id,
        dataset_name: dataset.manifest.dataset_name.clone(),
        synthetic: dataset.manifest.synthetic,
        split,
        pattern: arrival_config.pattern,
        seed: arrival_config.seed,
        target_per_second: arrival_config.target_per_second,
        configured_duration_seconds: arrival_config.duration.as_secs_f64(),
        scheduled_window_seconds: schedule.window.as_secs_f64(),
        schedule_elapsed_seconds,
        client_drain_seconds,
        elapsed_seconds,
        scheduled: schedule.arrivals.len(),
        offered,
        submission_attempted,
        accepted,
        rejected,
        not_submitted,
        acceptance_unknown,
        completed,
        succeeded,
        failed,
        timed_out,
        outstanding,
        accepted_outstanding,
        client_skipped_or_late,
        upload_budget_skipped,
        source_bytes_budgeted,
        offered_per_second,
        submission_attempts_per_second: submission_attempted as f64 / schedule.window.as_secs_f64(),
        submission_starts_in_window: timing.in_window,
        submission_starts_after_window: timing.after_window,
        submission_starts_in_window_per_second: timing.in_window as f64
            / schedule.window.as_secs_f64(),
        submission_scheduling_lag_samples: timing.scheduling_lags_ms.len(),
        submission_scheduling_lag_p50_ms: benchmark::percentile(&timing.scheduling_lags_ms, 0.50),
        submission_scheduling_lag_p95_ms: benchmark::percentile(&timing.scheduling_lags_ms, 0.95),
        scheduler_lag_p50_ms: benchmark::percentile(&scheduler_lags, 0.50),
        scheduler_lag_p95_ms: benchmark::percentile(&scheduler_lags, 0.95),
        submission_latency_samples: submit_latencies.len(),
        submission_latency_p50_ms: benchmark::percentile(&submit_latencies, 0.50),
        submission_latency_p95_ms: benchmark::percentile(&submit_latencies, 0.95),
        accepted_to_terminal_samples: accepted_to_terminal_latencies.len(),
        accepted_to_terminal_p50_ms: benchmark::percentile(
            &accepted_to_terminal_latencies,
            0.50,
        ),
        accepted_to_terminal_p95_ms: benchmark::percentile(
            &accepted_to_terminal_latencies,
            0.95,
        ),
        successful_completion_samples: successful_completion_latencies.len(),
        successful_completion_p50_ms: benchmark::percentile(
            &successful_completion_latencies,
            0.50,
        ),
        successful_completion_p95_ms: benchmark::percentile(
            &successful_completion_latencies,
            0.95,
        ),
        max_pending_tasks: arrival_config.max_pending_tasks,
        maximum_in_flight,
        max_outstanding_jobs: arrival_config.max_outstanding_jobs,
        maximum_registry_occupancy,
        outstanding_registry_skipped,
        drain_deadline_seconds: arrival_config.drain_deadline.as_secs_f64(),
        documents: results,
        limitations: vec![
            "Repeated occurrences of a corpus source are workload repetitions, not independent quality samples.".into(),
            "W/R are operator-declared metadata; this runner neither verifies nor changes runtime concurrency.".into(),
            "Accepted or acceptance-unknown submissions may continue after client failure. Reconcile private job IDs/idempotency keys and drain server work before another run; this runner never retries submission.".into(),
            "A capped offer rate or client-skipped occurrence is not evidence that the requested target rate was achieved.".into(),
            "Submission timing records the monotonic handoff to the HTTP client, not server receipt. Only starts before the schedule window ends contribute to submission_starts_in_window_per_second; normalized legacy rates may include later starts.".into(),
        ],
    })
}

#[derive(Default)]
struct SubmissionTiming {
    in_window: usize,
    after_window: usize,
    scheduling_lags_ms: Vec<f64>,
}

fn submission_timing(
    attempts: impl IntoIterator<Item = (Duration, Duration)>,
    window: Duration,
) -> SubmissionTiming {
    let mut timing = SubmissionTiming::default();
    for (scheduled, started) in attempts {
        if started < window {
            timing.in_window += 1;
        } else {
            timing.after_window += 1;
        }
        timing
            .scheduling_lags_ms
            .push(started.saturating_sub(scheduled).as_secs_f64() * 1000.0);
    }
    timing
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(pattern: ArrivalPattern) -> ArrivalConfig {
        ArrivalConfig::new(pattern, 2.0, 2, 20, 1024, 2, 7).unwrap()
    }

    #[test]
    fn fixed_rate_schedule_has_exact_offsets_and_duration_boundary() {
        let schedule = config(ArrivalPattern::FixedRate).schedule();
        assert_eq!(schedule.window, Duration::from_secs(2));
        assert_eq!(schedule.arrivals.len(), 4);
        assert_eq!(
            schedule
                .arrivals
                .iter()
                .map(|arrival| arrival.sequence)
                .collect::<Vec<_>>(),
            [0, 1, 2, 3]
        );
        assert_eq!(
            schedule
                .arrivals
                .iter()
                .map(|arrival| arrival.offset)
                .collect::<Vec<_>>(),
            [
                Duration::ZERO,
                Duration::from_millis(500),
                Duration::from_secs(1),
                Duration::from_millis(1500),
            ]
        );
    }

    #[test]
    fn poisson_schedule_is_seeded_and_respects_job_cap() {
        let mut first = config(ArrivalPattern::SeededPoisson);
        first.duration = Duration::from_secs(86_400);
        first.max_jobs = 12;
        let second = first.clone();
        let schedule = first.schedule();
        assert_eq!(schedule.arrivals, second.schedule().arrivals);
        assert_eq!(schedule.arrivals.len(), 12);
        assert!(schedule.arrivals[0].offset > Duration::ZERO);
        assert!(
            schedule
                .arrivals
                .windows(2)
                .all(|pair| pair[0].offset < pair[1].offset)
        );
    }

    #[test]
    fn arrival_config_rejects_unbounded_or_nonfinite_limits() {
        for rate in [0.0, f64::INFINITY, f64::NAN, 100.1] {
            assert!(
                ArrivalConfig::new(ArrivalPattern::FixedRate, rate, 10, 10, 1024, 1, 1).is_err()
            );
        }
        assert!(ArrivalConfig::new(ArrivalPattern::FixedRate, 1.0, 0, 10, 1024, 1, 1).is_err());
        assert!(
            ArrivalConfig::new(ArrivalPattern::FixedRate, 1.0, 10, 10_001, 1024, 1, 1).is_err()
        );
        assert!(ArrivalConfig::new(ArrivalPattern::FixedRate, 1.0, 10, 10, 1024, 5, 1).is_err());
    }

    #[test]
    fn poisson_random_endpoints_are_strictly_inside_the_unit_interval() {
        for (seed, expected) in [
            (0, f64::EPSILON / 2.0),
            (7_650_297_886_450_228_676, 1.0 - f64::EPSILON / 2.0),
        ] {
            let value = SeededRandom(seed).unit_interval();
            assert_eq!(value, expected);
            assert!(value > 0.0 && value < 1.0);
            let interval = next_interval(
                ArrivalPattern::SeededPoisson,
                0.001,
                &mut SeededRandom(seed),
            );
            assert!(!interval.is_zero());
            assert!(interval < Duration::from_secs(36_737));
        }
    }

    #[test]
    fn minimum_valid_poisson_rate_is_bounded_and_tiny_rates_are_rejected() {
        assert!(
            ArrivalConfig::new(ArrivalPattern::SeededPoisson, 1e-300, 1, 1, 1024, 1, 1).is_err()
        );
        let config = ArrivalConfig::new(
            ArrivalPattern::SeededPoisson,
            0.001,
            86_400,
            10_000,
            1024,
            1,
            7_650_297_886_450_228_676,
        )
        .unwrap();
        let schedule = config.schedule();
        assert_eq!(schedule.window, config.duration);
        assert!(schedule.arrivals.len() < config.max_jobs);
        assert!(
            schedule
                .arrivals
                .iter()
                .all(|arrival| arrival.offset < config.duration)
        );
    }

    #[test]
    fn submission_window_end_is_exclusive_and_lag_uses_actual_start() {
        let window = Duration::from_secs(1);
        let scheduled = Duration::from_millis(900);
        let timing = submission_timing(
            [
                (Duration::ZERO, Duration::ZERO),
                (scheduled, window - Duration::from_nanos(1)),
                (scheduled, window),
                (scheduled, window + Duration::from_nanos(1)),
            ],
            window,
        );
        assert_eq!(timing.in_window, 2);
        assert_eq!(timing.after_window, 2);
        assert_eq!(timing.scheduling_lags_ms.len(), 4);
        assert_eq!(timing.scheduling_lags_ms[0], 0.0);
        assert_eq!(timing.scheduling_lags_ms[2], 100.0);
        assert!(timing.scheduling_lags_ms[1] < 100.0);
        assert!(timing.scheduling_lags_ms[3] > 100.0);
        let empty = submission_timing([], window);
        assert_eq!(empty.in_window + empty.after_window, 0);
        assert!(empty.scheduling_lags_ms.is_empty());
    }
}
