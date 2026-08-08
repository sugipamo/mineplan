//! JSON-RPC handling for the localhost HTTP MCP server.

use crate::thought::{DEFAULT_CONTEXT_LIMIT, PremiseDraft, ThoughtDraft, ThoughtStore};
use serde::Deserialize;
use serde_json::{Value, json};

/// Handles one JSON-RPC request encoded as JSON text for the HTTP transport.
pub fn handle_json_request(store: &mut ThoughtStore, request_text: &str) -> Value {
    match serde_json::from_str::<Request>(request_text) {
        Ok(request) => handle_request(store, request),
        Err(error) => {
            json!({"jsonrpc":"2.0", "id": Value::Null, "error":{"code":-32700,"message":format!("parse error: {error}")}})
        }
    }
}

#[derive(Debug, Deserialize)]
struct Request {
    #[allow(dead_code)]
    jsonrpc: Option<String>,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

fn handle_request(store: &mut ThoughtStore, request: Request) -> Value {
    let id = request.id.unwrap_or(Value::Null);
    let notification = id.is_null();
    let result = match request.method.as_str() {
        "initialize" => Ok(json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "thought-memory", "version": env!("CARGO_PKG_VERSION")}
        })),
        "ping" => Ok(json!({})),
        "notifications/initialized" | "notifications/cancelled" => return Value::Null,
        "tools/list" => Ok(json!({"tools": tool_definitions()})),
        "tools/call" => call_tool(store, request.params),
        _ => Err((-32601, format!("method not found: {}", request.method))),
    };
    if notification {
        return Value::Null;
    }
    match result {
        Ok(result) => json!({"jsonrpc":"2.0", "id":id, "result":result}),
        Err((code, message)) => {
            json!({"jsonrpc":"2.0", "id":id, "error":{"code":code,"message":message}})
        }
    }
}

fn call_tool(store: &mut ThoughtStore, params: Value) -> Result<Value, (i32, String)> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or((-32602, "tools/call requires name".into()))?;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if name == "memory_help" {
        let topic = arguments.get("topic").and_then(Value::as_str);
        let text = help_text(topic)?;
        return Ok(json!({"topic": topic.unwrap_or("overview"), "help": text}));
    }
    let output = match name {
        "memory_create" => {
            let memory_id = required_string(&arguments, "memory_id")?;
            store.create_memory(memory_id).map(|_| json!({"memory_id": memory_id, "active_set": []}))
        }
        "memory_record_thought" => {
            let memory_id = required_string(&arguments, "memory_id")?;
            let associated_from = optional_string_list(&arguments, "associated_from")?;
            let premises = required_string_list(&arguments, "premises")?
                .into_iter().map(|content| PremiseDraft { content }).collect();
            store.record_thought(memory_id, ThoughtDraft { associated_from, premises }).map(|thought| json!({"thought": thought}))
        }
        "memory_get_context" => {
            let memory_id = required_string(&arguments, "memory_id")?;
            let limit = optional_limit(&arguments)?;
            let active_set = store.get_active_set(memory_id).map_err(store_error)?;
            store.get_context(memory_id, limit).map(|thoughts| json!({"memory_id": memory_id, "active_set": active_set.anchor_ids, "thoughts": thoughts}))
        }
        "memory_get_active_set" => {
            let memory_id = required_string(&arguments, "memory_id")?;
            store.get_active_set(memory_id).map(|active_set| json!(active_set))
        }
        "memory_active_set_replace" | "memory_active_set_reorder" => {
            let memory_id = required_string(&arguments, "memory_id")?;
            let anchors = required_string_list(&arguments, "anchor_ids")?;
            store.replace_active_set(memory_id, &anchors).map(|active_set| json!(active_set))
        }
        "memory_active_set_add" => {
            let memory_id = required_string(&arguments, "memory_id")?;
            let thought_id = required_string(&arguments, "thought_id")?;
            store.add_active_anchor(memory_id, thought_id).map(|active_set| json!(active_set))
        }
        "memory_active_set_remove" => {
            let memory_id = required_string(&arguments, "memory_id")?;
            let thought_id = required_string(&arguments, "thought_id")?;
            store.remove_active_anchor(memory_id, thought_id).map(|active_set| json!(active_set))
        }
        "memory_related_add" => {
            let memory_id = required_string(&arguments, "memory_id")?;
            let thought_id_a = required_string(&arguments, "thought_id_a")?;
            let thought_id_b = required_string(&arguments, "thought_id_b")?;
            store
                .add_related_link(memory_id, thought_id_a, thought_id_b)
                .map(|related_link| json!({"related_link": related_link}))
        }
        "memory_related_remove" => {
            let memory_id = required_string(&arguments, "memory_id")?;
            let thought_id_a = required_string(&arguments, "thought_id_a")?;
            let thought_id_b = required_string(&arguments, "thought_id_b")?;
            store
                .remove_related_link(memory_id, thought_id_a, thought_id_b)
                .map(|related_link| json!({"related_link": related_link}))
        }
        "memory_thought_merge" => {
            let memory_id = required_string(&arguments, "memory_id")?;
            let source = required_string(&arguments, "source_thought_id")?;
            let target = required_string(&arguments, "target_thought_id")?;
            store.merge_thoughts(memory_id, source, target).map(|merge| json!({"merge": merge}))
        }
        "memory_get_related" => {
            let memory_id = required_string(&arguments, "memory_id")?;
            let thought_id = required_string(&arguments, "thought_id")?;
            store.get_related_thoughts(memory_id, thought_id).map(|thoughts| {
                json!({"memory_id": memory_id, "thought_id": thought_id, "thoughts": thoughts})
            })
        }
        "memory_clear" => {
            let memory_id = required_string(&arguments, "memory_id")?;
            store.clear_memory(memory_id).map(|result| json!(result))
        }
        _ => return Err((-32602, format!("unknown tool: {name}"))),
    }.map_err(store_error)?;
    Ok(
        json!({"content":[{"type":"text", "text":serde_json::to_string_pretty(&output).expect("serializable tool output")}], "structuredContent":output}),
    )
}

