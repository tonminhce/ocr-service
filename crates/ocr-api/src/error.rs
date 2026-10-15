use axum::{
    Json,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use ocr_core::store::StoreError;
use serde::Serialize;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: &'static str,
}

#[derive(Serialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Serialize)]
struct ErrorDetail {
    code: &'static str,
    message: &'static str,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: &'static str) -> Self {
        Self {
            status,
            code,
            message,
        }
    }

    pub fn bad_request(message: &'static str) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_request", message)
    }

    pub fn too_large() -> Self {
        Self::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "upload_too_large",
            "The uploaded file exceeds the configured size limit",
        )
    }
}

impl From<StoreError> for ApiError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::IdempotencyConflict => Self::new(
                StatusCode::CONFLICT,
                "idempotency_conflict",
                "This Idempotency-Key was already used for different file content or extraction settings",
            ),
            StoreError::ResponseTooLarge => Self::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "response_limit_exceeded",
                "Stored response exceeds the supported allocation limit",
            ),
            StoreError::EnqueueTimeout(_) => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "enqueue_timeout",
                "Enqueue deadline exceeded; the outcome may be unknown; retry with the same Idempotency-Key",
            ),
            StoreError::QueueFull => Self::new(
                StatusCode::TOO_MANY_REQUESTS,
                "queue_capacity_exceeded",
                "The OCR job queue is full; retry with the same Idempotency-Key",
            ),
            other => {
                tracing::error!(error = %other, "job storage operation failed");
                Self::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "storage_unavailable",
                    "Job storage is temporarily unavailable; retry with the same Idempotency-Key",
                )
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let retry = matches!(
            self.status,
            StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
        );
        let mut response = (
            self.status,
            Json(ErrorBody {
                error: ErrorDetail {
                    code: self.code,
                    message: self.message,
                },
            }),
        )
            .into_response();
        if retry {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, "1".parse().unwrap());
        }
        crate::response::secure(response)
    }
}
