use axum::{
    extract::{Multipart, Path, State, multipart::MultipartRejection},
    http::{HeaderMap, StatusCode, header},
    response::Response,
};
use ocr_core::domain::NewJob;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{AppState, error::ApiError};

pub async fn submit(
    State(state): State<AppState>,
    headers: HeaderMap,
    multipart: Result<Multipart, MultipartRejection>,
) -> Result<Response, ApiError> {
    let _permit = state
        .upload_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "upload_capacity_exceeded",
                "Upload capacity is busy; retry with the same Idempotency-Key",
            )
        })?;
    let response = state.responses.json()?;
    let key = idempotency_key(&headers)?;
    let multipart = multipart.map_err(|_| {
        ApiError::bad_request("Expected multipart/form-data with exactly one 'file' field")
    })?;
    let (filename, input, extraction_profile) = tokio::time::timeout(
        std::time::Duration::from_secs(state.settings.upload_timeout_seconds),
        read_file(multipart, state.settings.max_upload_bytes),
    )
    .await
    .map_err(|_| {
        ApiError::new(
            StatusCode::REQUEST_TIMEOUT,
            "upload_timeout",
            "Upload did not finish within the configured deadline",
        )
    })??;
    let media_type = detect_media_type(&input).ok_or_else(|| {
        ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            "Supported inputs are PDF, PNG, JPEG, and WebP",
        )
    })?;
    let digest = format!("{:x}", Sha256::digest(&input));
    let outcome = state
        .store
        .submit_bounded_with_deadlines_and_response_limit(
            NewJob {
                id: Uuid::new_v4(),
                filename,
                media_type: media_type.into(),
                sha256: digest,
                input,
                idempotency_key: key,
                extraction_profile,
            },
            state.settings.max_pending_jobs,
            state.settings.enqueue_deadlines,
            state.settings.responses.max_json_bytes,
        )
        .await?;
    tracing::info!(job_id = %outcome.job.id, created = outcome.created, "OCR job accepted");
    let location = format!("/v1/ocr/jobs/{}", outcome.job.id);
    let mut response = response.json(outcome.job).await?;
    *response.status_mut() = StatusCode::ACCEPTED;
    response
        .headers_mut()
        .insert(header::LOCATION, location.parse().expect("UUID location"));
    Ok(response)
}

pub async fn get_job(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let response = state.responses.json()?;
    let id = Uuid::parse_str(&id).map_err(|_| ApiError::bad_request("Job ID must be a UUID"))?;
    let job = crate::response::deadline(
        state.settings.responses.read_timeout,
        state
            .store
            .get_bounded(id, state.settings.responses.max_json_bytes),
    )
    .await??
    .ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "job_not_found",
            "Job was not found or has expired",
        )
    })?;
    response.json(job).await
}

async fn read_file(
    mut multipart: Multipart,
    limit: usize,
) -> Result<(String, Vec<u8>, String), ApiError> {
    let mut file = None;
    let mut extraction_profile = None;
    while let Some(mut field) = multipart.next_field().await.map_err(multipart_error)? {
        match field.name() {
            Some("file") if file.is_none() => {
                let filename = sanitize_filename(field.file_name().unwrap_or("document"));
                let mut input = Vec::new();
                while let Some(chunk) = field.chunk().await.map_err(multipart_error)? {
                    if chunk.len() > limit.saturating_sub(input.len()) {
                        return Err(ApiError::too_large());
                    }
                    input.extend_from_slice(&chunk);
                }
                if input.is_empty() {
                    return Err(ApiError::bad_request("Uploaded file is empty"));
                }
                file = Some((filename, input));
            }
            Some("extraction_profile") if extraction_profile.is_none() => {
                let mut value = Vec::new();
                while let Some(chunk) = field.chunk().await.map_err(multipart_error)? {
                    if chunk.len() > 64_usize.saturating_sub(value.len()) || !chunk.is_ascii() {
                        return Err(ApiError::bad_request(
                            "extraction_profile must contain at most 64 ASCII bytes",
                        ));
                    }
                    value.extend_from_slice(&chunk);
                }
                let profile = match value.as_slice() {
                    b"none" => "none",
                    b"vi_rental_contract_v1" => "vi_rental_contract_v1",
                    _ => {
                        return Err(ApiError::bad_request(
                            "extraction_profile must be 'none' or 'vi_rental_contract_v1'",
                        ));
                    }
                };
                extraction_profile = Some(profile.to_owned());
            }
            _ => {
                return Err(ApiError::bad_request(
                    "Expected one 'file' field and at most one 'extraction_profile' field",
                ));
            }
        }
    }
    let (filename, input) = file.ok_or_else(|| ApiError::bad_request("Missing 'file' field"))?;
    Ok((
        filename,
        input,
        extraction_profile.unwrap_or_else(|| "none".into()),
    ))
}

fn multipart_error(error: axum::extract::multipart::MultipartError) -> ApiError {
    if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
        ApiError::too_large()
    } else {
        ApiError::bad_request("Malformed multipart upload")
    }
}

