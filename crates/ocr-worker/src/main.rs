use ocr_core::{
    metrics::{MetricsEndpoint, process_metrics},
    store::Store,
};
use ocr_worker::{queue::RedisQueue, settings::Settings, worker};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .json()
        .init();
    let _ = process_metrics();
    let settings = Settings::from_env()?;
    let parser = ocr_worker::parser_client::health_check(
        &settings.parser_socket_path,
        std::time::Duration::from_secs(2),
    )
    .await?;
    // More local jobs than parser sessions would turn normal load into Busy
    // retries, so the deployment must size the parser for this worker.
    if settings.worker_concurrency > parser.max_sessions as usize {
        anyhow::bail!(
            "WORKER_CONCURRENCY {} exceeds isolated parser capacity {}",
            settings.worker_concurrency,
            parser.max_sessions
        );
    }
    tracing::info!(
        parser_max_sessions = parser.max_sessions,
        "isolated parser ready"
    );
    let store = Store::connect(&settings.database_url).await?;
    store.check_schema().await?;
    store.health().await?;
    let shutdown = CancellationToken::new();
    let endpoint = MetricsEndpoint::from_env("WORKER_METRICS_BIND_ADDR").await?;
    let observer = ocr_worker::observer::spawn(
        store.clone(),
        RedisQueue::new(&settings).map_err(|_| anyhow::anyhow!("Redis observer setup failed"))?,
        shutdown.clone(),
    );
    let metrics_task = endpoint.map(|endpoint| {
        let stop = shutdown.clone();
        tracing::info!(address = %endpoint.local_addr().expect("bound metrics socket"), "worker metrics listening");
        tokio::spawn(async move {
            let result = endpoint.serve(stop.clone().cancelled_owned()).await;
            if result.is_err() { stop.cancel(); }
            result
        })
    });
    let signal = shutdown.clone();
    tokio::spawn(async move {
        shutdown_signal().await;
        signal.cancel();
    });
    let result = worker::run(store, settings, shutdown.clone()).await;
    shutdown.cancel();
    observer.await?;
    if let Some(task) = metrics_task {
        task.await??;
    }
    result
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
