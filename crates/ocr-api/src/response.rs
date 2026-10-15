//! Response allocations are admitted before database reads. CPU and output
//! reservations move into the blocking closure, then into the response body.
use std::{
    io::{self, Write},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use axum::{
    body::Body,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use bytes::{Buf, Bytes};
use http_body::{Frame, SizeHint};
use serde::Serialize;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Sleep;

use crate::{config::ResponseSettings, error::ApiError};

const CHUNK_BYTES: usize = 16 * 1024;

#[derive(Clone)]
pub struct ResponseBudget {
    settings: Arc<ResponseSettings>,
    small: Arc<Semaphore>,
    large: Arc<Semaphore>,
    bytes: Arc<Semaphore>,
    small_cpu: Arc<Semaphore>,
    large_cpu: Arc<Semaphore>,
}

impl ResponseBudget {
    pub fn new(settings: ResponseSettings) -> Self {
        Self {
            small: Arc::new(Semaphore::new(settings.small_concurrency)),
            large: Arc::new(Semaphore::new(settings.large_concurrency)),
            bytes: Arc::new(Semaphore::new(settings.large_reserved_bytes)),
            small_cpu: Arc::new(Semaphore::new(2)),
            large_cpu: Arc::new(Semaphore::new(settings.serialization_concurrency)),
            settings: Arc::new(settings),
        }
    }

    pub fn small(&self) -> Result<Admission, ApiError> {
        self.admit(
            self.small.clone(),
            self.small_cpu.clone(),
            self.settings.max_small_bytes,
            false,
        )
    }

    pub fn json(&self) -> Result<Admission, ApiError> {
        self.admit(
            self.large.clone(),
            self.large_cpu.clone(),
            self.settings.max_json_bytes,
            true,
        )
    }

    pub fn source(&self) -> Result<Admission, ApiError> {
        self.admit(
            self.large.clone(),
            self.large_cpu.clone(),
            self.settings.max_source_bytes,
            true,
        )
    }

    fn admit(
        &self,
        slots: Arc<Semaphore>,
        cpu: Arc<Semaphore>,
        cap: usize,
        large: bool,
    ) -> Result<Admission, ApiError> {
        let slot = slots.try_acquire_owned().map_err(|_| capacity())?;
        // Reserve the maximum before fetching, not merely bytes observed after
        // a potentially large read. Small responses have a separate fixed budget.
        let bytes = if large {
            Some(
                self.bytes
                    .clone()
                    .try_acquire_many_owned(cap as u32)
                    .map_err(|_| capacity())?,
            )
        } else {
            None
        };
        Ok(Admission {
            _slot: slot,
            _bytes: bytes,
            cpu,
            cap,
            timeout: self.settings.body_timeout,
        })
    }
}

pub struct Admission {
    _slot: OwnedSemaphorePermit,
    _bytes: Option<OwnedSemaphorePermit>,
    cpu: Arc<Semaphore>,
    cap: usize,
    timeout: std::time::Duration,
}

impl Admission {
    pub async fn json<T: Serialize + Send + 'static>(self, value: T) -> Result<Response, ApiError> {
        // This queue is bounded by the previously acquired response slots and
        // byte reservations. Waiting avoids rejecting a committed mutation just
        // because another admitted serializer momentarily owns a CPU slot.
        let cpu = self
            .cpu
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| capacity())?;
        let (bytes, admission) = tokio::task::spawn_blocking(move || {
            // Aborting the waiting request cannot release either CPU or output
            // reservations while this synchronous serializer is still running.
            let _cpu = cpu;
            let mut writer = BoundedWriter {
                bytes: Vec::new(),
                cap: self.cap,
                exceeded: false,
            };
            let result = serde_json::to_writer(&mut writer, &value);
            if writer.exceeded {
                return Err(limit());
            }
            result.map_err(|_| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "serialization_failed",
                    "Response serialization failed",
                )
            })?;
            Ok((writer.bytes, self))
        })
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "serialization_failed",
                "Response serialization failed",
            )
        })??;
        admission.bytes(bytes, "application/json")
    }

    pub fn bytes(mut self, bytes: Vec<u8>, content_type: &str) -> Result<Response, ApiError> {
        if bytes.len() > self.cap || bytes.capacity() > self.cap {
            return Err(limit());
        }
        // Fetch/serialization reserved the worst case. Retain only backing
        // allocation bytes during egress, so small source files do not each
        // monopolize a50-MiB reservation for a slow client's entire lifetime.
        if let Some(reservation) = self._bytes.as_mut() {
            let unused = reservation.num_permits().saturating_sub(bytes.capacity());
            drop(reservation.split(unused));
        }
        let len = bytes.len();
        let deadline = Box::pin(tokio::time::sleep(self.timeout));
        let mut response = Response::new(Body::new(AdmittedBody {
            bytes: Bytes::from(bytes),
            admission: Some(self),
            deadline,
        }));
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            content_type.parse().map_err(|_| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "invalid_content_type",
                    "Stored artifact content type is invalid",
                )
            })?,
        );
        response
            .headers_mut()
            .insert(header::CONTENT_LENGTH, len.into());
        Ok(secure(response))
    }
}

