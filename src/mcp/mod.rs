//! A [Model Context Protocol](https://modelcontextprotocol.io) server that lets
//! AI agents drive Rojo on their own: serve a project, ask Studio to connect,
//! build, and look up which files make up which instances.
//!
//! It speaks the MCP stdio transport: newline-delimited JSON-RPC 2.0 messages
//! on standard input and output. Standard output carries nothing but protocol
//! messages, so everything else is logged to standard error.

mod serve;
mod tools;
mod tree;

use std::{
    io::{self, BufRead, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
};

use serde_json::{json, Value};

use self::tools::Tools;

/// Protocol versions this server can speak, newest first. The tools API it
/// uses is the same in all of them.
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] =
    &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "\
Rojo syncs a Roblox project on disk into Roblox Studio.

Typical flow: call `serve_start` (it serves the project this server was started in unless you \
pass `project`), then `studio_connect` so Studio connects and syncs. After that, edit files on \
disk and Rojo syncs every change into Studio live. `serve_status` shows whether Studio is \
connected and `serve_logs` shows the server output.

Studio connects on its own only if the Rojo plugin's \"Agent Connections\" setting is on, and the \
plugin only looks at the host and port it is set to (localhost:34872 by default), so keep the \
default port unless the project sets another one. If the plugin asks to confirm the first sync, \
someone has to accept it in Studio.

`find_instances`, `get_instance`, `file_to_instance` and `instance_tree` show how files map to \
instances, which tells you which file to edit for a given instance. Instance paths are written \
like `ReplicatedStorage.Shared.Module`.";

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// A JSON-RPC error for a request that could not be handled at all. Failures
/// of a tool itself are reported in its result instead, so the agent sees
/// them.
#[derive(Debug)]
pub struct RpcError {
    code: i64,
    message: String,
}

impl RpcError {
    fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: INVALID_PARAMS,
            message: message.into(),
        }
    }
}

/// Runs the server until standard input is closed.
pub fn run_stdio(default_project: PathBuf) -> anyhow::Result<()> {
    let stdout = Arc::new(Mutex::new(io::stdout()));
    let tools = Arc::new(Tools::new(default_project));

    log::info!("Rojo MCP server ready on standard input and output");

    for line in io::stdin().lock().lines() {
        let line = match line {
            Ok(line) => line,
            Err(err) => {
                log::error!("Could not read from standard input: {}", err);
                break;
            }
        };

        if line.trim().is_empty() {
            continue;
        }

        let message: Value = match serde_json::from_str(&line) {
            Ok(message) => message,
            Err(err) => {
                send(
                    &stdout,
                    &error_response(Value::Null, PARSE_ERROR, format!("Parse error: {}", err)),
                );
                continue;
            }
        };

        // Tool calls can take a while, such as waiting for Studio to connect,
        // so they run on their own thread and pings keep being answered.
        if message.get("method").and_then(Value::as_str) == Some("tools/call") {
            let tools = Arc::clone(&tools);
            let stdout = Arc::clone(&stdout);
            thread::spawn(move || {
                if let Some(response) = handle_message(&tools, message) {
                    send(&stdout, &response);
                }
            });
        } else if let Some(response) = handle_message(&tools, message) {
            send(&stdout, &response);
        }
    }

    // The client went away. Servers started for it are of no use anymore.
    tools.shutdown();

    Ok(())
}

fn send(stdout: &Mutex<io::Stdout>, message: &Value) {
    let mut stdout = stdout.lock().unwrap();
    let result = serde_json::to_writer(&mut *stdout, message)
        .map_err(io::Error::from)
        .and_then(|()| stdout.write_all(b"\n"))
        .and_then(|()| stdout.flush());

    if let Err(err) = result {
        log::error!("Could not write to standard output: {}", err);
    }
}

