use std::time::Duration;

use anyhow::{Result, bail};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .json()
        .init();

    let socket_path = ocr_worker::parser_service::socket_path_from_env()?;
    let mut args = std::env::args_os().skip(1);
    match (args.next(), args.next()) {
        (Some(argument), None) if argument == "--healthcheck" => {
            ocr_worker::parser_client::health_check(&socket_path, Duration::from_secs(2)).await?;
            Ok(())
        }
        (None, None) => {
            let scratch_root = ocr_worker::parser_service::scratch_root_from_env()?;
            let limits = ocr_worker::parser_service::ServiceLimits::from_env()?;
            let shutdown = CancellationToken::new();
            let signal = shutdown.clone();
            tokio::spawn(async move {
                shutdown_signal().await;
                signal.cancel();
            });
            tracing::info!(
                max_sessions = limits.max_sessions,
                "isolated parser listening"
            );
            ocr_worker::parser_service::serve(&socket_path, &scratch_root, limits, shutdown)
                .await?;
            Ok(())
        }
        _ => bail!("parser-renderer accepts no arguments or --healthcheck"),
    }
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
