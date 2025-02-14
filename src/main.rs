use axum::{
    routing::{post, get},
    Router,
    extract::{State, Json},
    response::Response,
    http::{Request, StatusCode, header},
    body::{Body, Bytes},
};
use futures::{Stream, StreamExt};
use reqwest::Client;
use serde_json::{json, Value};
use std::convert::Infallible;
use tokio::net::TcpListener;
use std::sync::Arc;

// App state containing HTTP client
struct AppState {
    client: Client,
}

#[tokio::main]
async fn main() {
    // Initialize logging
    env_logger::init();

    // Create HTTP client
    let client = Client::builder()
        .build()
        .expect("Failed to create HTTP client");

    let state = Arc::new(AppState { client });

    // Create router
    let app = Router::new()
        .route("/chat/completions", post(handle_chat_completions))
        .route("/health", get(health_check))
        .with_state(state);

    // Start server
    let listener = TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!("Server running on http://0.0.0.0:3000");
    axum::serve(listener, app).await.unwrap();
}

// Health check endpoint
async fn health_check() -> StatusCode {
    StatusCode::OK
}

// Main chat completions handler
async fn handle_chat_completions(
    State(state): State<Arc<AppState>>,
    req: Request<Body>,
) -> Response {
    // Extract request details
    let (parts, body) = req.into_parts();
    let body_bytes = match hyper::body::to_bytes(body).await {
        Ok(bytes) => bytes,
        Err(e) => {
            return create_error_response(
                StatusCode::BAD_REQUEST,
                "Failed to read request body",
                &e.to_string(),
            );
        }
    };

    // Parse request body to check for streaming
    let is_streaming = match serde_json::from_slice::<Value>(&body_bytes) {
        Ok(json) => json.get("stream").and_then(|v| v.as_bool()).unwrap_or(false),
        Err(_) => false,
    };

    // Forward request to actual LLM service
    let target_url = get_target_url(&parts.headers);
    let mut builder = state.client
        .post(&target_url)
        .headers(parts.headers);

    // Add body to request
    builder = builder.body(body_bytes);

    // Send request
    let response = match builder.send().await {
        Ok(resp) => resp,
        Err(e) => {
            return create_error_response(
                StatusCode::BAD_GATEWAY,
                "Failed to forward request",
                &e.to_string(),
            );
        }
    };

    let status = response.status();
    let headers = response.headers().clone();

    // Handle streaming and non-streaming responses differently
    if is_streaming {
        handle_streaming_response(response).await
    } else {
        handle_normal_response(response).await
    }
}

// Handle normal (non-streaming) response
async fn handle_normal_response(response: reqwest::Response) -> Response {
    let status = response.status();
    let headers = response.headers().clone();

    match response.bytes().await {
        Ok(bytes) => {
            let mut builder = Response::builder()
                .status(status);

            // Copy relevant headers
            for (key, value) in headers.iter() {
                builder = builder.header(key, value);
            }

            builder
                .body(Body::from(bytes))
                .unwrap_or_else(|_| create_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to create response",
                    "Internal server error"
                ))
        }
        Err(e) => create_error_response(
            StatusCode::BAD_GATEWAY,
            "Failed to read response",
            &e.to_string()
        ),
    }
}

// Handle streaming response
async fn handle_streaming_response(response: reqwest::Response) -> Response {
    let status = response.status();
    let headers = response.headers().clone();

    let stream = response.bytes_stream().map(|result| match result {
        Ok(bytes) => Ok(Bytes::from(bytes)),
        Err(e) => Err(std::io::Error::new(std::io::ErrorKind::Other, e)),
    });

    let body = Body::from_stream(stream);

    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/event-stream");

    // Copy other relevant headers
    for (key, value) in headers.iter() {
        if key != header::CONTENT_TYPE {
            builder = builder.header(key, value);
        }
    }

    builder
        .body(body)
        .unwrap_or_else(|_| create_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to create streaming response",
            "Internal server error"
        ))
}

// Helper function to create error responses in OpenAI format
fn create_error_response(status: StatusCode, message: &str, details: &str) -> Response {
    let error_json = json!({
        "error": {
            "message": message,
            "type": "proxy_error",
            "code": status.as_u16(),
            "details": details
        }
    });

    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&error_json).unwrap()))
        .unwrap()
}

// Helper function to get target URL from headers
fn get_target_url(headers: &header::HeaderMap) -> String {
    // You can implement custom logic here to extract the target URL
    // For now, we'll use a default
    "http://your-internal-llm-service/chat/completions".to_string()
}

// Helper function to check if response is SSE
fn is_sse_response(headers: &header::HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.contains("text/event-stream"))
        .unwrap_or(false)
}
