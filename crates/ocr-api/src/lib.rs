mod auth;
pub mod config;
mod error;
pub mod response;
mod routes;
pub mod server;

use std::sync::Arc;

use axum::{
    Router,
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    middleware,
    routing::{get, post},
};
use ocr_core::store::Store;
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};

use config::Settings;
use error::ApiError;

#[derive(Clone)]
pub struct AppState {
    pub store: Store,
    pub settings: Arc<Settings>,
    pub upload_slots: Arc<tokio::sync::Semaphore>,
    pub responses: response::ResponseBudget,
    pub review_slots: Arc<tokio::sync::Semaphore>,
}

impl AppState {
    pub fn new(store: Store, settings: Arc<Settings>) -> Self {
        Self {
            store,
            responses: response::ResponseBudget::new(settings.responses.clone()),
            review_slots: Arc::new(tokio::sync::Semaphore::new(4)),
            upload_slots: Arc::new(tokio::sync::Semaphore::new(settings.upload_concurrency)),
            settings,
        }
    }
}

pub fn router(state: AppState) -> Router {
    let jobs = Router::new()
        .route("/v1/ocr/jobs", post(routes::jobs::submit))
        .route("/v1/ocr/jobs/{id}", get(routes::jobs::get_job))
        .route("/v1/ocr/jobs/{id}/status", get(routes::views::status))
        .route("/v1/ocr/jobs/{id}/business", get(routes::views::business))
        .route(
            "/v1/ocr/jobs/{id}/artifacts/{kind}",
            get(routes::views::artifact),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::authenticate,
        ));
    let reviews = Router::new()
        .route(
            "/v1/ocr/jobs/{id}/review",
            get(routes::views::review_history).post(routes::views::review),
        )
        .layer(DefaultBodyLimit::max(64 * 1024))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::authenticate_review,
        ));
    let body_limit = state.settings.max_upload_bytes + 64 * 1024;
    Router::new()
        .merge(jobs)
        .merge(reviews)
        .route("/health/live", get(|| async { StatusCode::OK }))
        .route("/health/ready", get(ready))
        .fallback(|| async {
            ApiError::new(StatusCode::NOT_FOUND, "route_not_found", "Route not found")
        })
        .method_not_allowed_fallback(|| async {
            ApiError::new(
                StatusCode::METHOD_NOT_ALLOWED,
                "method_not_allowed",
                "Method not allowed",
            )
        })
        .layer(DefaultBodyLimit::max(body_limit))
        .layer(middleware::from_fn(secure_response))
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(TraceLayer::new_for_http())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .with_state(state)
}

async fn ready(State(state): State<AppState>) -> Result<StatusCode, ApiError> {
    tokio::time::timeout(std::time::Duration::from_secs(2), state.store.health())
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "storage_unavailable",
                "Job storage readiness timed out",
            )
        })??;
    Ok(StatusCode::OK)
}

async fn secure_response(
    request: axum::extract::Request,
    next: middleware::Next,
) -> axum::response::Response {
    response::secure(next.run(request).await)
}