struct BoundedWriter {
    bytes: Vec<u8>,
    cap: usize,
    exceeded: bool,
}
impl Write for BoundedWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.len() > self.cap.saturating_sub(self.bytes.len()) {
            self.exceeded = true;
            return Err(io::Error::other("response limit exceeded"));
        }
        // Vec's geometric growth may exceed the cap even when len does not.
        // Reserve exactly the required increment to cap backing capacity too.
        if self.bytes.len() + buf.len() > self.bytes.capacity() {
            let needed = self.bytes.len() + buf.len();
            let target = self
                .bytes
                .capacity()
                .max(1024)
                .saturating_mul(2)
                .max(needed)
                .min(self.cap);
            self.bytes.reserve_exact(target - self.bytes.len());
        }
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct AdmittedBody {
    bytes: Bytes,
    admission: Option<Admission>,
    deadline: Pin<Box<Sleep>>,
}
impl http_body::Body for AdmittedBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        if self.bytes.is_empty() {
            self.admission.take();
            return Poll::Ready(None);
        }
        if self.deadline.as_mut().poll(cx).is_ready() {
            self.bytes = Bytes::new();
            self.admission.take();
            return Poll::Ready(Some(Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "response body deadline exceeded",
            ))));
        }
        let len = self.bytes.len().min(CHUNK_BYTES);
        // Copy bounded chunks: a trailing Hyper/socket buffer must not retain
        // the entire large Bytes allocation after its admission is released.
        let chunk = Bytes::copy_from_slice(&self.bytes[..len]);
        self.bytes.advance(len);
        Poll::Ready(Some(Ok(Frame::data(chunk))))
    }
    fn is_end_stream(&self) -> bool {
        self.bytes.is_empty()
    }
    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.bytes.len() as u64)
    }
}

pub fn secure(response: impl IntoResponse) -> Response {
    let mut response = response.into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "private, no-store".parse().unwrap());
    response
        .headers_mut()
        .insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    response
}

fn capacity() -> ApiError {
    ApiError::new(
        StatusCode::TOO_MANY_REQUESTS,
        "response_capacity_exceeded",
        "Response capacity is busy; retry later",
    )
}
fn limit() -> ApiError {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "response_limit_exceeded",
        "Stored response exceeds the supported wire limit",
    )
}

