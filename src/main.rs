//! Local HTTP transport for ordered memory.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use mineplan::mcp;
use mineplan::ordered_memory::MemoryStore;
use serde_json::{Value, json};
use std::env;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct AppState {
    store: Arc<Mutex<MemoryStore>>,
    memory_id: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let database_path = env::var("MEMORY_DB_PATH").unwrap_or_else(|_| "mineplan.sqlite3".into());
    let memory_id = env::var("MEMORY_ID").unwrap_or_else(|_| "default".into());
    let command = parse_command(env::args().skip(1).collect())
        .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
    match command {
        Command::Serve => serve(&database_path, &memory_id).await,
        Command::Help => {
            println!("{}", help_text());
            Ok(())
        }
        Command::Version => {
            println!("mineplan {}", env!("MINEPLAN_BUILD_VERSION"));
            Ok(())
        }
        Command::ClearMemory { memory_id, confirm } => {
            clear_memory_command(&database_path, &memory_id, confirm.as_deref())
        }
    }
}

async fn serve(database_path: &str, memory_id: &str) -> Result<(), Box<dyn std::error::Error>> {
    let port = env::var("MEMORY_HTTP_PORT").map_or(Ok(3000), |value| value.parse::<u16>())?;
    let bind = format!("127.0.0.1:{port}");
    let mut store = MemoryStore::open(database_path)?;
    store.create_memory_if_missing(memory_id)?;
    let state = AppState {
        store: Arc::new(Mutex::new(store)),
        memory_id: memory_id.into(),
    };
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    eprintln!("mineplan HTTP server: http://{bind}");
    eprintln!("MCP: http://{bind}/mcp");
    eprintln!("memory: {memory_id}");
    axum::serve(listener, app(state)).await?;
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Serve,
    Help,
    Version,
    ClearMemory {
        memory_id: String,
        confirm: Option<String>,
    },
}

fn parse_command(arguments: Vec<String>) -> Result<Command, String> {
    if arguments.is_empty() {
        return Ok(Command::Serve);
    }
    if matches!(
        arguments.first().map(String::as_str),
        Some("help" | "--help" | "-h")
    ) {
        return if arguments.len() == 1 {
            Ok(Command::Help)
        } else {
            Err(usage("help takes no arguments"))
        };
    }
    if matches!(
        arguments.first().map(String::as_str),
        Some("version" | "--version" | "-V")
    ) {
        return if arguments.len() == 1 {
            Ok(Command::Version)
        } else {
            Err(usage("version takes no arguments"))
        };
    }
    if arguments.first().map(String::as_str) != Some("clear-memory") {
        return Err(usage(format!("unknown command: {}", arguments[0])));
    }
    let mut memory_id = None;
    let mut confirm = None;
    let mut index = 1;
    while index < arguments.len() {
        let flag = &arguments[index];
        let value = arguments
            .get(index + 1)
            .ok_or_else(|| usage(format!("{flag} requires a value")))?;
        match flag.as_str() {
            "--memory-id" if memory_id.is_none() => memory_id = Some(value.clone()),
            "--confirm" if confirm.is_none() => confirm = Some(value.clone()),
            "--memory-id" | "--confirm" => {
                return Err(usage(format!("duplicate option: {flag}")));
            }
            _ => return Err(usage(format!("unknown option: {flag}"))),
        }
        index += 2;
    }
    let memory_id = memory_id.ok_or_else(|| usage("--memory-id is required"))?;
    if memory_id.trim().is_empty() {
        return Err(usage("--memory-id must not be empty"));
    }
    Ok(Command::ClearMemory { memory_id, confirm })
}

fn clear_memory_command(
    database_path: &str,
    memory_id: &str,
    confirmation: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let confirmed = match confirmation {
        Some(value) => value.to_string(),
        None => {
            eprintln!("memory \"{memory_id}\" の全メモと辺を物理削除します。");
            eprintln!("この操作は取り消せません。");
            eprint!("続行するには memory_id を再入力してください: ");
            io::stderr().flush()?;
            let mut input = String::new();
            io::stdin().read_line(&mut input)?;
            input.trim_end_matches(['\r', '\n']).to_string()
        }
    };
    if confirmed != memory_id {
        return Err(format!(
            "confirmation did not match memory_id; nothing was deleted: {memory_id}"
        )
        .into());
    }
    let mut store = MemoryStore::open(database_path)?;
    let result = store.clear_memory(memory_id)?;
    println!(
        "cleared memory {}: {} notes, {} orders deleted",
        result.memory_id, result.deleted_notes, result.deleted_orders
    );
    Ok(())
}

