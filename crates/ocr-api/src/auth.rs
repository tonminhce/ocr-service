use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::Response,
};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::{AppState, error::ApiError};

pub async fn authenticate(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let supplied = request
        .headers()
        .get("authorization")
        .and_then(|header| header.to_str().ok())
        .and_then(|header| header.strip_prefix("Bearer "))
        .unwrap_or("");
    if !token_matches(supplied, &state.settings.api_token) {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "A valid service bearer token is required",
        ));
    }
    Ok(next.run(request).await)
}

/// Privileged review routes deliberately do not accept the OCR service token.
pub async fn authenticate_review(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let expected = state.settings.review_token.as_deref().ok_or_else(|| {
        ApiError::new(StatusCode::NOT_FOUND, "route_not_found", "Route not found")
    })?;
    let supplied = request
        .headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .unwrap_or("");
    if !token_matches(supplied, expected) {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "A valid privileged review bearer token is required",
        ));
    }
    Ok(next.run(request).await)
}

fn token_matches(supplied: &str, expected: &str) -> bool {
    let supplied = Sha256::digest(supplied.as_bytes());
    let expected = Sha256::digest(expected.as_bytes());
    supplied.ct_eq(&expected).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_comparison_rejects_missing_and_near_matches() {
        assert!(token_matches(
            "correct-service-token",
            "correct-service-token"
        ));
        assert!(!token_matches("", "correct-service-token"));
        assert!(!token_matches(
            "correct-service-tokeN",
            "correct-service-token"
        ));
    }
}
