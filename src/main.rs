//! Local HTTP transport and read-only inspector for Thought memory.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use memory_server::mcp;
use memory_server::thought::{DEFAULT_CONTEXT_LIMIT, ThoughtStore};
use serde::Deserialize;
use serde_json::{Value, json};
use std::env;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct AppState {
    store: Arc<Mutex<ThoughtStore>>,
}

#[derive(Deserialize)]
struct ContextQuery {
    limit: Option<usize>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let database_path = env::var("MEMORY_DB_PATH").unwrap_or_else(|_| "memory.sqlite3".into());
    let port = env::var("MEMORY_HTTP_PORT").map_or(Ok(3000), |value| value.parse::<u16>())?;
    let bind = format!("127.0.0.1:{port}");
    let state = AppState {
        store: Arc::new(Mutex::new(ThoughtStore::open(&database_path)?)),
    };
    let app = app(state);
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    eprintln!("Thought memory HTTP server: http://{bind}");
    eprintln!("WebUI: http://{bind}/  MCP: http://{bind}/mcp");
    axum::serve(listener, app).await?;
    Ok(())
}

fn app(state: AppState) -> Router {
    Router::new()
        .route("/mcp", post(mcp_post).get(mcp_get))
        .route("/api/memories", get(list_memories))
        .route("/api/memories/{memory_id}/context", get(get_context))
        .route("/api/memories/{memory_id}/thoughts", get(list_thoughts))
        .with_state(state)
}

async fn mcp_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    if !origin_is_allowed(&headers, &state) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let response = match state.store.lock() {
        Ok(mut store) => mcp::handle_json_request(&mut store, &request.to_string()),
        Err(_) => {
            json!({"jsonrpc":"2.0","id":Value::Null,"error":{"code":-32603,"message":"memory store lock poisoned"}})
        }
    };
    ([(header::CONTENT_TYPE, "application/json")], Json(response)).into_response()
}

async fn mcp_get(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !origin_is_allowed(&headers, &state) {
        return StatusCode::FORBIDDEN.into_response();
    }
    StatusCode::METHOD_NOT_ALLOWED.into_response()
}

async fn list_memories(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !origin_is_allowed(&headers, &state) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state.store.lock() {
        Ok(store) => match store.list_memory_ids() {
            Ok(memory_ids) => Json(json!({"memory_ids": memory_ids})).into_response(),
            Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
        },
        Err(_) => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "memory store lock poisoned",
        ),
    }
}

async fn get_context(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(memory_id): Path<String>,
    Query(query): Query<ContextQuery>,
) -> Response {
    if !origin_is_allowed(&headers, &state) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let limit = query.limit.unwrap_or(DEFAULT_CONTEXT_LIMIT);
    let result: Result<_, String> = match state.store.lock() {
        Ok(store) => (|| {
            let active_set = store
                .get_active_set(&memory_id)
                .map_err(|error| error.to_string())?;
            let thoughts = store
                .get_context(&memory_id, limit)
                .map_err(|error| error.to_string())?;
            Ok::<_, String>((active_set, thoughts))
        })(),
        Err(_) => Err("memory store lock poisoned".into()),
    };
    match result {
        Ok((active_set, thoughts)) => Json(json!({"memory_id": memory_id, "active_set": active_set.anchor_ids, "thoughts": thoughts})).into_response(),
        Err(error) => api_error(StatusCode::NOT_FOUND, &error),
    }
}

async fn list_thoughts(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(memory_id): Path<String>,
) -> Response {
    if !origin_is_allowed(&headers, &state) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let result: Result<_, String> = match state.store.lock() {
        Ok(store) => store
            .list_thoughts(&memory_id)
            .map_err(|error| error.to_string()),
        Err(_) => Err("memory store lock poisoned".into()),
    };
    match result {
        Ok(thoughts) => Json(json!({"memory_id": memory_id, "thoughts": thoughts})).into_response(),
        Err(error) => api_error(StatusCode::NOT_FOUND, &error),
    }
}

fn origin_is_allowed(headers: &HeaderMap, state: &AppState) -> bool {
    let _ = state;
    headers.get(header::ORIGIN).is_none()
}

fn api_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({"error": message}))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_app() -> Router {
        app(AppState {
            store: Arc::new(Mutex::new(ThoughtStore::open(":memory:").unwrap())),
        })
    }

    async fn call(app: &Router, request: Request<Body>) -> Response {
        app.clone().oneshot(request).await.unwrap()
    }

    fn mcp_request(id: usize, name: &str, arguments: Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/mcp")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":arguments}}).to_string(),
            ))
            .unwrap()
    }

    #[tokio::test]
    async fn http_mcp_and_read_only_context_share_one_store() {
        let app = test_app();
        for (id, name, arguments) in [
            (1, "memory_create", json!({"memory_id":"m"})),
            (
                2,
                "memory_record_thought",
                json!({"memory_id":"m","premises":["記録された前提"]}),
            ),
            (
                3,
                "memory_active_set_add",
                json!({"memory_id":"m","thought_id":"T1"}),
            ),
        ] {
            assert_eq!(
                call(&app, mcp_request(id, name, arguments)).await.status(),
                StatusCode::OK
            );
        }
        let response = call(
            &app,
            Request::builder()
                .uri("/api/memories/m/context")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap()["active_set"],
            json!(["T1"])
        );
        let response = call(
            &app,
            Request::builder()
                .uri("/api/memories/m/thoughts")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap()["thoughts"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn browser_requests_from_another_origin_are_rejected() {
        let app = test_app();
        let response = call(
            &app,
            Request::builder()
                .uri("/api/memories")
                .header(header::ORIGIN, "http://example.test")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
}
