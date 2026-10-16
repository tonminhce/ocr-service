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
            seed,
        })
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
        let mantissa = value >> 11;
        (mantissa as f64 + 1.0) / 9_007_199_254_740_993.0
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
    pub accepted: usize,
    pub rejected: usize,
    pub completed: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub timed_out: usize,
    pub outstanding: usize,
    pub client_skipped_or_late: usize,
    pub upload_budget_skipped: usize,
    pub source_bytes_budgeted: u64,
    pub offered_per_second: f64,
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
    pub documents: Vec<ArrivalOccurrence>,
    pub limitations: Vec<String>,
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
    let started = Instant::now();
    let mut pending = FuturesUnordered::new();
    let mut results = Vec::with_capacity(schedule.arrivals.len());
    let mut scheduler_lags = Vec::with_capacity(schedule.arrivals.len());
    let mut source_bytes_budgeted = 0_u64;
    let mut upload_budget_skipped = 0;
    let mut maximum_in_flight = 0;

    for arrival in &schedule.arrivals {
        let due = started + arrival.offset;
        loop {
            tokio::select! {
                biased;
                Some(result) = pending.next(), if !pending.is_empty() => results.push(result),
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
        let client = client.clone();
        pending.push(async move {
            let result = benchmark::execute_document(
                &client,
                dataset,
                document,
                benchmark_config,
                benchmark::Occurrence {
                    run_id,
                    sequence: Some(arrival.sequence),
                    expected_source_size: Some(source_size),
                },
            )
            .await;
            let submit_latency_ms = result.submit_latency_ms;
            let accepted_to_terminal_latency_ms = result.accepted_to_terminal_latency_ms;
            ArrivalOccurrence {
                sequence: arrival.sequence,
                document_id: document.id.clone(),
                scheduled_offset_ms: offset_ms,
                scheduler_lag_ms: lag_ms,
                offered: true,
                skip_reason: None,
                submit_latency_ms,
                accepted_to_terminal_latency_ms,
                result: Some(result),
            }
        });
        maximum_in_flight = maximum_in_flight.max(pending.len());
    }

    let schedule_finished = Instant::now();
    let schedule_elapsed_seconds = schedule_finished.duration_since(started).as_secs_f64();
    while let Some(result) = pending.next().await {
        results.push(result);
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
    let outstanding = accepted.saturating_sub(completed);
    let offered = offered_results.len();
    let client_skipped_or_late = results
        .iter()
        .filter(|result| !result.offered && result.skip_reason != Some("total_upload_byte_limit"))
        .count();
    let offered_per_second = offered as f64 / schedule.window.as_secs_f64();
    Ok(ArrivalReport {
        schema_version: "ocr-arrival-benchmark/1.0.0",
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
        accepted,
        rejected: offered.saturating_sub(accepted),
        completed,
        succeeded,
        failed,
        timed_out,
        outstanding,
        client_skipped_or_late,
        upload_budget_skipped,
        source_bytes_budgeted,
        offered_per_second,
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
        documents: results,
        limitations: vec![
            "Repeated occurrences of a corpus source are workload repetitions, not independent quality samples.".into(),
            "W/R are operator-declared metadata; this runner neither verifies nor changes runtime concurrency.".into(),
            "Accepted jobs may continue after a client timeout; inspect private job IDs and drain them before another run.".into(),
            "A capped offer rate or client-skipped occurrence is not evidence that the requested target rate was achieved.".into(),
        ],
    })
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
}
