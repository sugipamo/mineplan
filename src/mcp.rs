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
            "serverInfo": {"name": "mineplan", "version": env!("MINEPLAN_BUILD_VERSION")}
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
        "node_add" => {
            let node_name = required_string(&arguments, "node_name")?;
            store
                .add_note(memory_id, node_name)
                .map(|result| json!({"added":result.added}))
        }
        "node_update" => {
            let from = required_string(&arguments, "from_node_name")?;
            let to = required_string(&arguments, "to_node_name")?;
            store
                .rename_note(memory_id, from, to)
                .map(|result| json!({"changed":result.changed,"merged":result.merged}))
        }
        "edge_add" => {
            let before = required_string(&arguments, "before")?;
            let after = required_string(&arguments, "after")?;
            let edge_name = required_string(&arguments, "edge_name")?;
            store
                .add_order(memory_id, before, after, edge_name)
                .map(|result| json!({"added":result.added}))
        }
        "edge_update" => {
            let edge_id = required_i64(&arguments, "edge_id")?;
            let before = optional_string(&arguments, "before")?;
            let after = optional_string(&arguments, "after")?;
            let edge_name = optional_string(&arguments, "edge_name")?;
            if before.is_none() && after.is_none() && edge_name.is_none() {
                return Err((-32602, "edge_update requires a field to change".into()));
            }
            store
                .update_order(
                    memory_id,
                    edge_id,
                    before.as_deref(),
                    after.as_deref(),
                    edge_name.as_deref(),
                )
                .map(|_| json!({"updated":true}))
        }
        "edge_delete" => {
            let edge_id = required_i64(&arguments, "edge_id")?;
            store
                .delete_order(memory_id, edge_id)
                .map(|deleted| json!({"deleted":deleted}))
        }
        "memory_focus" => {
            let focus = required_string_list(&arguments, "focus")?;
            let limit = optional_usize(&arguments, "limit", DEFAULT_FOCUS_LIMIT)?;
            store.focus(memory_id, &focus, limit).map(|view| {
                let connections: Vec<Value> = view
                    .connections
                    .into_iter()
                    .map(|edge| {
                        json!({
                            "edge_id": edge.edge_id,
                            "before": edge.before,
                            "after": edge.after,
                            "edge_name": edge.reason
                        })
                    })
                    .collect();
                json!({
                    "before": view.before,
                    "focus": view.focus,
                    "after": view.after,
                    "connections": connections
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

fn optional_string(value: &Value, name: &str) -> Result<Option<String>, (i32, String)> {
    match value.get(name) {
        None => Ok(None),
        Some(value) => value
            .as_str()
            .filter(|text| !text.trim().is_empty())
            .map(|text| Some(text.to_string()))
            .ok_or((-32602, format!("{name} must be a non-empty string"))),
    }
}

fn required_i64(value: &Value, name: &str) -> Result<i64, (i32, String)> {
    value
        .get(name)
        .and_then(Value::as_i64)
        .filter(|number| *number > 0)
        .ok_or((-32602, format!("{name} must be a positive integer")))
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
            "node_add",
            "Add a free-text node by node_name. Repeating the call is safe and returns added=false.",
            json!({"node_name":{"type":"string"}}),
            vec!["node_name"],
        ),
        tool(
            "node_update",
            "Change a node's string name. If the destination already exists, merge both nodes and rewire all edges. Exact duplicate edges remain separate.",
            json!({"from_node_name":{"type":"string"},"to_node_name":{"type":"string"}}),
            vec!["from_node_name", "to_node_name"],
        ),
        tool(
            "edge_add",
            "Add one directed edge. Missing nodes are created automatically. edge_name is a label, not necessarily a fact. Reverse edges, cycles, and self-edges are valid.",
            json!({"before":{"type":"string"},"after":{"type":"string"},"edge_name":{"type":"string"}}),
            vec!["before", "after", "edge_name"],
        ),
        tool(
            "edge_update",
            "Update an edge by its persistent edge_id. Any supplied before, after, or edge_name field is changed; omitted fields remain unchanged.",
            json!({"edge_id":{"type":"integer","minimum":1},"before":{"type":"string"},"after":{"type":"string"},"edge_name":{"type":"string"}}),
            vec!["edge_id"],
        ),
        tool(
            "edge_delete",
            "Delete one edge by its persistent edge_id. Node memories are not deleted.",
            json!({"edge_id":{"type":"integer","minimum":1}}),
            vec!["edge_id"],
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
            [
                "node_add",
                "node_update",
                "edge_add",
                "edge_update",
                "edge_delete",
                "memory_focus"
            ]
        );
    }

    #[test]
    fn mcp_round_trip_returns_a_focus_view() {
        let mut store = MemoryStore::open(":memory:").unwrap();
        store.create_memory("m").unwrap();
        for (id, name, arguments) in [
            (
                1,
                "edge_add",
                json!({"before":"過去","after":"中心","edge_name":"中心より前の記録"}),
            ),
            (
                2,
                "edge_add",
                json!({"before":"中心","after":"後続","edge_name":"中心より後の記録"}),
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
        assert!(view["connections"][0].get("edge_id").is_some());
        assert_eq!(view["connections"][0]["edge_name"], "中心より前の記録");
        let edge_id = view["connections"][0]["edge_id"].as_i64().unwrap();
        for (id, name, arguments) in [
            (
                4,
                "edge_update",
                json!({"edge_id":edge_id,"edge_name":"更新された辺"}),
            ),
            (5, "edge_delete", json!({"edge_id":edge_id})),
        ] {
            let response = handle_json_request(
                &mut store,
                "m",
                &json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":arguments}}).to_string(),
            );
            assert!(response.get("error").is_none(), "{response}");
        }
        assert!(view.get("truncated").is_none());
        assert!(view.get("memory_id").is_none());
        assert!(view.get("limit").is_none());
    }
}
