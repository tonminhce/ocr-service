use std::{
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use futures_util::StreamExt;
use ocr_core::{
    metrics::{DependencySnapshot, Metrics, OperationalSnapshot, RuntimeSnapshot, process_metrics},
    store::Store,
};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::{parser_client, queue::RedisQueue, settings::Settings};

const SNAPSHOT_INTERVAL: Duration = Duration::from_secs(30);
const SNAPSHOT_BUDGET: Duration = Duration::from_secs(5);
const DEPENDENCY_BUDGET: Duration = Duration::from_secs(2);
const MODEL_CATALOG_LIMIT: usize = 64 * 1024;

struct ModelProbe {
    client: reqwest::Client,
    endpoint: String,
    model: String,
}

impl ModelProbe {
    fn new(
        base_url: &str,
        model: &str,
        key: Option<&str>,
        budget: Duration,
    ) -> anyhow::Result<Self> {
        let mut headers = HeaderMap::new();
        if let Some(key) = key {
            let mut value = HeaderValue::from_str(&format!("Bearer {key}"))
                .map_err(|_| anyhow::anyhow!("invalid model probe credential format"))?;
            value.set_sensitive(true);
            headers.insert(AUTHORIZATION, value);
        }
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(budget.min(Duration::from_secs(1)))
            .timeout(budget)
            .pool_max_idle_per_host(1)
            .build()
            .map_err(|_| anyhow::anyhow!("cannot configure bounded model probe"))?;
        Ok(Self {
            client,
            endpoint: format!("{}/models", base_url.trim_end_matches('/')),
            model: model.into(),
        })
    }

    async fn available(&self) -> bool {
        let Ok(response) = self.client.get(&self.endpoint).send().await else {
            return false;
        };
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|size| size > MODEL_CATALOG_LIMIT as u64)
        {
            return false;
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let Ok(chunk) = chunk else {
                return false;
            };
            if chunk.len() > MODEL_CATALOG_LIMIT.saturating_sub(body.len()) {
                return false;
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|value| {
                value
                    .get("data")
                    .and_then(|data| data.as_array())
                    .map(|models| {
                        models.iter().any(|model| {
                            model.get("id").and_then(|id| id.as_str()) == Some(self.model.as_str())
                        })
                    })
            })
            .unwrap_or(false)
    }
}

struct DependencyProbe {
    parser_socket: PathBuf,
    model: ModelProbe,
}

fn observed(up: bool, started: Instant) -> DependencySnapshot {
    DependencySnapshot {
        up,
        checked_at_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        elapsed: started.elapsed(),
    }
}

impl DependencyProbe {
    async fn snapshot(&self, store: &Store) -> RuntimeSnapshot {
        let parser = async {
            let started = Instant::now();
            let capacity = tokio::time::timeout(
                DEPENDENCY_BUDGET,
                parser_client::health_check(&self.parser_socket, DEPENDENCY_BUDGET),
            )
            .await
            .ok()
            .and_then(Result::ok);
            (
                observed(capacity.is_some(), started),
                capacity.map(|capacity| (capacity.active_sessions, capacity.max_sessions)),
            )
        };
        let model = async {
            let started = Instant::now();
            observed(self.model.available().await, started)
        };
        let ((parser, parser_sessions), model_catalog) = tokio::join!(parser, model);
        RuntimeSnapshot {
            sampled_at_seconds: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            parser,
            parser_sessions,
            model_catalog,
            database_pool: store.pool_snapshot(),
        }
    }
}

async fn collect(store: &Store, queue: &RedisQueue, probes: &DependencyProbe, metrics: &Metrics) {
    let operational = tokio::time::timeout(SNAPSHOT_BUDGET, async {
        let database = async { store.operational_snapshot().await.map_err(|_| ()) };
        let redis = async { queue.operational_snapshot().await.map_err(|_| ()) };
        tokio::try_join!(database, redis)
    });
    let (snapshot, runtime) = tokio::join!(operational, probes.snapshot(store));
    metrics.runtime_snapshot(runtime);
    match snapshot {
        Ok(Ok((database, (pending_entries, memory_bytes, maxmemory_bytes)))) => {
            metrics.operational_snapshot(OperationalSnapshot {
                redis_pending_entries: pending_entries,
                redis_memory_bytes: memory_bytes,
                redis_maxmemory_bytes: maxmemory_bytes,
                ..database
            });
        }
        Ok(Err(())) | Err(_) => {
            metrics.operational_collector_failed();
            tracing::warn!("operational metrics snapshot failed");
        }
    }
}

pub fn spawn(
    store: Store,
    queue: RedisQueue,
    settings: &Settings,
    shutdown: CancellationToken,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    let probes = DependencyProbe {
        parser_socket: settings.parser_socket_path.clone(),
        model: ModelProbe::new(
            &settings.vllm_base_url,
            &settings.vllm_model,
            settings.vllm_api_key.as_deref(),
            DEPENDENCY_BUDGET,
        )?,
    };
    let metrics = process_metrics();
    metrics.enable_operational_collector();
    metrics.runtime_snapshot(RuntimeSnapshot::default());
    Ok(tokio::spawn(async move {
        let mut interval = tokio::time::interval(SNAPSHOT_INTERVAL);
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => return,
                _ = interval.tick() => {}
            }

            tokio::select! {
                biased;
                _ = shutdown.cancelled() => return,
                _ = collect(&store, &queue, &probes, metrics) => {}
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use axum::{
        Router,
        body::Body,
        http::{HeaderMap, Response, StatusCode},
        routing::get,
    };
    use tokio::{
        net::{TcpListener, UnixListener},
        sync::Barrier,
        task::JoinHandle,
    };

    use super::*;
    use crate::parser_protocol::{
        Frame, FrameKind, Operation, REQUEST_DATA_LIMIT, read_frame, write_frame,
    };

    struct Server {
        base: String,
        task: JoinHandle<()>,
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn server(router: Router) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Server { base, task }
    }

    #[tokio::test]
    async fn authenticated_catalog_and_scrapes_do_not_generate_probe_requests() {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let http = server(Router::new().route(
            "/v1/models",
            get(move |headers: HeaderMap| {
                let seen = seen.clone();
                async move {
                    assert_eq!(headers[AUTHORIZATION], "Bearer synthetic-probe-key");
                    seen.fetch_add(1, Ordering::SeqCst);
                    r#"{"data":[{"id":"fixture-model"}]}"#
                }
            }),
        ))
        .await;
        let probe = ModelProbe::new(
            &http.base,
            "fixture-model",
            Some("synthetic-probe-key"),
            DEPENDENCY_BUDGET,
        )
        .unwrap();
        assert!(probe.available().await);
        let metrics = Metrics::new();
        metrics.runtime_snapshot(RuntimeSnapshot::default());
        for _ in 0..100 {
            assert!(metrics.render().contains("ocr_dependency_up"));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn unhealthy_malformed_oversized_and_unadvertised_catalogs_fail_closed() {
        for (status, body) in [
            (
                StatusCode::UNAUTHORIZED,
                r#"{"data":[{"id":"fixture-model"}]}"#.to_owned(),
            ),
            (
                StatusCode::OK,
                r#"{"data":[{"id":"other-model"}]}"#.to_owned(),
            ),
            (StatusCode::OK, "not-json".to_owned()),
            (StatusCode::OK, "x".repeat(MODEL_CATALOG_LIMIT + 1)),
        ] {
            let http = server(Router::new().route(
                "/v1/models",
                get(move || {
                    let body = body.clone();
                    async move { (status, body) }
                }),
            ))
            .await;
            let probe =
                ModelProbe::new(&http.base, "fixture-model", None, DEPENDENCY_BUDGET).unwrap();
            assert!(!probe.available().await);
        }
    }

    #[tokio::test]
    async fn redirects_never_forward_model_credentials() {
        let forwarded = Arc::new(AtomicUsize::new(0));
        let received = forwarded.clone();
        let destination = server(Router::new().route(
            "/v1/models",
            get(move || {
                received.fetch_add(1, Ordering::SeqCst);
                async { r#"{"data":[{"id":"fixture-model"}]}"# }
            }),
        ))
        .await;
        let location = format!("{}/models", destination.base);
        let redirect = server(Router::new().route(
            "/v1/models",
            get(move || {
                let location = location.clone();
                async move {
                    Response::builder()
                        .status(302)
                        .header("Location", location)
                        .body(Body::empty())
                        .unwrap()
                }
            }),
        ))
        .await;
        let probe = ModelProbe::new(
            &redirect.base,
            "fixture-model",
            Some("synthetic-probe-key"),
            DEPENDENCY_BUDGET,
        )
        .unwrap();
        assert!(!probe.available().await);
        assert_eq!(forwarded.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn stalled_catalog_is_deadline_bounded() {
        let http = server(Router::new().route(
            "/v1/models",
            get(|| async { std::future::pending::<String>().await }),
        ))
        .await;
        let probe =
            ModelProbe::new(&http.base, "fixture-model", None, Duration::from_millis(50)).unwrap();
        assert!(
            !tokio::time::timeout(Duration::from_secs(1), probe.available())
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn chunked_overflow_is_rejected_before_waiting_for_end_of_body() {
        let http = server(Router::new().route(
            "/v1/models",
            get(|| async {
                let chunks = futures_util::stream::iter([
                    Ok::<_, std::io::Error>(axum::body::Bytes::from(vec![
                        b'x';
                        MODEL_CATALOG_LIMIT
                    ])),
                    Ok(axum::body::Bytes::from_static(b"x")),
                ])
                .chain(futures_util::stream::pending());
                Response::builder().body(Body::from_stream(chunks)).unwrap()
            }),
        ))
        .await;
        let probe = ModelProbe::new(&http.base, "fixture-model", None, DEPENDENCY_BUDGET).unwrap();
        assert!(
            !tokio::time::timeout(Duration::from_secs(1), probe.available())
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn headers_followed_by_a_stalled_body_are_deadline_bounded() {
        let http = server(Router::new().route(
            "/v1/models",
            get(|| async {
                let chunks = futures_util::stream::once(async {
                    Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b"{"))
                })
                .chain(futures_util::stream::pending());
                Response::builder().body(Body::from_stream(chunks)).unwrap()
            }),
        ))
        .await;
        let probe =
            ModelProbe::new(&http.base, "fixture-model", None, Duration::from_millis(50)).unwrap();
        assert!(
            !tokio::time::timeout(Duration::from_secs(1), probe.available())
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn dropping_a_probe_cancels_its_response_body_without_a_detached_request() {
        struct Dropped(Option<tokio::sync::oneshot::Sender<()>>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                if let Some(send) = self.0.take() {
                    let _ = send.send(());
                }
            }
        }
        let (send, dropped) = tokio::sync::oneshot::channel();
        let state = Arc::new(std::sync::Mutex::new(Some(send)));
        let (ready, headers) = tokio::sync::oneshot::channel();
        let ready = Arc::new(std::sync::Mutex::new(Some(ready)));
        let http = server(Router::new().route(
            "/v1/models",
            get(move || {
                let guard = Dropped(state.lock().unwrap().take());
                if let Some(ready) = ready.lock().unwrap().take() {
                    let _ = ready.send(());
                }
                async move {
                    let chunks = futures_util::stream::once(async {
                        Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b"{"))
                    })
                    .chain(futures_util::stream::once(async move {
                        let _guard = guard;
                        std::future::pending::<Result<axum::body::Bytes, std::io::Error>>().await
                    }));
                    Response::builder().body(Body::from_stream(chunks)).unwrap()
                }
            }),
        ))
        .await;
        let probe = ModelProbe::new(&http.base, "fixture-model", None, DEPENDENCY_BUDGET).unwrap();
        let task = tokio::spawn(async move { probe.available().await });
        headers.await.unwrap();
        tokio::task::yield_now().await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        tokio::time::timeout(Duration::from_secs(1), dropped)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn parser_and_catalog_probes_progress_concurrently_without_database_io() {
        let barrier = Arc::new(Barrier::new(2));
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("parser.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let peer = barrier.clone();
        let parser = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let frame = read_frame(&mut stream, FrameKind::Request, REQUEST_DATA_LIMIT)
                .await
                .unwrap();
            assert!(matches!(frame.operation, Operation::HealthCheck));
            peer.wait().await;
            write_frame(
                &mut stream,
                FrameKind::Response,
                &Frame {
                    operation: Operation::HealthReady {
                        max_sessions: 8,
                        active_sessions: 2,
                    },
                    data: vec![],
                },
                0,
            )
            .await
            .unwrap();
        });
        let peer = barrier.clone();
        let http = server(Router::new().route(
            "/v1/models",
            get(move || {
                let peer = peer.clone();
                async move {
                    peer.wait().await;
                    r#"{"data":[{"id":"fixture-model"}]}"#
                }
            }),
        ))
        .await;
        let probes = DependencyProbe {
            parser_socket: socket,
            model: ModelProbe::new(&http.base, "fixture-model", None, DEPENDENCY_BUDGET).unwrap(),
        };
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(3)
            .connect_lazy("postgres://unused:synthetic@127.0.0.1:1/unused")
            .unwrap();
        let store = Store::from_pool(pool);
        let snapshot = tokio::time::timeout(Duration::from_secs(1), probes.snapshot(&store))
            .await
            .unwrap();
        assert!(snapshot.parser.up && snapshot.model_catalog.up);
        assert_eq!(snapshot.parser_sessions, Some((2, 8)));
        assert_eq!(snapshot.database_pool.size, 0);
        assert_eq!(snapshot.database_pool.max, 3);
        assert!(snapshot.parser.checked_at_seconds > 0);
        assert!(snapshot.sampled_at_seconds >= snapshot.parser.checked_at_seconds);
        parser.await.unwrap();
    }
}
