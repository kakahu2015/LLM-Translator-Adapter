use axum::{
    routing::{post, get},
    Router,
    response::Response,
    http::{self, StatusCode, HeaderMap, HeaderName, HeaderValue},
    body::{Body, Bytes},
};
use futures::{Stream, StreamExt};
use reqwest::Client;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use std::sync::Arc;
use std::str::FromStr;

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

// Convert axum HeaderMap to reqwest HeaderMap
fn convert_headers(headers: &HeaderMap) -> reqwest::header::HeaderMap {
    let mut new_headers = reqwest::header::HeaderMap::new();
    for (key, value) in headers.iter() {
        if let Ok(name) = reqwest::header::HeaderName::from_bytes(key.as_ref()) {
            if let Ok(val) = reqwest::header::HeaderValue::from_bytes(value.as_bytes()) {
                new_headers.insert(name, val);
            }
        }
    }
    new_headers
}

// Main chat completions handler
async fn handle_chat_completions(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    // Parse request body to check for streaming
    let is_streaming = match serde_json::from_slice::<Value>(&body) {
        Ok(json) => json.get("stream").and_then(|v| v.as_bool()).unwrap_or(false),
        Err(_) => false,
    };

    // Forward request to actual LLM service
    let target_url = get_target_url(&headers);
    let reqwest_headers = convert_headers(&headers);
    
    let response = match state.client
        .post(&target_url)
        .headers(reqwest_headers)
        .body(body)
        .send()
        .await {
            Ok(resp) => resp,
            Err(e) => {
                return create_error_response(
                    StatusCode::BAD_GATEWAY,
                    "Failed to forward request",
                    &e.to_string(),
                );
            }
        };

    // Handle streaming and non-streaming responses differently
    if is_streaming {
        handle_streaming_response(response).await
    } else {
        handle_normal_response(response).await
    }
}

// Handle normal (non-streaming) response
async fn handle_normal_response(response: reqwest::Response) -> Response<Body> {
    let status = StatusCode::from_u16(response.status().as_u16())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    
    let mut builder = Response::builder()
        .status(status);

    // Copy headers
    if let Some(headers) = builder.headers_mut() {
        for (key, value) in response.headers() {
            if let Ok(name) = HeaderName::from_str(key.as_str()) {
                if let Ok(val) = HeaderValue::from_bytes(value.as_bytes()) {
                    headers.insert(name, val);
                }
            }
        }
    }

    match response.bytes().await {
        Ok(bytes) => builder
            .body(Body::from(bytes))
            .unwrap_or_else(|_| create_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to create response",
                "Internal server error"
            )),
        Err(e) => create_error_response(
            StatusCode::BAD_GATEWAY,
            "Failed to read response",
            &e.to_string()
        ),
    }
}

// Handle streaming response
async fn handle_streaming_response(response: reqwest::Response) -> Response<Body> {
    let status = StatusCode::from_u16(response.status().as_u16())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    
    let mut builder = Response::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, "text/event-stream");

    // Copy other relevant headers
    if let Some(headers) = builder.headers_mut() {
        for (key, value) in response.headers() {
            if key != http::header::CONTENT_TYPE {
                if let Ok(name) = HeaderName::from_str(key.as_str()) {
                    if let Ok(val) = HeaderValue::from_bytes(value.as_bytes()) {
                        headers.insert(name, val);
                    }
                }
            }
        }
    }

    let stream = response.bytes_stream().map(|result| match result {
        Ok(bytes) => Ok(Bytes::from(bytes)),
        Err(e) => Err(std::io::Error::new(std::io::ErrorKind::Other, e)),
    });

    builder
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| create_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to create streaming response",
            "Internal server error"
        ))
}

// Helper function to create error responses in OpenAI format
fn create_error_response(status: StatusCode, message: &str, details: &str) -> Response<Body> {
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
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&error_json).unwrap()))
        .unwrap()
}

// Helper function to get target URL from headers
fn get_target_url(_headers: &HeaderMap) -> String {
    // You can implement custom logic here to extract the target URL
    // For now, we'll use a default
    "http://your-internal-llm-service/chat/completions".to_string()
}
