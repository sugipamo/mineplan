//! Local HTTP transport for ordered memory.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use mineplan::mcp;
use mineplan::ordered_memory::MemoryStore;
use mineplan::path_mcp;
use serde_json::{Value, json};
use std::env;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct AppState {
    store: Arc<Mutex<MemoryStore>>,
    memory_id: String,
    path_max_focus_calls: usize,
    path_focus_limit: usize,
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
    let path_max_focus_calls =
        env::var("PATH_MAX_FOCUS_CALLS").map_or(Ok(50), |value| value.parse::<usize>())?;
    let path_focus_limit =
        env::var("PATH_FOCUS_LIMIT").map_or(Ok(50), |value| value.parse::<usize>())?;
    let bind = format!("127.0.0.1:{port}");
    let mut store = MemoryStore::open(database_path)?;
    store.create_memory_if_missing(memory_id)?;
    let state = AppState {
        store: Arc::new(Mutex::new(store)),
        memory_id: memory_id.into(),
        path_max_focus_calls,
        path_focus_limit,
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
  PATH_MAX_FOCUS_CALLS  Maximum in-memory focus observations per path (default: 50)
  PATH_FOCUS_LIMIT      Node and edge limit per path observation (default: 50)

MCP:
  POST http://127.0.0.1:3000/mcp
  Tools: add_node, update_node_name, update_node_memo, delete_node, add_edge, update_edge, delete_edge, edge_to_node, add_sequence, focus, find_path"
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
    if request.get("method").and_then(Value::as_str) == Some("tools/list") {
        let mut response = match state.store.lock() {
            Ok(mut store) => {
                mcp::handle_json_request(&mut store, &state.memory_id, &request.to_string())
            }
            Err(_) => {
                return api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "memory store lock poisoned",
                );
            }
        };
        if let Some(tools) = response
            .pointer_mut("/result/tools")
            .and_then(Value::as_array_mut)
        {
            tools.push(path_mcp::tool_definition());
        }
        return Json(response).into_response();
    }
    if request.pointer("/params/name").and_then(Value::as_str) == Some("find_path")
        && request.get("method").and_then(Value::as_str) == Some("tools/call")
    {
        let memory = match state.store.lock() {
            Ok(store) => match store.get_memory(&state.memory_id) {
                Ok(memory) => memory,
                Err(error) => {
                    return api_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string());
                }
            },
            Err(_) => {
                return api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "memory store lock poisoned",
                );
            }
        };
        return Json(path_mcp::handle_json_request(
            &memory,
            state.path_max_focus_calls,
            state.path_focus_limit,
            request,
        ))
        .into_response();
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
            path_max_focus_calls: 50,
            path_focus_limit: 50,
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

    fn path_mcp_request(id: usize, from: &str, to: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/mcp")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"find_path","arguments":{"from":from,"to":to}}}).to_string(),
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
                json!({"edge_name":"task","previous":"前","next":"後"}),
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
        assert_eq!(
            output["groups"],
            json!([{"edge_name":"task","previous":[["前"]],"next":[]}])
        )
    }

    #[tokio::test]
    async fn find_path_uses_the_same_memory_through_the_main_endpoint() {
        let app = test_app();
        for (id, previous, next, edge_name) in
            [(1, "A", "B", "x"), (2, "B", "C", "x"), (3, "C", "D", "y")]
        {
            let response = call(
                &app,
                mcp_request(
                    id,
                    "add_edge",
                    json!({"edge_name":edge_name,"previous":previous,"next":next}),
                ),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
        }
        let response = call(&app, path_mcp_request(4, "A", "D")).await;
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let response: Value = serde_json::from_slice(&body).unwrap();
        let output: Value =
            serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        assert_eq!(output["turns"], 1);
        assert_eq!(output["tasks"][0]["sequence"], json!(["A", "B", "C"]));
        assert_eq!(output["tasks"][1]["sequence"], json!(["C", "D"]));
    }

    #[tokio::test]
    async fn main_tools_list_includes_memory_and_path_tools() {
        let response = call(
            &test_app(),
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}).to_string(),
                ))
                .unwrap(),
        )
        .await;
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let response: Value = serde_json::from_slice(&body).unwrap();
        let tools = response["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 11);
        assert_eq!(tools.last().unwrap()["name"], "find_path");
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
