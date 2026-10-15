//! Real HTTP/1 TCP sockets exercise backpressure that Tower oneshot cannot.
use axum::{Router, response::IntoResponse, routing::get};
use ocr_api::{
    config::{ResponseSettings, TransportSettings},
    response::ResponseBudget,
    server,
};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

const PAYLOAD: usize = 16 * 1024 * 1024;
async fn fixture(
    transport: TransportSettings,
) -> (
    std::net::SocketAddr,
    tokio::sync::oneshot::Sender<()>,
    Arc<ResponseBudget>,
) {
    let budget = Arc::new(ResponseBudget::new(ResponseSettings {
        large_concurrency: 2,
        large_reserved_bytes: 2 * PAYLOAD,
        max_source_bytes: PAYLOAD,
        body_timeout: Duration::from_secs(10),
        ..Default::default()
    }));
    let app = Router::new()
        .route(
            "/large",
            get({
                let budget = budget.clone();
                move || {
                    let budget = budget.clone();
                    async move {
                        match budget.json() {
                            Ok(admission) => admission
                                .bytes(vec![b'x'; PAYLOAD], "application/octet-stream")
                                .unwrap(),
                            Err(error) => error.into_response(),
                        }
                    }
                }
            }),
        )
        .route(
            "/status",
            get({
                let budget = budget.clone();
                move || {
                    let budget = budget.clone();
                    async move {
                        match budget.small() {
                            Ok(admission) => admission
                                .json(serde_json::json!({"status":"running"}))
                                .await
                                .unwrap_or_else(IntoResponse::into_response),
                            Err(error) => error.into_response(),
                        }
                    }
                }
            }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        server::serve(listener, app, transport, async {
            let _ = stopped.await;
        })
        .await
        .unwrap();
    });
    (address, stop, budget)
}
async fn unread_large(address: std::net::SocketAddr) -> TcpStream {
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream
        .write_all(b"GET /large HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    let mut headers = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !headers.ends_with(b"\r\n\r\n") {
            headers.push(stream.read_u8().await.unwrap());
        }
    })
    .await
    .unwrap();
    assert!(headers.starts_with(b"HTTP/1.1 200"));
    stream
}
async fn request_get(address: std::net::SocketAddr, path: &str) -> reqwest::Response {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap()
        .get(format!("http://{address}{path}"))
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn slow_tcp_readers_are_bounded_status_progresses_and_disconnect_releases_slots() {
    let (address, stop, _) = fixture(TransportSettings::default()).await;
    let one = unread_large(address).await;
    let two = unread_large(address).await;
    for _ in 0..6 {
        let response = request_get(address, "/large").await;
        assert_eq!(response.status(), 429);
        assert_eq!(response.headers()["retry-after"], "1");
        assert_eq!(
            response.json::<serde_json::Value>().await.unwrap()["error"]["code"],
            "response_capacity_exceeded"
        );
        let response = request_get(address, "/status").await;
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.json::<serde_json::Value>().await.unwrap()["status"],
            "running"
        );
    }
    drop((one, two));
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let response = request_get(address, "/large").await;
            if response.status() == 200 {
                assert_eq!(response.bytes().await.unwrap().len(), PAYLOAD);
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let _ = stop.send(());
}

#[tokio::test]
async fn stalled_tcp_writes_expire_and_restore_large_capacity() {
    let (address, stop, _) = fixture(TransportSettings {
        write_stall_timeout: Duration::from_millis(150),
        ..Default::default()
    })
    .await;
    let one = unread_large(address).await;
    let two = unread_large(address).await;
    assert_eq!(request_get(address, "/large").await.status(), 429);
    tokio::time::sleep(Duration::from_millis(700)).await;
    let response = request_get(address, "/large").await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.bytes().await.unwrap().len(), PAYLOAD);
    drop((one, two));
    let _ = stop.send(());
}

#[tokio::test]
async fn connection_cap_and_header_deadline_release_partial_requests() {
    let (address, stop, _) = fixture(TransportSettings {
        max_connections: 2,
        header_timeout: Duration::from_millis(150),
        ..Default::default()
    })
    .await;
    let mut one = TcpStream::connect(address).await.unwrap();
    one.write_all(b"GET /status HTTP/1.1\r\n").await.unwrap();
    let mut two = TcpStream::connect(address).await.unwrap();
    two.write_all(b"GET /status HTTP/1.1\r\n").await.unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    let mut third = TcpStream::connect(address).await.unwrap();
    let mut byte = [0];
    let closed = tokio::time::timeout(Duration::from_millis(100), third.read(&mut byte))
        .await
        .unwrap();
    assert!(matches!(closed, Ok(0) | Err(_)));
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(request_get(address, "/status").await.status(), 200);
    drop((one, two));
    let _ = stop.send(());
}

#[tokio::test]
#[ignore = "explicit RSS diagnostic; run alone with --ignored --nocapture --test-threads=1"]
async fn tcp_backpressure_rss_diagnostic() {
    fn rss_kib() -> u64 {
        let output = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()
            .unwrap();
        assert!(output.status.success());
        std::str::from_utf8(&output.stdout)
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }
    let (address, stop, budget) = fixture(TransportSettings::default()).await;
    let baseline = rss_kib();
    let mut held_samples = Vec::new();
    let mut drained_samples = Vec::new();
    for _ in 0..12 {
        let one = unread_large(address).await;
        let two = unread_large(address).await;
        for _ in 0..4 {
            assert_eq!(request_get(address, "/large").await.status(), 429);
            assert_eq!(request_get(address, "/status").await.status(), 200);
        }
        held_samples.push(rss_kib());
        drop((one, two));
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(first) = budget.json()
                    && let Ok(second) = budget.json()
                {
                    drop((first, second));
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        drained_samples.push(rss_kib());
    }
    let tail = &held_samples[4..];
    let tail_spread = tail.iter().max().unwrap() - tail.iter().min().unwrap();
    eprintln!(
        "api_transport_rss_diagnostic={}",
        serde_json::json!({
            "pid":std::process::id(), "baseline_kib":baseline,
            "held_kib":held_samples, "drained_kib":drained_samples,
            "steady_tail_spread_kib":tail_spread,
            "cycles":12,"large_payload_bytes":PAYLOAD,"simultaneous_large_bodies":2,
            "scope":"single-process synthetic socket fixture; not whole-deployment capacity"
        })
    );
    assert!(
        tail_spread < 64 * 1024,
        "steady allocations did not plateau within64MiB diagnostic allowance"
    );
    let _ = stop.send(());
}
