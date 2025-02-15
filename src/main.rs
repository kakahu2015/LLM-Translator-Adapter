use axum::{
    extract::State,
    routing::post,
    Router,
    response::Response,
    http::{StatusCode, header},
    body::{Body, Bytes},
};
use config::{Config, ConfigError};
use futures::StreamExt;
use reqwest::Client;
use serde::Deserialize;
use std::sync::Arc;
use serde_json::Value;
use tokio::net::TcpListener;
use tracing::{info, error};

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
    // 初始化日志
    tracing_subscriber::fmt::init();

    // 加载配置
    let config = Arc::new(AppConfig::load()?);
    info!("Configuration loaded successfully");
    
    let client = Client::new();
    let state = Arc::new(AppState { 
        client,
        config: config.clone(),
    });

    let app = Router::new()
        .route("/v1/chat/completions", post(handle_chat))
        .with_state(state);

    let addr = format!("{}:{}", config.host, config.port);
    let listener = TcpListener::bind(&addr).await?;
    info!("Server running on http://{}", addr);
    
    axum::serve(listener, app).await?;
    Ok(())
}

async fn handle_chat(
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> Response<Body> {
    // 解析请求体
    let mut payload: Value = match serde_json::from_slice(&body) {
        Ok(json) => json,
        Err(e) => {
            error!("Failed to parse request body: {}", e);
            return create_error_response(
                StatusCode::BAD_REQUEST,
                "Invalid request body",
                "Could not parse request body as JSON",
            );
        }
    };

    // 替换模型名称
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("model".to_string(), Value::String(state.config.default_model.clone()));
    }

    let is_stream = payload.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);

    // 设置请求头
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    headers.insert(
        reqwest::header::AUTHORIZATION,
        reqwest::header::HeaderValue::from_str(&format!("Bearer {}", state.config.model_key))
            .map_err(|e| {
                error!("Failed to create authorization header: {}", e);
                create_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Invalid configuration",
                    "Failed to create authorization header",
                )
            })?,
    );

    info!("Forwarding request to model API");
    
    // 转发请求
    let response = match state.client
        .post(&state.config.model_url)
        .headers(headers)
        .json(&payload)
        .send()
        .await {
            Ok(resp) => resp,
            Err(e) => {
                error!("Failed to forward request: {}", e);
                return create_error_response(
                    StatusCode::BAD_GATEWAY,
                    "Failed to forward request",
                    &e.to_string(),
                );
            }
        };

    // 处理响应
    if is_stream {
        handle_streaming_response(response).await
    } else {
        handle_normal_response(response).await
    }
}
