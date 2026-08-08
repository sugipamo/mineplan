//! JSON-RPC and MCP tool handling for mineplan.

use crate::ordered_memory::{DEFAULT_FOCUS_LIMIT, MemoryStore};
use serde::Deserialize;
use serde_json::{Value, json};

pub fn handle_json_request(store: &mut MemoryStore, memory_id: &str, request_text: &str) -> Value {
    match serde_json::from_str::<Request>(request_text) {
        Ok(request) => handle_request(store, memory_id, request),
        Err(error) => {
            json!({"jsonrpc":"2.0","id":Value::Null,"error":{"code":-32700,"message":format!("parse error: {error}")}})
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

fn handle_request(store: &mut MemoryStore, memory_id: &str, request: Request) -> Value {
    let id = request.id.unwrap_or(Value::Null);
    let notification = id.is_null();
    let result = match request.method.as_str() {
        "initialize" => Ok(json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "mineplan", "version": env!("CARGO_PKG_VERSION")}
        })),
        "ping" => Ok(json!({})),
        "notifications/initialized" | "notifications/cancelled" => return Value::Null,
        "tools/list" => Ok(json!({"tools": tool_definitions()})),
        "tools/call" => call_tool(store, memory_id, request.params),
        _ => Err((-32601, format!("method not found: {}", request.method))),
    };
    if notification {
        return Value::Null;
    }
    match result {
        Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
        Err((code, message)) => {
            json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
        }
    }
}

fn call_tool(
    store: &mut MemoryStore,
    memory_id: &str,
    params: Value,
) -> Result<Value, (i32, String)> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or((-32602, "tools/call requires name".into()))?;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let output = match name {
        "note_add" => {
            let note = required_string(&arguments, "note")?;
            store
                .add_note(memory_id, note)
                .map(|result| json!({"added":result.added}))
        }
        "note_rename" => {
            let from = required_string(&arguments, "from")?;
            let to = required_string(&arguments, "to")?;
            store
                .rename_note(memory_id, from, to)
                .map(|result| json!({"changed":result.changed,"merged":result.merged}))
        }
        "order_add" => {
            let before = required_string(&arguments, "before")?;
            let after = required_string(&arguments, "after")?;
            let reason = required_string(&arguments, "reason")?;
            store
                .add_order(memory_id, before, after, reason)
                .map(|result| json!({"added":result.added}))
        }
        "memory_focus" => {
            let focus = required_string_list(&arguments, "focus")?;
            let limit = optional_usize(&arguments, "limit", DEFAULT_FOCUS_LIMIT)?;
            store.focus(memory_id, &focus, limit).map(|view| {
                json!({
                    "before": view.before,
                    "focus": view.focus,
                    "after": view.after,
                    "connections": view.connections,
                    "truncated": view.truncated
                })
            })
        }
        _ => return Err((-32602, format!("unknown tool: {name}"))),
    }
    .map_err(|error| (-32000, error.to_string()))?;
    Ok(tool_result(output))
}

fn tool_result(output: Value) -> Value {
    json!({
        "content": [{"type":"text","text":serde_json::to_string(&output).expect("serializable output")}]
    })
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
        .ok_or((-32602, format!("{name} is required")))?
        .as_array()
        .ok_or((-32602, format!("{name} must be an array of strings")))?
        .iter()
        .map(|item| {
            item.as_str()
                .filter(|text| !text.trim().is_empty())
                .map(String::from)
                .ok_or((-32602, format!("{name} must contain non-empty strings")))
        })
        .collect()
}

fn optional_usize(value: &Value, name: &str, default: usize) -> Result<usize, (i32, String)> {
    match value.get(name) {
        None => Ok(default),
        Some(number) => number
            .as_u64()
            .and_then(|number| usize::try_from(number).ok())
            .ok_or((-32602, format!("{name} must be a non-negative integer"))),
    }
}

fn tool_definitions() -> Vec<Value> {
    vec![
        tool(
            "note_add",
            "Add a free-text note. Repeating the call is safe and returns added=false.",
            json!({"note":{"type":"string"}}),
            vec!["note"],
        ),
        tool(
            "note_rename",
            "Change a note's string identity. If the destination already exists, merge both notes, rewire all orders, and collapse exact order duplicates.",
            json!({"from":{"type":"string"},"to":{"type":"string"}}),
            vec!["from", "to"],
        ),
        tool(
            "order_add",
            "Record one directed, reasoned before/after assertion. Missing notes are created automatically. The reverse direction may be recorded as a separate edge with its own reason; bidirectional edges, cycles, and self-edges are valid and become local strongly connected focus groups. This does not assert completion, intention, or truth.",
            json!({"before":{"type":"string"},"after":{"type":"string"},"reason":{"type":"string"}}),
            vec!["before", "after", "reason"],
        ),
        tool(
            "memory_focus",
            "Select at most limit nearby notes, analyze SCCs only within that local graph, and return before/focus/after as lists of SCC note lists. Connections are independently capped at limit.",
            json!({"focus":{"type":"array","items":{"type":"string"},"minItems":1},"limit":{"type":"integer","minimum":0,"default":50}}),
            vec!["focus"],
        ),
    ]
}

fn tool(name: &str, description: &str, properties: Value, required: Vec<&str>) -> Value {
    json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false}})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_ordered_memory_operations() {
        let names: Vec<_> = tool_definitions()
            .into_iter()
            .map(|tool| tool["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            ["note_add", "note_rename", "order_add", "memory_focus"]
        );
    }

    #[test]
    fn mcp_round_trip_returns_a_focus_view() {
        let mut store = MemoryStore::open(":memory:").unwrap();
        store.create_memory("m").unwrap();
        for (id, name, arguments) in [
            (
                1,
                "order_add",
                json!({"before":"過去","after":"中心","reason":"中心より前の記録"}),
            ),
            (
                2,
                "order_add",
                json!({"before":"中心","after":"後続","reason":"中心より後の記録"}),
            ),
        ] {
            let response = handle_json_request(
                &mut store,
                "m",
                &json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":arguments}}).to_string(),
            );
            assert!(response.get("error").is_none(), "{response}");
        }
        let response = handle_json_request(
            &mut store,
            "m",
            &json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"memory_focus","arguments":{"focus":["中心"]}}}).to_string(),
        );
        assert!(response["result"].get("structuredContent").is_none());
        let view: Value =
            serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        assert_eq!(view["before"], json!([["過去"]]));
        assert_eq!(view["focus"], json!([["中心"]]));
        assert_eq!(view["after"], json!([["後続"]]));
        assert_eq!(view["connections"].as_array().unwrap().len(), 2);
        assert_eq!(view["truncated"], false);
        assert!(view.get("memory_id").is_none());
        assert!(view.get("limit").is_none());
    }
}
