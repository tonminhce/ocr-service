use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, Query, State, rejection::JsonRejection},
    http::{StatusCode, header},
    response::Response,
};
use ocr_core::{
    projection::{Artifact, JobStatus},
    review::{ReviewError, ReviewRequest},
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{AppState, error::ApiError};

fn id(raw: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(raw).map_err(|_| ApiError::bad_request("Job ID must be a UUID"))
}
fn missing() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "artifact_not_found",
        "Job or requested artifact was not found, is unavailable, or has expired",
    )
}
fn capacity(
    slots: Arc<tokio::sync::Semaphore>,
) -> Result<tokio::sync::OwnedSemaphorePermit, ApiError> {
    slots.try_acquire_owned().map_err(|_| {
        ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "read_capacity_exceeded",
            "Result or review capacity is busy; retry later",
        )
    })
}
pub async fn status(
    State(state): State<AppState>,
    Path(raw): Path<String>,
) -> Result<Response, ApiError> {
    let response = state.responses.small()?;
    let status: JobStatus = crate::response::deadline(
        state.settings.responses.read_timeout,
        state.store.get_status(id(&raw)?),
    )
    .await??
    .ok_or_else(missing)?;
    response.json(status).await
}

pub async fn business(
    State(state): State<AppState>,
    Path(raw): Path<String>,
) -> Result<Response, ApiError> {
    let response = state.responses.json()?;
    let business = crate::response::deadline(
        state.settings.responses.read_timeout,
        state
            .store
            .get_business_bounded(id(&raw)?, state.settings.responses.max_json_bytes),
    )
    .await?
    .map_err(review_error)?
    .ok_or_else(missing)?;
    response.json(business).await
}

pub async fn artifact(
    State(state): State<AppState>,
    Path((raw, kind)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    if !matches!(
        kind.as_str(),
        "source" | "markdown" | "layout" | "evidence" | "diagnostics" | "result"
    ) {
        return Err(missing());
    }
    let response = if kind == "source" {
        state.responses.source()?
    } else {
        state.responses.json()?
    };
    let artifact = crate::response::deadline(
        state.settings.responses.read_timeout,
        state.store.get_artifact_bounded(
            id(&raw)?,
            &kind,
            if kind == "source" {
                state.settings.responses.max_source_bytes
            } else {
                state.settings.responses.max_json_bytes
            },
        ),
    )
    .await??
    .ok_or_else(missing)?;
    let (content_type, filename, bytes) = match artifact {
        Artifact::Source { media_type, bytes } => {
            let filename = match media_type.as_str() {
                "application/pdf" => "document.pdf",
                "image/png" => "document.png",
                "image/jpeg" => "document.jpg",
                "image/webp" => "document.webp",
                _ => "document.bin",
            };
            (media_type, filename, bytes)
        }
        Artifact::Markdown(markdown) => (
            "text/markdown; charset=utf-8".into(),
            "document.md",
            markdown.into_bytes(),
        ),
        Artifact::Json(value) => {
            let mut result = response.json(value).await?;
            result.headers_mut().insert(
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"artifact.json\"".parse().unwrap(),
            );
            return Ok(result);
        }
    };
    let mut result = response.bytes(bytes, &content_type)?;
    result.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        format!("attachment; filename=\"{filename}\"")
            .parse()
            .expect("fixed filename"),
    );
    Ok(result)
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryQuery {
    #[serde(default)]
    after_revision: i64,
}

pub async fn review_history(
    State(state): State<AppState>,
    Path(raw): Path<String>,
    Query(query): Query<HistoryQuery>,
) -> Result<Response, ApiError> {
    let _permit = capacity(state.review_slots.clone())?;
    let response = state.responses.json()?;
    let history = crate::response::deadline(
        state.settings.responses.read_timeout,
        state.store.review_history(id(&raw)?, query.after_revision),
    )
    .await?
    .map_err(review_error)?;
    response.json(history).await
}

pub async fn review(
    State(state): State<AppState>,
    Path(raw): Path<String>,
    body: Result<Json<ReviewRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let _permit = capacity(state.review_slots.clone())?;
    let response = state.responses.json()?;
    let request = body
        .map_err(|error| {
            if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
                ApiError::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "review_too_large",
                    "Review request exceeds 64 KiB",
                )
            } else {
                ApiError::bad_request("Expected a valid review request JSON body")
            }
        })?
        .0;
    let principal = state
        .settings
        .review_principal
        .as_deref()
        .ok_or_else(missing)?;
    let snapshot = crate::response::deadline(
        state.settings.responses.read_timeout,
        state.store.append_review_bounded(
            id(&raw)?,
            principal,
            request,
            state.settings.responses.max_json_bytes,
        ),
    )
    .await?
    .map_err(review_error)?;
    response.json(snapshot).await
}

fn review_error(error: ReviewError) -> ApiError {
    match error {
        ReviewError::NotFound | ReviewError::Unavailable => missing(),
        ReviewError::ResponseTooLarge => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "response_limit_exceeded",
            "Stored response exceeds the supported allocation limit",
        ),
        ReviewError::Conflict => ApiError::new(
            StatusCode::CONFLICT,
            "review_conflict",
            "Review revision is stale or request_id was reused with a different request",
        ),
        ReviewError::Invalid(message) => {
            ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_review", message)
        }
        other => {
            tracing::error!(error=%other,"review or business projection failed");
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "review_unavailable",
                "Review storage or structured result is temporarily unavailable",
            )
        }
    }
}