fn idempotency_key(headers: &HeaderMap) -> Result<Option<String>, ApiError> {
    if headers.get_all("idempotency-key").iter().count() > 1 {
        return Err(ApiError::bad_request(
            "Supply only one Idempotency-Key header",
        ));
    }
    headers.get("idempotency-key").map(|value| {
        let key = value.to_str().map_err(|_| ApiError::bad_request("Idempotency-Key must be ASCII"))?;
        if key.is_empty() || key.len() > 128 || !key.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(ApiError::bad_request("Idempotency-Key must contain 1 to 128 printable ASCII characters without spaces"));
        }
        Ok(key.to_owned())
    }).transpose()
}

fn sanitize_filename(name: &str) -> String {
    let basename = name.rsplit(['/', '\\']).next().unwrap_or("document");
    let name: String = basename
        .chars()
        .filter(|ch| !ch.is_control())
        .take(180)
        .collect();
    if name.is_empty() {
        "document".into()
    } else {
        name
    }
}

fn detect_media_type(input: &[u8]) -> Option<&'static str> {
    if input.starts_with(b"%PDF-") {
        Some("application/pdf")
    } else if input.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if input.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if input.len() >= 12 && &input[..4] == b"RIFF" && &input[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, extract::FromRequest, http::Request};

    async fn multipart(parts: &[(&str, &[u8])]) -> Multipart {
        let mut body = Vec::new();
        for (name, bytes) in parts {
            body.extend_from_slice(
                format!("--boundary\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n")
                    .as_bytes(),
            );
            body.extend_from_slice(bytes);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(b"--boundary--\r\n");
        Multipart::from_request(
            Request::post("/")
                .header("content-type", "multipart/form-data; boundary=boundary")
                .body(Body::from(body))
                .unwrap(),
            &(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn optional_profile_is_order_independent_and_defaults_to_ocr_only() {
        let (_, input, profile) = read_file(multipart(&[("file", b"document")]).await, 8)
            .await
            .unwrap();
        assert_eq!(input, b"document");
        assert_eq!(profile, "none");
        for parts in [
            vec![
                ("file", b"document".as_slice()),
                ("extraction_profile", b"vi_rental_contract_v1".as_slice()),
            ],
            vec![
                ("extraction_profile", b"vi_rental_contract_v1".as_slice()),
                ("file", b"document".as_slice()),
            ],
            vec![
                ("file", b"document".as_slice()),
                ("extraction_profile", b"none".as_slice()),
            ],
        ] {
            let expected = parts
                .iter()
                .find(|part| part.0 == "extraction_profile")
                .unwrap()
                .1;
            let (_, input, profile) = read_file(multipart(&parts).await, 8).await.unwrap();
            assert_eq!(input, b"document");
            assert_eq!(profile.as_bytes(), expected);
        }
    }

    #[tokio::test]
    async fn invalid_duplicate_unknown_or_oversized_profiles_are_rejected() {
        for value in [
            b"".as_slice(),
            b"unknown",
            b"none ",
            b"NONE",
            &[0xff],
            &[b'x'; 65],
        ] {
            let error = read_file(
                multipart(&[("file", b"document"), ("extraction_profile", value)]).await,
                8,
            )
            .await
            .unwrap_err();
            assert_eq!(error.status, StatusCode::BAD_REQUEST);
        }
        for parts in [
            vec![
                ("extraction_profile", b"none".as_slice()),
                ("file", b"document".as_slice()),
                ("extraction_profile", b"none".as_slice()),
            ],
            vec![
                ("file", b"document".as_slice()),
                ("unexpected", b"none".as_slice()),
            ],
            vec![("extraction_profile", b"none".as_slice())],
        ] {
            assert_eq!(
                read_file(multipart(&parts).await, 8)
                    .await
                    .unwrap_err()
                    .status,
                StatusCode::BAD_REQUEST
            );
        }
    }

    #[test]
    fn identifies_content_without_trusting_filename() {
        assert_eq!(detect_media_type(b"%PDF-1.7"), Some("application/pdf"));
        assert_eq!(
            detect_media_type(b"\x89PNG\r\n\x1a\nrest"),
            Some("image/png")
        );
        assert_eq!(detect_media_type(b"\xff\xd8\xffrest"), Some("image/jpeg"));
        assert_eq!(detect_media_type(b"RIFF1234WEBP"), Some("image/webp"));
        assert_eq!(detect_media_type(b"pretend.pdf"), None);
        assert_eq!(detect_media_type(b"RIFF"), None);
    }

    #[test]
    fn validates_retry_keys_and_safe_display_filename() {
        let mut headers = HeaderMap::new();
        headers.insert("idempotency-key", "invoice-42".parse().unwrap());
        assert_eq!(
            idempotency_key(&headers).unwrap().as_deref(),
            Some("invoice-42")
        );
        headers.insert("idempotency-key", "two words".parse().unwrap());
        assert!(idempotency_key(&headers).is_err());
        assert_eq!(sanitize_filename("../../invoice.pdf"), "invoice.pdf");
        assert_eq!(sanitize_filename("C:\\private\\invoice.pdf"), "invoice.pdf");
        assert_eq!(sanitize_filename("\0"), "document");
    }
}
