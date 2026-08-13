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
        "add_node" => {
            let node_name = required_string(&arguments, "node_name")?;
            let result = match optional_string(&arguments, "memo")? {
                Some(memo) => store.add_note_with_memo(memory_id, node_name, &memo),
                None => store.add_note(memory_id, node_name),
            }
            .map_err(|error| (-32000, error.to_string()))?;
            Ok(json!({"added":result.added}))
        }
        "update_node_name" => {
            let from = required_string(&arguments, "from_node_name")?;
            let to = required_string(&arguments, "to_node_name")?;
            let result = store
                .rename_note(memory_id, from, to)
                .map_err(|error| (-32000, error.to_string()))?;
            Ok(json!({"changed":result.changed,"merged":result.merged}))
        }
        "update_node_memo" => {
            let node_name = required_string(&arguments, "node_name")?;
            let memo = required_string_allow_empty(&arguments, "memo")?;
            store
                .update_note_memo(memory_id, node_name, memo)
                .map(|_| json!({"updated":true}))
        }
        "delete_node" => {
            let node_name = required_string(&arguments, "node_name")?;
            store
                .delete_note(memory_id, node_name)
                .map(|_| json!({"deleted":true}))
        }
        "add_edge" => {
            let edge_name = required_string(&arguments, "edge_name")?;
            let previous = required_string(&arguments, "previous")?;
            let next = required_string(&arguments, "next")?;
            store
                .add_order(memory_id, previous, next, edge_name)
                .map(|result| json!({"added":result.added}))
        }
        "add_sequence" => {
            let sequence = required_string_array(&arguments, "sequence")?;
            if sequence.len() < 2 {
                return Err((-32602, "sequence must contain at least two nodes".into()));
            }
            let edge_name = required_string(&arguments, "edge_name")?;
            let mut added_edges = 0usize;
            for pair in sequence.windows(2) {
                store
                    .add_order(memory_id, &pair[0], &pair[1], edge_name)
                    .map_err(|error| (-32000, error.to_string()))?;
                added_edges += 1;
            }
            Ok(json!({"added_edges":added_edges}))
        }
        "update_edge" => {
            let edge_id = required_i64(&arguments, "edge_id")?;
            let edge_name = optional_string(&arguments, "edge_name")?;
            let previous = optional_string(&arguments, "previous")?;
            let next = optional_string(&arguments, "next")?;
            if edge_name.is_none() && previous.is_none() && next.is_none() {
                return Err((-32602, "update_edge requires a field to change".into()));
            }
            store
                .update_order(
                    memory_id,
                    edge_id,
                    previous.as_deref(),
                    next.as_deref(),
                    edge_name.as_deref(),
                )
                .map(|_| json!({"updated":true}))
        }
        "delete_edge" => {
            let edge_id = required_i64(&arguments, "edge_id")?;
            store
                .delete_order(memory_id, edge_id)
                .map(|deleted| json!({"deleted":deleted}))
        }
        "edge_to_node" => {
            let edge_id = required_i64(&arguments, "edge_id")?;
            let node_name = required_string(&arguments, "node_name")?;
            store
                .edge_to_node(memory_id, edge_id, node_name)
                .map(|result| json!({
                    "removed_edge_id": result.removed_edge_id,
                    "added_edge_ids": result.added_edge_ids
                }))
        }
        "focus" => {
            let focus = required_string(&arguments, "focus")?.to_owned();
            let limit = optional_usize(&arguments, "limit", DEFAULT_FOCUS_LIMIT)?;
            let include_connections = optional_bool(&arguments, "include_connections", false)?;
            store.focus(memory_id, std::slice::from_ref(&focus), limit).map(|view| {
                let connections: Vec<Value> = view
                    .connections
                    .into_iter()
                    .map(|edge| {
                        json!({"edge_id": edge.edge_id, "edge_name": edge.edge_name, "previous": edge.previous, "next": edge.next})
                    })
                    .collect();
                let groups: Vec<Value> = view
                    .named_groups
                    .into_iter()
                    .filter_map(|(edge_name, previous, next)| {
                        let previous: Vec<Vec<String>> = previous
                            .into_iter()
                            .map(|component| {
                                component
                                    .into_iter()
                                    .filter(|node| node != &focus)
                                    .collect()
                            })
                            .filter(|component: &Vec<String>| !component.is_empty())
                            .collect();
                        let next: Vec<Vec<String>> = next
                            .into_iter()
                            .map(|component| {
                                component
                                    .into_iter()
                                    .filter(|node| node != &focus)
                                    .collect()
                            })
                            .filter(|component: &Vec<String>| !component.is_empty())
                            .collect();
                        (!previous.is_empty() || !next.is_empty()).then(|| {
                            json!({"edge_name": edge_name, "previous": previous, "next": next})
                        })
                    })
                    .collect();
                let mut output = json!({
                    "focus": focus,
                    "groups": groups,
                    "memos": view
                        .memos
                        .into_iter()
                        .collect::<std::collections::HashMap<_, _>>(),
                });
                if include_connections {
                    output["connections"] = json!(connections);
                }
                output
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

fn required_string_allow_empty<'a>(value: &'a Value, name: &str) -> Result<&'a str, (i32, String)> {
    value
        .get(name)
        .and_then(Value::as_str)
        .ok_or((-32602, format!("{name} must be a string")))
}

fn required_string_array(value: &Value, name: &str) -> Result<Vec<String>, (i32, String)> {
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

fn optional_bool(value: &Value, name: &str, default: bool) -> Result<bool, (i32, String)> {
    match value.get(name) {
        None => Ok(default),
        Some(value) => value
            .as_bool()
            .ok_or((-32602, format!("{name} must be a boolean"))),
    }
}

fn tool_definitions() -> Vec<Value> {
    vec![
        tool(
            "add_node",
            "Add a free-text node by node_name, optionally with a memo. Repeating the call is safe and returns added=false.",
            json!({"node_name":{"type":"string"},"memo":{"type":"string"}}),
            vec!["node_name"],
        ),
        tool(
            "update_node_name",
            "Change a node's string name. Memo is not changed. If the destination already exists, merge both nodes and rewire all edges.",
            json!({"from_node_name":{"type":"string"},"to_node_name":{"type":"string"}}),
            vec!["from_node_name", "to_node_name"],
        ),
        tool(
            "update_node_memo",
            "Replace the memo attached to one node.",
            json!({"node_name":{"type":"string"},"memo":{"type":"string"}}),
            vec!["node_name", "memo"],
        ),
        tool(
            "delete_node",
            "Physically delete a node and all incident edges. Re-adding the same node_name does not restore the old edges.",
            json!({"node_name":{"type":"string"}}),
            vec!["node_name"],
        ),
        tool(
            "add_edge",
            "Add one named ordered edge. Focus can traverse it toward next or previous. Missing nodes are created automatically. Multiple edges are valid.",
            json!({"edge_name":{"type":"string"},"previous":{"type":"string"},"next":{"type":"string"}}),
            vec!["edge_name", "previous", "next"],
        ),
        tool(
            "update_edge",
            "Update an edge by its persistent edge_id. Any supplied edge_name, previous, or next field is changed.",
            json!({"edge_id":{"type":"integer","minimum":1},"edge_name":{"type":"string"},"previous":{"type":"string"},"next":{"type":"string"}}),
            vec!["edge_id"],
        ),
        tool(
            "delete_edge",
            "Delete one edge by its persistent edge_id. Node memories are not deleted.",
            json!({"edge_id":{"type":"integer","minimum":1}}),
            vec!["edge_id"],
        ),
        tool(
            "edge_to_node",
            "Replace one edge with two same-named edges through a node. The node is created when missing. The change is atomic.",
            json!({"edge_id":{"type":"integer","minimum":1},"node_name":{"type":"string"}}),
            vec!["edge_id", "node_name"],
        ),
        tool(
            "add_sequence",
            "Add a linear sequence in the given order. Each adjacent pair becomes one edge whose previous and next directions are both traversable.",
            json!({"sequence":{"type":"array","items":{"type":"string"},"minItems":2},"edge_name":{"type":"string"}}),
            vec!["sequence", "edge_name"],
        ),
        tool(
            "focus",
            "Select at most limit nearby notes from one focus node. Continue in the same previous or next direction and with the same edge_name, then return SCC groups by edge_name. Set include_connections=true when edge IDs are needed for edge operations.",
            json!({"focus":{"type":"string"},"limit":{"type":"integer","minimum":0,"default":50},"include_connections":{"type":"boolean","default":false}}),
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
                "add_node",
                "update_node_name",
                "update_node_memo",
                "delete_node",
                "add_edge",
                "update_edge",
                "delete_edge",
                "edge_to_node",
                "add_sequence",
                "focus"
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
                "add_edge",
                json!({"edge_name":"task","previous":"過去","next":"中心"}),
            ),
            (
                2,
                "add_edge",
                json!({"edge_name":"task","previous":"中心","next":"後続"}),
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
            &json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"focus","arguments":{"focus":"中心","include_connections":true}}}).to_string(),
        );
        assert!(response["result"].get("structuredContent").is_none());
        let view: Value =
            serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        assert_eq!(view["focus"], "中心");
        assert_eq!(
            view["groups"],
            json!([{"edge_name":"task","previous":[["過去"]],"next":[["後続"]]}])
        );
        assert_eq!(view["connections"].as_array().unwrap().len(), 2);
        assert!(view["connections"][0].get("edge_id").is_some());
        assert_eq!(view["connections"][0]["edge_name"], "task");
        let edge_id = view["connections"][0]["edge_id"].as_i64().unwrap();
        for (id, name, arguments) in [
            (
                4,
                "update_edge",
                json!({"edge_id":edge_id,"next":"更新された後続"}),
            ),
            (5, "delete_edge", json!({"edge_id":edge_id})),
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

    #[test]
    fn add_sequence_adds_one_bidirectionally_traversable_edge_per_pair() {
        let mut store = MemoryStore::open(":memory:").unwrap();
        store.create_memory("m").unwrap();
        let response = handle_json_request(
            &mut store,
            "m",
            &json!({
                "jsonrpc":"2.0",
                "id":1,
                "method":"tools/call",
                "params":{"name":"add_sequence","arguments":{
                    "sequence":["A","B","C"],
                    "edge_name":"task"
                }}
            })
            .to_string(),
        );
        assert!(response.get("error").is_none(), "{response}");
        let memory = store.get_memory("m").unwrap();
        assert_eq!(memory.orders.len(), 2);
        assert_eq!(memory.orders[0].previous, "A");
        assert_eq!(memory.orders[0].next, "B");
        assert_eq!(memory.orders[1].previous, "B");
        assert_eq!(memory.orders[1].next, "C");
    }

    #[test]
    fn edge_to_node_returns_replacement_edge_ids() {
        let mut store = MemoryStore::open(":memory:").unwrap();
        store.create_memory("m").unwrap();
        let edge_id = store
            .add_order("m", "A", "B", "task")
            .unwrap()
            .order
            .edge_id;
        let response = handle_json_request(
            &mut store,
            "m",
            &json!({
                "jsonrpc":"2.0",
                "id":1,
                "method":"tools/call",
                "params":{"name":"edge_to_node","arguments":{
                    "edge_id":edge_id,
                    "node_name":"X"
                }}
            })
            .to_string(),
        );
        let output: Value =
            serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        assert_eq!(output["removed_edge_id"], edge_id);
        assert_eq!(output["added_edge_ids"].as_array().unwrap().len(), 2);
        let memory = store.get_memory("m").unwrap();
        assert_eq!(memory.orders.len(), 2);
    }
}