fn usage(message: impl AsRef<str>) -> String {
    format!("{}\n\n{}", message.as_ref(), help_text())
}

fn help_text() -> &'static str {
    "mineplan MCP server

USAGE:
  mineplan
  mineplan clear-memory --memory-id <id> [--confirm <id>]
  mineplan help
  mineplan version

ENVIRONMENT:
  MEMORY_ID         Memory used by MCP tools (default: default)
  MEMORY_DB_PATH    SQLite database path (default: mineplan.sqlite3)
  MEMORY_HTTP_PORT  Local HTTP port (default: 3000)

MCP:
  POST http://127.0.0.1:3000/mcp
  Tools: add_node, update_node_name, update_node_memo, delete_node, add_edge, update_edge, delete_edge, add_sequence, focus"
}

fn app(state: AppState) -> Router {
    Router::new()
        .route("/mcp", post(mcp_post).get(mcp_get))
        .with_state(state)
}

async fn mcp_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    if !origin_is_allowed(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state.store.lock() {
        Ok(mut store) => Json(mcp::handle_json_request(
            &mut store,
            &state.memory_id,
            &request.to_string(),
        ))
        .into_response(),
        Err(_) => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "memory store lock poisoned",
        ),
    }
}

async fn mcp_get(headers: HeaderMap) -> Response {
    if !origin_is_allowed(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    StatusCode::METHOD_NOT_ALLOWED.into_response()
}

fn origin_is_allowed(headers: &HeaderMap) -> bool {
    headers.get(header::ORIGIN).is_none()
}

fn api_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({"error":message}))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use tower::ServiceExt;

    fn temporary_database_path() -> std::path::PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "mineplan-cli-test-{}-{unique}.sqlite3",
            std::process::id()
        ))
    }

    fn test_app() -> Router {
        let mut store = MemoryStore::open(":memory:").unwrap();
        store.create_memory("default").unwrap();
        app(AppState {
            store: Arc::new(Mutex::new(store)),
            memory_id: "default".into(),
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
    async fn mcp_uses_the_server_configured_memory_without_an_id_argument() {
        let app = test_app();
        let response = call(
            &app,
            mcp_request(
                1,
                "add_edge",
                json!({"edge_name":"next","from":"前","to":"後"}),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let response = call(&app, mcp_request(2, "focus", json!({"focus":"後"}))).await;
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let response: Value = serde_json::from_slice(&body).unwrap();
        let output: Value =
            serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        assert_eq!(output["focus"], "後");
        assert_eq!(output["groups"], json!([]));
    }

    #[tokio::test]
    async fn browser_requests_from_another_origin_are_rejected() {
        let response = call(
            &test_app(),
            Request::builder()
                .uri("/mcp")
                .header(header::ORIGIN, "http://example.test")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn parses_server_and_clear_memory_commands() {
        assert_eq!(parse_command(vec![]).unwrap(), Command::Serve);
        assert_eq!(parse_command(vec!["--help".into()]).unwrap(), Command::Help);
        assert_eq!(
            parse_command(vec!["--version".into()]).unwrap(),
            Command::Version
        );
        assert_eq!(parse_command(vec!["-V".into()]).unwrap(), Command::Version);
        assert!(parse_command(vec!["-v".into()]).is_err());
        assert!(help_text().contains("MEMORY_ID"));
        assert_eq!(
            parse_command(vec![
                "clear-memory".into(),
                "--memory-id".into(),
                "minecraft".into(),
                "--confirm".into(),
                "minecraft".into(),
            ])
            .unwrap(),
            Command::ClearMemory {
                memory_id: "minecraft".into(),
                confirm: Some("minecraft".into()),
            }
        );
        assert!(parse_command(vec!["clear-memory".into()]).is_err());
    }

    #[test]
    fn cli_confirmation_must_match_before_memory_is_cleared() {
        let path = temporary_database_path();
        {
            let mut store = MemoryStore::open(&path).unwrap();
            store.create_memory("minecraft").unwrap();
            store.add_note("minecraft", "残すメモ").unwrap();
        }
        assert!(clear_memory_command(path.to_str().unwrap(), "minecraft", Some("wrong")).is_err());
        assert_eq!(
            MemoryStore::open(&path)
                .unwrap()
                .get_memory("minecraft")
                .unwrap()
                .notes,
            ["残すメモ"]
        );
        clear_memory_command(path.to_str().unwrap(), "minecraft", Some("minecraft")).unwrap();
        assert!(
            MemoryStore::open(&path)
                .unwrap()
                .get_memory("minecraft")
                .unwrap()
                .notes
                .is_empty()
        );
        std::fs::remove_file(path).unwrap();
    }
}