fn store_error(error: crate::thought::ThoughtError) -> (i32, String) {
    (-32000, error.to_string())
}

fn required_string<'a>(value: &'a Value, name: &str) -> Result<&'a str, (i32, String)> {
    value
        .get(name)
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .ok_or((-32602, format!("{name} must be a non-empty string")))
}
fn required_string_list(value: &Value, name: &str) -> Result<Vec<String>, (i32, String)> {
    value
        .get(name)
        .ok_or((-32602, format!("{name} is required")))
        .and_then(|v| string_list(v, name))
}
fn optional_string_list(value: &Value, name: &str) -> Result<Vec<String>, (i32, String)> {
    value
        .get(name)
        .map(|v| string_list(v, name))
        .transpose()
        .map(|list| list.unwrap_or_default())
}
fn string_list(value: &Value, name: &str) -> Result<Vec<String>, (i32, String)> {
    value
        .as_array()
        .ok_or((-32602, format!("{name} must be an array of strings")))?
        .iter()
        .map(|item| {
            item.as_str()
                .map(String::from)
                .ok_or((-32602, format!("{name} must be an array of strings")))
        })
        .collect()
}
fn optional_limit(value: &Value) -> Result<usize, (i32, String)> {
    match value.get("limit") {
        None => Ok(DEFAULT_CONTEXT_LIMIT),
        Some(limit) => limit
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or((-32602, "limit must be a non-negative integer".into())),
    }
}

