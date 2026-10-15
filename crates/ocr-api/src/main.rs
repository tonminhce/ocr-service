use std::sync::Arc;

use anyhow::Result;
use axum::{
    extract::{MatchedPath, Request},
    middleware::{self, Next},
    response::Response,
};
use ocr_api::{AppState, config::Settings, router};
use ocr_core::{
    metrics::{HttpRoute, MetricsEndpoint, process_metrics},
    store::Store,
};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,tower_http=info".into()),
        )
        .init();
    let _ = process_metrics();
    let settings = Arc::new(Settings::from_env()?);
    let store = Store::connect(&settings.database_url).await?;
    store.check_schema().await?;
    let endpoint = MetricsEndpoint::from_env("API_METRICS_BIND_ADDR").await?;
    let listener = tokio::net::TcpListener::bind(settings.bind_addr).await?;
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let signal_stop = stop.clone();
    tokio::spawn(async move {
        shutdown().await;
        let _ = signal_stop.send(true);
    });
    let metrics_task = endpoint.map(|endpoint| {
        let mut receiver = receiver.clone();
        let metrics_stop = stop.clone();
        tracing::info!(address = %endpoint.local_addr().expect("bound metrics socket"), "API metrics listening");
        tokio::spawn(async move {
            let result = endpoint.serve(async move { let _ = receiver.wait_for(|stopped| *stopped).await; }).await;
            if result.is_err() { let _ = metrics_stop.send(true); }
            result
        })
    });
    tracing::info!(address = %listener.local_addr()?, "OCR API listening");
    let mut receiver = receiver;
    let transport = settings.transport.clone();
    let result = ocr_api::server::serve(
        listener,
        router(AppState::new(store, settings)).layer(middleware::from_fn(observe_request)),
        transport,
        async move {
            let _ = receiver.wait_for(|stopped| *stopped).await;
        },
    )
    .await;
    let _ = stop.send(true);
    if let Some(task) = metrics_task {
        task.await??;
    }
    result?;
    Ok(())
}

async fn observe_request(request: Request, next: Next) -> Response {
    let route = HttpRoute::from_matched_path(
        request
            .extensions()
            .get::<MatchedPath>()
            .map(MatchedPath::as_str),
    );
    let started = std::time::Instant::now();
    let response = next.run(request).await;
    process_metrics().http_request(route, response.status(), started.elapsed());
    response
}

async fn shutdown() {
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
    tracing::info!("OCR API shutting down");
}
