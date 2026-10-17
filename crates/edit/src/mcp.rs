//! A minimal MCP server over stdio (newline-delimited JSON-RPC 2.0): `initialize`,
//! `tools/list`, `tools/call` and `ping`. The coding agent starts it as
//! `<rushes> edit-tools --footage <dir>` and calls the editing tools through it.

use std::io::{BufRead, Write};
use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;

use serde_json::{Value, json};

use crate::tools::{Tools, definitions};

const PROTOCOL_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// What the server tells the agent about itself (shown with the tool list).
const INSTRUCTIONS: &str = "Rushes editing tools for the footage folder you are in. Work through them: \
inventory, transcribe, read_transcript, silences, shots and look to understand the material; plan_cut to \
set the cut by phrase ids; render a preview, check it, fix, then render final.";

/// Serves until `input` closes.
pub fn serve(footage_dir: &Path, input: impl BufRead, output: impl Write + Send) -> std::io::Result<()> {
    let output = Mutex::new(output);
    let send = |message: &Value| {
        let mut out = output.lock().unwrap();
        let _ = writeln!(out, "{message}");
        let _ = out.flush();
    };
    let log_path = footage_dir.join("edit").join(".cache").join("tools.log");
    let log = |line: &str| {
        if let Some(dir) = log_path.parent()
            && std::fs::create_dir_all(dir).is_ok()
            && let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&log_path)
        {
            let _ = writeln!(file, "{line}");
        }
    };
    let mut tools = Tools::new(footage_dir);
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            send(&json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "parse error"}}));
            continue;
        };
        let Some(method) = message["method"].as_str() else { continue };
        let id = message.get("id").cloned();
        let Some(id) = id else { continue }; // a notification: nothing to answer
        let params = &message["params"];
        let result = match method {
            "initialize" => {
                let requested = params["protocolVersion"].as_str().unwrap_or_default();
                let version =
                    PROTOCOL_VERSIONS.iter().find(|v| **v == requested).copied().unwrap_or(PROTOCOL_VERSIONS[1]);
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "rushes", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": INSTRUCTIONS,
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": definitions()})),
            "tools/call" => {
                let name = params["name"].as_str().unwrap_or_default().to_owned();
                let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
                let token = params["_meta"].get("progressToken").cloned();
                let counter = Mutex::new(0u64);
                let progress = |text: &str| {
                    if let Some(token) = &token {
                        let mut n = counter.lock().unwrap();
                        *n += 1;
                        send(&json!({"jsonrpc": "2.0", "method": "notifications/progress",
                            "params": {"progressToken": token, "progress": *n, "message": text}}));
                    }
                };
                let started = Instant::now();
                let result = tools.call(&name, arguments.clone(), &progress);
                log(&format!(
                    "{} {name} {:.1}s {} args={}",
                    if result.is_error { "ERR" } else { "ok " },
                    started.elapsed().as_secs_f64(),
                    if result.is_error { first_text(&result.to_json()) } else { String::new() },
                    arguments
                ));
                Ok(result.to_json())
            }
            _ => Err(json!({"code": -32601, "message": format!("method not found: {method}")})),
        };
        match result {
            Ok(result) => send(&json!({"jsonrpc": "2.0", "id": id, "result": result})),
            Err(error) => send(&json!({"jsonrpc": "2.0", "id": id, "error": error})),
        }
    }
    Ok(())
}

fn first_text(result: &Value) -> String {
    result["content"][0]["text"].as_str().unwrap_or_default().chars().take(300).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_initialize_list_and_unknown_methods() {
        let dir = tempfile::tempdir().unwrap();
        let input = [
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"inventory","arguments":{}}}"#,
            r#"{"jsonrpc":"2.0","id":4,"method":"resources/list"}"#,
        ]
        .join("\n");
        let mut output = Vec::new();
        serve(dir.path(), input.as_bytes(), &mut output).unwrap();
        let replies: Vec<Value> =
            String::from_utf8(output).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(replies.len(), 4, "the notification gets no reply");
        assert_eq!(replies[0]["result"]["protocolVersion"], "2025-06-18");
        let names: Vec<&str> =
            replies[1]["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"plan_cut") && names.contains(&"render"), "{names:?}");
        // An empty folder: the tool fails, the server answers with an error result.
        assert_eq!(replies[2]["result"]["isError"], true);
        assert!(replies[2]["result"]["content"][0]["text"].as_str().unwrap().contains("no media files"));
        assert_eq!(replies[3]["error"]["code"], -32601);
    }
}
