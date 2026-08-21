use crate::ordered_memory::Memory;
use crate::path::find_observed_path;
use serde::Deserialize;
use serde_json::{Map, Value, json};

#[derive(Debug, Deserialize)]
struct Request {
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

pub fn handle_json_request(
    memory: &Memory,
    max_focus_calls: usize,
    focus_limit: usize,
    request: Value,
) -> Value {
    let request = match serde_json::from_value::<Request>(request) {
        Ok(request) => request,
        Err(error) => return rpc_error(Value::Null, -32600, format!("invalid request: {error}")),
    };
    let id = request.id.unwrap_or(Value::Null);
    let notification = id.is_null();
    let result = match request.method.as_str() {
        "initialize" => Ok(json!({
            "protocolVersion":"2025-06-18",
            "capabilities":{"tools":{}},
            "serverInfo":{"name":"mineplan-path","version":env!("MINEPLAN_BUILD_VERSION")}
        })),
        "ping" => Ok(json!({})),
        "notifications/initialized" | "notifications/cancelled" => return Value::Null,
        "tools/list" => Ok(json!({"tools":[tool_definition()]})),
        "tools/call" => call_tool(memory, max_focus_calls, focus_limit, request.params),
        _ => Err((-32601, format!("method not found: {}", request.method))),
    };
    if notification {
        return Value::Null;
    }
    match result {
        Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
        Err((code, message)) => rpc_error(id, code, message),
    }
}

fn call_tool(
    memory: &Memory,
    max_focus_calls: usize,
    focus_limit: usize,
    params: Value,
) -> Result<Value, (i32, String)> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or((-32602, "tools/call requires name".into()))?;
    if name != "find_path" {
        return Err((-32602, format!("unknown tool: {name}")));
    }
    let arguments = params
        .get("arguments")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_else(Map::new);
    let from = required_string(&arguments, "from")?;
    let to = required_string(&arguments, "to")?;
    let output = match find_observed_path(memory, from, to, max_focus_calls, focus_limit)
        .map_err(|message| (-32000, message))?
    {
        Some(path) => json!({
            "from":from,
            "to":to,
            "turns":path.turns,
            "tasks":path.tasks
        }),
        None => json!({"from":from,"to":to,"found":false}),
    };
    Ok(json!({
        "content":[{"type":"text","text":serde_json::to_string(&output).expect("serializable output")}]
    }))
}

pub fn tool_definition() -> Value {
    json!({
        "name":"find_path",
        "description":"Find one low-turn route between two mineplan nodes. A turn is a change of edge_name. found=false means no route was found through bounded focus-style observation of one memory snapshot.",
        "inputSchema":{
            "type":"object",
            "properties":{"from":{"type":"string"},"to":{"type":"string"}},
            "required":["from","to"],
            "additionalProperties":false
        }
    })
}

fn required_string<'a>(
    arguments: &'a Map<String, Value>,
    name: &str,
) -> Result<&'a str, (i32, String)> {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .ok_or((-32602, format!("{name} must be a non-empty string")))
}

fn rpc_error(id: Value, code: i32, message: String) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ordered_memory::{Memory, Order};
    use std::collections::HashMap;

    fn memory() -> Memory {
        Memory {
            memory_id: "test".into(),
            notes: ["A", "B", "C"].into_iter().map(String::from).collect(),
            memos: HashMap::new(),
            orders: vec![
                Order {
                    edge_id: 1,
                    edge_name: "x".into(),
                    previous: "A".into(),
                    next: "B".into(),
                },
                Order {
                    edge_id: 2,
                    edge_name: "y".into(),
                    previous: "B".into(),
                    next: "C".into(),
                },
            ],
        }
    }

    #[test]
    fn exposes_only_find_path() {
        let response = handle_json_request(
            &memory(),
            50,
            50,
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        );
        assert_eq!(response["result"]["tools"].as_array().unwrap().len(), 1);
        assert_eq!(response["result"]["tools"][0]["name"], "find_path");
    }

    #[test]
    fn returns_task_sequences() {
        let response = handle_json_request(
            &memory(),
            50,
            50,
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"find_path","arguments":{"from":"A","to":"C"}}}),
        );
        let output: Value =
            serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        assert_eq!(output["turns"], 1);
        assert_eq!(output["tasks"][0]["sequence"], json!(["A", "B"]));
        assert_eq!(output["tasks"][1]["sequence"], json!(["B", "C"]));
    }
}
