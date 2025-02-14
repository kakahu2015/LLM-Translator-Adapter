use axum::{
    extract::State,
    routing::post,
    Router,
    response::Response,
    http::{StatusCode, header},
    body::{Body, Bytes},
};
use futures::StreamExt;
use reqwest::Client;
use std::sync::Arc;
use serde_json::Value;
use tokio::net::TcpListener;

// 固定配置
const MODEL_URL: &str = "http://your-model-endpoint/api/chat";
const MODEL_KEY: &str = "your-key-here";
const DEFAULT_MODEL: &str = "your-model-name";  // 写死的模型名称

#[derive(Clone)]
struct AppState {
    client: Client,
}

#[tokio::main]
async fn main() {
    let client = Client::new();
    let state = Arc::new(AppState { client });

    let app = Router::new()
        .route("/v1/chat/completions", post(handle_chat))
        .with_state(state);

    let listener = TcpListener::bind("0.0.0.0:5000").await.unwrap();
    println!("Server running on http://0.0.0.0:5000");
    axum::serve(listener, app).await.unwrap();
}

async fn handle_chat(
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> Response<Body> {
    // 解析请求体
    let mut payload: Value = match serde_json::from_slice(&body) {
        Ok(json) => json,
        Err(_) => {
            return create_error_response(
                StatusCode::BAD_REQUEST,
                "Invalid request body",
                "Could not parse request body as JSON",
            );
        }
    };

    // 替换为固定的模型名称
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("model".to_string(), Value::String(DEFAULT_MODEL.to_string()));
    }

    // 检查是否为流式请求
    let is_stream = payload.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);

    // 固定请求头
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    headers.insert(
        reqwest::header::AUTHORIZATION,
        reqwest::header::HeaderValue::from_str(&format!("Bearer {}", MODEL_KEY)).unwrap(),
    );

    // 发送请求
    let response = match state.client
        .post(MODEL_URL)
        .headers(headers)
        .json(&payload)
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

    // 处理响应
    if is_stream {
        handle_streaming_response(response).await
    } else {
        handle_normal_response(response).await
    }
}

async fn handle_normal_response(response: reqwest::Response) -> Response<Body> {
    let status = StatusCode::from_u16(response.status().as_u16())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    
    match response.bytes().await {
        Ok(bytes) => Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, "application/json")
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

async fn handle_streaming_response(response: reqwest::Response) -> Response<Body> {
    let status = StatusCode::from_u16(response.status().as_u16())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    
    let stream = response.bytes_stream().map(|result| match result {
        Ok(bytes) => Ok(Bytes::from(bytes)),
        Err(e) => Err(std::io::Error::new(std::io::ErrorKind::Other, e)),
    });

    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| create_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to create streaming response",
            "Internal server error"
        ))
}

fn create_error_response(status: StatusCode, message: &str, details: &str) -> Response<Body> {
    let error_json = serde_json::json!({
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
