use std::time::Duration;

use ocr_core::{
    metrics::{OperationalSnapshot, process_metrics},
    store::Store,
};
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::queue::RedisQueue;

const SNAPSHOT_INTERVAL: Duration = Duration::from_secs(30);
const SNAPSHOT_BUDGET: Duration = Duration::from_secs(5);

pub fn spawn(
    store: Store,
    queue: RedisQueue,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    let metrics = process_metrics();
    metrics.enable_operational_collector();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(SNAPSHOT_INTERVAL);
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => return,
                _ = interval.tick() => {}
            }

            let snapshot = tokio::time::timeout(SNAPSHOT_BUDGET, async {
                let database = async { store.operational_snapshot().await.map_err(|_| ()) };
                let redis = async { queue.operational_snapshot().await.map_err(|_| ()) };
                tokio::try_join!(database, redis)
            })
            .await;

            match snapshot {
                Ok(Ok((database, (pending_entries, memory_bytes)))) => {
                    process_metrics().operational_snapshot(OperationalSnapshot {
                        redis_pending_entries: pending_entries,
                        redis_memory_bytes: memory_bytes,
                        ..database
                    });
                }
                Ok(Err(())) | Err(_) => {
                    process_metrics().operational_collector_failed();
                    tracing::warn!("operational metrics snapshot failed");
                }
            }
        }
    })
}