fn tool_definitions() -> Vec<Value> {
    vec![
        tool(
            "memory_create",
            "Create one persistent memory, identified by memory_id.",
            json!({"memory_id":{"type":"string"}}),
            vec!["memory_id"],
        ),
        tool(
            "memory_help",
            "Explain mineplan concepts and common operation sequences. Use topic overview, workflow, context, merge, or errors.",
            json!({"topic":{"type":"string","enum":["overview","workflow","context","merge","errors"],"default":"overview"}}),
            vec![],
        ),
        tool(
            "memory_record_thought",
            "Append a Thought. Premises are free text; changing them means creating a later Thought linked to its sources.",
            json!({"memory_id":{"type":"string"},"associated_from":{"type":"array","items":{"type":"string"},"default":[]},"premises":{"type":"array","items":{"type":"string"}}}),
            vec!["memory_id", "premises"],
        ),
        tool(
            "memory_get_context",
            "Return the first N Thoughts discovered by bidirectional BFS from active-set anchors. Default N is 50.",
            json!({"memory_id":{"type":"string"},"limit":{"type":"integer","minimum":0,"default":50}}),
            vec!["memory_id"],
        ),
        tool(
            "memory_get_active_set",
            "Get the current ordered active-set anchors.",
            json!({"memory_id":{"type":"string"}}),
            vec!["memory_id"],
        ),
        tool(
            "memory_active_set_replace",
            "Replace the full ordered active set. An empty list is valid.",
            json!({"memory_id":{"type":"string"},"anchor_ids":{"type":"array","items":{"type":"string"}}}),
            vec!["memory_id", "anchor_ids"],
        ),
        tool(
            "memory_active_set_reorder",
            "Set the exact ordered active set; equivalent to replace and retained as an explicit operation.",
            json!({"memory_id":{"type":"string"},"anchor_ids":{"type":"array","items":{"type":"string"}}}),
            vec!["memory_id", "anchor_ids"],
        ),
        tool(
            "memory_active_set_add",
            "Append one existing Thought ID to the active set.",
            json!({"memory_id":{"type":"string"},"thought_id":{"type":"string"}}),
            vec!["memory_id", "thought_id"],
        ),
        tool(
            "memory_active_set_remove",
            "Remove one active-set anchor.",
            json!({"memory_id":{"type":"string"},"thought_id":{"type":"string"}}),
            vec!["memory_id", "thought_id"],
        ),
        tool(
            "memory_related_add",
            "Add a name-free, undirected related link between two existing Thoughts in one memory. Newer links are explored first by memory_get_context.",
            json!({"memory_id":{"type":"string"},"thought_id_a":{"type":"string"},"thought_id_b":{"type":"string"}}),
            vec!["memory_id", "thought_id_a", "thought_id_b"],
        ),
        tool(
            "memory_related_remove",
            "Remove one existing undirected related link between two Thoughts.",
            json!({"memory_id":{"type":"string"},"thought_id_a":{"type":"string"},"thought_id_b":{"type":"string"}}),
            vec!["memory_id", "thought_id_a", "thought_id_b"],
        ),
        tool(
            "memory_thought_merge",
            "Hide one Thought's content while traversing its edges through another Thought at zero exploration cost.",
            json!({"memory_id":{"type":"string"},"source_thought_id":{"type":"string"},"target_thought_id":{"type":"string"}}),
            vec!["memory_id", "source_thought_id", "target_thought_id"],
        ),
        tool(
            "memory_get_related",
            "Return Thoughts directly connected to one Thought by related, newest link first.",
            json!({"memory_id":{"type":"string"},"thought_id":{"type":"string"}}),
            vec!["memory_id", "thought_id"],
        ),
        tool(
            "memory_clear",
            "Immediately clear all Thoughts, associated_from links, related links, and active-set anchors from one memory while keeping memory_id reusable. Returns deletion counts; an empty memory succeeds with zero counts.",
            json!({"memory_id":{"type":"string"}}),
            vec!["memory_id"],
        ),
    ]
}

fn help_text(topic: Option<&str>) -> Result<&'static str, (i32, String)> {
    match topic.unwrap_or("overview") {
        "overview" => Ok(
            "mineplan は Thought を SQLite に保存する localhost 専用の記憶 MCP です。memory_create で記憶を作り、memory_record_thought で追記し、active_set を起点に memory_get_context で文脈を取得します。Thought は削除・上書きせず、前提を変えた場合は新しい Thought を作ります。",
        ),
        "workflow" => Ok(
            "基本手順: 1) memory_create(memory_id) 2) memory_record_thought で最初の Thought を記録 3) memory_active_set_replace または memory_active_set_add で注目ノードを設定 4) memory_get_context で周辺文脈を取得 5) 新しい判断は memory_record_thought で関連元を associated_from に指定して追記します。",
        ),
        "context" => Ok(
            "memory_get_context は active_set のアンカーから関連辺と associated_from 辺を双方向に探索します。limit は表示する通常ノードの件数です。統合ノードへの遷移はコスト0で limit を消費せず、統合元の内容は表示されません。active_set には同じ memory 内の既存ノードなら現在の表示範囲外でも指定できます。",
        ),
        "merge" => Ok(
            "memory_thought_merge(memory_id, source_thought_id, target_thought_id) は source を非表示にし、source の辺を target へ統合経路として扱います。統合関係は SQLite に保存されます。存在しないノードや同一ノードは指定できません。",
        ),
        "errors" => Ok(
            "ノード ID と memory_id は同じ記憶内に存在している必要があります。存在しない memory_id や Thought、重複した active anchor・関連・統合はエラーになります。エラーは JSON-RPC の error として返ります。",
        ),
        other => Err((-32602, format!("unknown help topic: {other}"))),
    }
}
fn tool(name: &str, description: &str, properties: Value, required: Vec<&str>) -> Value {
    json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false}})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tool_list_exposes_only_thought_memory_operations() {
        let tools = tool_definitions();
        let names: Vec<_> = tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"memory_record_thought"));
        assert!(names.contains(&"memory_related_add"));
        assert!(names.contains(&"memory_get_related"));
        assert!(names.contains(&"memory_clear"));
        assert!(!names.iter().any(|name| name.contains("event")
            || name.contains("reset")
            || name.contains("search")));
    }
}