/// Handles one message, returning the response to send if it needs one.
fn handle_message(tools: &Tools, message: Value) -> Option<Value> {
    let id = message.get("id").cloned();

    let Some(method) = message.get("method").and_then(Value::as_str) else {
        // This server never sends requests, so a message without a method is
        // not something it can answer.
        let is_response = message.get("result").is_some() || message.get("error").is_some();
        return match id {
            Some(id) if !is_response => Some(error_response(
                id,
                INVALID_REQUEST,
                "Invalid request: missing method".to_owned(),
            )),
            _ if message.is_array() => Some(error_response(
                Value::Null,
                INVALID_REQUEST,
                "Invalid request: batches are not supported".to_owned(),
            )),
            _ => None,
        };
    };

    // Notifications, such as `notifications/initialized`, need no response.
    let id = id?;
    let params = message.get("params").cloned().unwrap_or(Value::Null);

    let result = match method {
        "initialize" => Ok(initialize_result(&params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools::definitions() })),
        "tools/call" => tools.call(&params),
        "resources/list" => Ok(json!({ "resources": [] })),
        "resources/templates/list" => Ok(json!({ "resourceTemplates": [] })),
        "prompts/list" => Ok(json!({ "prompts": [] })),
        _ => Err(RpcError {
            code: METHOD_NOT_FOUND,
            message: format!("Method not found: {}", method),
        }),
    };

    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(err) => error_response(id, err.code, err.message),
    })
}

fn initialize_result(params: &Value) -> Value {
    let requested = params.get("protocolVersion").and_then(Value::as_str);

    // Use the client's version if we know it, and otherwise offer our newest
    // one; the client decides whether it can work with that.
    let protocol_version = requested
        .filter(|version| SUPPORTED_PROTOCOL_VERSIONS.contains(version))
        .unwrap_or(SUPPORTED_PROTOCOL_VERSIONS[0]);

    json!({
        "protocolVersion": protocol_version,
        "capabilities": {
            "tools": { "listChanged": false },
        },
        "serverInfo": {
            "name": "rojo",
            "title": "Rojo",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "instructions": INSTRUCTIONS,
    })
}

fn error_response(id: Value, code: i64, message: String) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

#[cfg(test)]
mod test {
    use super::*;

    fn tools() -> Tools {
        Tools::new(PathBuf::from("."))
    }

    #[test]
    fn initialize_echoes_supported_versions_and_offers_the_newest_otherwise() {
        let known = handle_message(
            &tools(),
            json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
                    "params": { "protocolVersion": "2025-03-26" } }),
        )
        .unwrap();
        assert_eq!(known["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(known["result"]["serverInfo"]["name"], "rojo");
        assert!(known["result"]["capabilities"]["tools"].is_object());

        let unknown = handle_message(
            &tools(),
            json!({ "jsonrpc": "2.0", "id": 2, "method": "initialize",
                    "params": { "protocolVersion": "1999-01-01" } }),
        )
        .unwrap();
        assert_eq!(
            unknown["result"]["protocolVersion"],
            SUPPORTED_PROTOCOL_VERSIONS[0]
        );
    }

    #[test]
    fn notifications_and_responses_get_no_reply() {
        let tools = tools();
        assert!(handle_message(
            &tools,
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })
        )
        .is_none());
        assert!(
            handle_message(&tools, json!({ "jsonrpc": "2.0", "id": 5, "result": {} })).is_none()
        );
    }

    #[test]
    fn unknown_methods_and_tools_are_errors() {
        let tools = tools();

        let method = handle_message(
            &tools,
            json!({ "jsonrpc": "2.0", "id": "a", "method": "does/not/exist" }),
        )
        .unwrap();
        assert_eq!(method["id"], "a");
        assert_eq!(method["error"]["code"], METHOD_NOT_FOUND);

        let tool = handle_message(
            &tools,
            json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                    "params": { "name": "does_not_exist", "arguments": {} } }),
        )
        .unwrap();
        assert_eq!(tool["error"]["code"], INVALID_PARAMS);
    }

    #[test]
    fn tools_list_describes_every_tool_with_an_object_schema() {
        let response = handle_message(
            &tools(),
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
        )
        .unwrap();

        let listed = response["result"]["tools"].as_array().unwrap();
        assert!(!listed.is_empty());
        for tool in listed {
            assert!(tool["name"].is_string());
            assert!(tool["description"].is_string());
            assert_eq!(tool["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn batches_are_rejected() {
        let response = handle_message(&tools(), json!([{ "jsonrpc": "2.0", "id": 1 }])).unwrap();
        assert_eq!(response["error"]["code"], INVALID_REQUEST);
    }
}
