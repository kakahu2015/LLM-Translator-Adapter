use axum::{
    extract::State,
    routing::post,
    Router,
    response::Response,
    http::{self, StatusCode, header},
    body::{Body, Bytes},
};
use config::{Config, ConfigError};
use futures::StreamExt;
use reqwest::Client;
use serde::Deserialize;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing_subscriber;

#[derive(Debug, Deserialize, Clone)]
struct AppConfig {
    model_url: String,
    model_key: String,
    default_model: String,
    port: u16,
    host: String,
}

impl AppConfig {
    pub fn load() -> Result<Self, ConfigError> {
        let config = Config::builder()
            .add_source(config::File::with_name("config/default"))
            .add_source(config::File::with_name("config/local").required(false))
            .build()?;

        config.try_deserialize()
    }
}

#[derive(Clone)]
struct AppState {
    client: Client,
    config: Arc<AppConfig>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    
    let config = Arc::new(AppConfig::load()?);
    println!("Configuration loaded successfully");
    
    let client = Client::new();
    let state = Arc::new(AppState { 
        client,
        config: config.clone(),
    });

    let app = Router::new()
        .route("/v1beta/openai/chat/completions", post(handle_chat))
        .with_state(state);

    let addr = format!("{}:{}", config.host, config.port);
    let listener = TcpListener::bind(&addr).await?;
    println!("Server running on http://{}", addr);
    
    axum::serve(listener, app).await?;
    Ok(())
}

fn create_error_response(
    status: StatusCode,
    error_type: &str,
    message: &str,
) -> Response<Body> {
    let error_response = serde_json::json!({
        "error": {
            "type": error_type,
            "message": message,
        }
    });

    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_string(&error_response).unwrap()))
        .unwrap()
}

async fn handle_streaming_response(response: reqwest::Response) -> Response<Body> {
    let status = response.status();
    let headers = response.headers().clone();
    
    let stream = response.bytes_stream().map(|result| {
        match result {
            Ok(bytes) => Ok(bytes.to_vec()),
            Err(e) => {
                println!("Error reading stream: {}", e);
                Err(std::io::Error::new(std::io::ErrorKind::Other, e))
            }
        }
    });

    let body = Body::from_stream(stream);
    
    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive");

    for (key, value) in headers.iter() {
        if !["transfer-encoding", "connection"].contains(&key.as_str()) {
            if let (Ok(name), Ok(val)) = (
                http::HeaderName::from_bytes(key.as_ref()),
                http::HeaderValue::from_bytes(value.as_bytes())
            ) {
                builder = builder.header(name, val);
            }
        }
    }

    builder.body(body).unwrap()
}

async fn handle_normal_response(response: reqwest::Response) -> Response<Body> {
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = match response.bytes().await {
        Ok(b) => b,
        Err(e) => {
            println!("Failed to read response body: {}", e);
            return create_error_response(
                StatusCode::BAD_GATEWAY,
                "Failed to read response",
                &e.to_string(),
            );
        }
    };

    let mut builder = Response::builder()
        .status(status);

    for (key, value) in headers.iter() {
        if !["transfer-encoding", "connection"].contains(&key.as_str()) {
            if let (Ok(name), Ok(val)) = (
                http::HeaderName::from_bytes(key.as_ref()),
                http::HeaderValue::from_bytes(value.as_bytes())
            ) {
                builder = builder.header(name, val);
            }
        }
    }

    builder.body(Body::from(bytes)).unwrap()
}

async fn handle_chat(
    State(state): State<Arc<AppState>>,
    headers: http::HeaderMap,
    body: Bytes,
) -> Response<Body> {
    let mut forward_headers = headers;
    forward_headers.insert(
        http::header::AUTHORIZATION,
        format!("Bearer {}", state.config.model_key).parse().unwrap()
    );

    let response = match state.client
        .post(&state.config.model_url)
        .headers(forward_headers)
        .body(body)
        .send()
        .await {
            Ok(resp) => resp,
            Err(e) => {
                println!("Failed to forward request: {}", e);
                return create_error_response(
                    StatusCode::BAD_GATEWAY,
                    "Failed to forward request",
                    &e.to_string(),
                );
            }
        };

    let is_stream = response.headers()
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.contains("text/event-stream"))
        .unwrap_or(false);

    if is_stream {
        handle_streaming_response(response).await
    } else {
        handle_normal_response(response).await
    }
}
