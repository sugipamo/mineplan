//! Read-only localhost viewer for the Thought memory HTTP API.

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use reqwest::Url;
use std::env;

const UI: &str = include_str!("viewer.html");

#[derive(Clone)]
struct AppState {
    client: reqwest::Client,
    backend: Url,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let backend_text =
        env::var("MEMORY_VIEWER_BACKEND").unwrap_or_else(|_| "http://127.0.0.1:3000".into());
    let backend = local_backend_url(&backend_text)?;
    let port = env::var("MEMORY_VIEWER_PORT").map_or(Ok(3100), |value| value.parse::<u16>())?;
    let bind = format!("127.0.0.1:{port}");
    let app = Router::new()
        .route("/", get(index))
        .route("/api/memories", get(memories))
        .route("/api/memories/{memory_id}/context", get(context))
        .route("/api/memories/{memory_id}/thoughts", get(thoughts))
        .with_state(AppState {
            client: reqwest::Client::new(),
            backend,
        });
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    eprintln!("Thought memory viewer: http://{bind}");
    axum::serve(listener, app).await?;
    Ok(())
}

fn local_backend_url(value: &str) -> Result<Url, Box<dyn std::error::Error>> {
    let url = Url::parse(value)?;
    let local = matches!(
        url.host_str(),
        Some("127.0.0.1") | Some("localhost") | Some("::1")
    );
    if url.scheme() != "http" || !local {
        return Err("MEMORY_VIEWER_BACKEND must be a localhost http URL".into());
    }
    Ok(url)
}

async fn index() -> Html<&'static str> {
    Html(UI)
}

async fn memories(State(state): State<AppState>) -> Response {
    proxy(&state, &["api", "memories"]).await
}

async fn context(State(state): State<AppState>, Path(memory_id): Path<String>) -> Response {
    proxy(&state, &["api", "memories", &memory_id, "context"]).await
}

async fn thoughts(State(state): State<AppState>, Path(memory_id): Path<String>) -> Response {
    proxy(&state, &["api", "memories", &memory_id, "thoughts"]).await
}

async fn proxy(state: &AppState, segments: &[&str]) -> Response {
    let mut url = state.backend.clone();
    {
        let mut path = match url.path_segments_mut() {
            Ok(path) => path,
            Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR, "invalid backend URL"),
        };
        path.pop_if_empty();
        path.extend(segments);
    }
    match state.client.get(url).send().await {
        Ok(response) => {
            let status =
                StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            match response.bytes().await {
                Ok(body) => {
                    (status, [(header::CONTENT_TYPE, "application/json")], body).into_response()
                }
                Err(_) => error(
                    StatusCode::BAD_GATEWAY,
                    "could not read memory-server response",
                ),
            }
        }
        Err(_) => error(StatusCode::BAD_GATEWAY, "could not reach memory server"),
    }
}

fn error(status: StatusCode, message: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        format!("{{\"error\":\"{message}\"}}"),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_must_be_local_http() {
        assert!(local_backend_url("http://127.0.0.1:3000").is_ok());
        assert!(local_backend_url("http://localhost:3000").is_ok());
        assert!(local_backend_url("https://example.com").is_err());
        assert!(local_backend_url("http://192.0.2.1:3000").is_err());
    }
}