/// Total asynchronous storage/projection deadline includes pool acquisition.
/// SQL readers independently apply transaction-local statement/lock limits.
pub async fn deadline<T>(
    duration: std::time::Duration,
    future: impl std::future::Future<Output = T>,
) -> Result<T, ApiError> {
    tokio::time::timeout(duration, future).await.map_err(|_| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "operation_timeout", "Storage operation deadline exceeded; a mutation outcome may be unknown; retry with the same request or idempotency key"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use serde::ser::SerializeSeq;
    use std::sync::{
        Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    fn budget() -> ResponseBudget {
        ResponseBudget::new(ResponseSettings {
            small_concurrency: 1,
            large_concurrency: 2,
            serialization_concurrency: 1,
            max_small_bytes: 1024,
            max_json_bytes: 1024,
            max_source_bytes: 1024,
            large_reserved_bytes: 2048,
            body_timeout: std::time::Duration::from_millis(30),
            read_timeout: std::time::Duration::from_secs(1),
        })
    }

    struct Many(Arc<AtomicUsize>);
    impl Serialize for Many {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let mut seq = serializer.serialize_seq(None)?;
            for _ in 0..100_000 {
                self.0.fetch_add(1, Ordering::SeqCst);
                seq.serialize_element(&"large-value")?;
            }
            seq.end()
        }
    }

    #[tokio::test]
    async fn writer_stops_during_serialization_and_releases_reservations() {
        let budget = budget();
        let visited = Arc::new(AtomicUsize::new(0));
        let error = budget
            .json()
            .unwrap()
            .json(Many(visited.clone()))
            .await
            .unwrap_err();
        assert_eq!(error.code, "response_limit_exceeded");
        assert!(visited.load(Ordering::SeqCst) < 100);
        assert_eq!(budget.bytes.available_permits(), 2048);
        assert_eq!(budget.large_cpu.available_permits(), 1);
        let mut writer = BoundedWriter {
            bytes: Vec::new(),
            cap: 1700,
            exceeded: false,
        };
        for _ in 0..1700 {
            writer.write_all(b"x").unwrap();
        }
        assert!(writer.bytes.capacity() <= 1700);
        assert!(writer.write_all(b"x").is_err());
    }

    struct Blocking {
        entered: std::sync::mpsc::Sender<()>,
        gate: Arc<(Mutex<bool>, Condvar)>,
    }
    impl Serialize for Blocking {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            self.entered.send(()).unwrap();
            let (lock, cv) = &*self.gate;
            let _guard = cv
                .wait_while(lock.lock().unwrap(), |ready| !*ready)
                .unwrap();
            serializer.serialize_str("finished")
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_waiter_does_not_release_running_cpu_or_output_reservation() {
        let budget = budget();
        let (tx, rx) = std::sync::mpsc::channel();
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let admission = budget.json().unwrap();
        let gate2 = gate.clone();
        let task = tokio::spawn(async move {
            admission
                .json(Blocking {
                    entered: tx,
                    gate: gate2,
                })
                .await
        });
        rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        task.abort();
        let _ = task.await;
        assert_eq!(budget.large_cpu.available_permits(), 0);
        assert_eq!(budget.bytes.available_permits(), 1024);
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(20),
                budget.json().unwrap().json("second")
            )
            .await
            .is_err()
        );
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while budget.large_cpu.available_permits() != 1
                || budget.bytes.available_permits() != 2048
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn body_holds_output_budget_until_consumption_drop_or_deadline() {
        let budget = budget();
        let one = budget
            .json()
            .unwrap()
            .bytes(vec![1; 1024], "application/octet-stream")
            .unwrap();
        let two = budget
            .json()
            .unwrap()
            .bytes(vec![2; 1024], "application/octet-stream")
            .unwrap();
        assert!(budget.json().is_err());
        // Large outputs cannot consume the compact status reserve.
        let status = budget
            .small()
            .unwrap()
            .json(serde_json::json!({"status":"running"}))
            .await
            .unwrap();
        assert_eq!(status.status(), StatusCode::OK);
        drop(status);
        drop(one);
        assert_eq!(budget.bytes.available_permits(), 1024);
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        assert!(two.into_body().collect().await.is_err());
        assert_eq!(budget.bytes.available_permits(), 2048);
    }

    #[tokio::test]
    async fn egress_accounts_backing_capacity_without_retaining_worst_case_reservation() {
        let budget = budget();
        let response = budget
            .json()
            .unwrap()
            .bytes(vec![1; 100], "application/octet-stream")
            .unwrap();
        assert_eq!(budget.bytes.available_permits(), 1948);
        let other = budget
            .json()
            .unwrap()
            .bytes(vec![2; 100], "application/octet-stream")
            .unwrap();
        assert!(budget.json().is_err()); // body-count cap still applies
        drop((response, other));
        assert_eq!(budget.bytes.available_permits(), 2048);
        let mut hidden = Vec::with_capacity(4096);
        hidden.push(1);
        assert_eq!(
            budget
                .json()
                .unwrap()
                .bytes(hidden, "application/octet-stream")
                .unwrap_err()
                .code,
            "response_limit_exceeded"
        );
        assert_eq!(budget.bytes.available_permits(), 2048);
    }

    #[tokio::test]
    async fn serialized_json_and_content_headers_preserve_contract() {
        let budget = budget();
        let value = serde_json::json!({"id":"existing-job","result":{"markdown":"Tiếng Việt"},"error":null});
        let expected = serde_json::to_vec(&value).unwrap();
        let response = budget.json().unwrap().json(value).await.unwrap();
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "private, no-store"
        );
        assert_eq!(
            response.headers()[header::X_CONTENT_TYPE_OPTIONS],
            "nosniff"
        );
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
        assert_eq!(
            response.headers()[header::CONTENT_LENGTH],
            expected.len().to_string()
        );
        assert_eq!(
            &response.into_body().collect().await.unwrap().to_bytes()[..],
            &expected
        );
    }
}
