//! CPU-only OpenAI-compatible stub for end-to-end API/worker smoke tests.
//! Run with `cargo run -p ocr-worker --example mock_vllm`.
use axum::{
    Router,
    extract::Json,
    http::StatusCode,
    routing::{get, post},
};
use serde_json::{Value, json};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let bind = std::env::var("MOCK_BIND_ADDR")
        .or_else(|_| std::env::var("MOCK_VLLM_BIND"))
        .unwrap_or_else(|_| "127.0.0.1:8000".into());
    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/v1/chat/completions", post(completion));
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    println!("Mock vLLM listening on http://{bind}/v1 (fixture text only; no OCR inference)");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

async fn completion(Json(request): Json<Value>) -> Result<Json<Value>, StatusCode> {
    let image = request
        .pointer("/messages/1/content/1/image_url/url")
        .and_then(Value::as_str);
    if !image.is_some_and(|s| s.starts_with("data:image/jpeg;base64,"))
        || request["stream"] != false
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let model = request["model"].as_str().unwrap_or("mock");
    let markdown = std::env::var("MOCK_VLLM_TEXT").unwrap_or_else(|_| {
        "# Mock OCR result\n\nThis text comes from the CPU-only test stub.".into()
    });
    Ok(Json(json!({
        "id": "mock-completion", "object": "chat.completion", "model": model,
        "choices": [{"index": 0, "message": {"role": "assistant", "content": markdown}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0}
    })))
}
