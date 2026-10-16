//! The agent's `stream-json` protocol: one JSON object per stdout line.
//!
//! Only what the app shows is modelled. Lines are read leniently through
//! `serde_json::Value`, so new fields, content blocks and event types never fail a run:
//! anything unrecognised becomes [`AgentEvent::Unknown`].

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::process::AgentProcess;

/// Longest tool-call summary, in characters.
const TOOL_SUMMARY_CHARS: usize = 120;
/// Longest tool-result summary, in characters.
const RESULT_SUMMARY_CHARS: usize = 300;
/// Longest raw excerpt kept for unknown lines, in characters.
const UNKNOWN_EXCERPT_CHARS: usize = 200;

/// One step of an agent run, ready to show in a timeline and to persist.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    /// The agent process started. Reported by this crate rather than read from the
    /// agent, before anything else: its process group, to find it again if the app
    /// dies while it runs (see [`crate::stop_leftover_agent`]).
    Spawned {
        process: AgentProcess,
    },
    /// First event of every run.
    Init {
        session_id: String,
        model: String,
        cwd: PathBuf,
        tools: Vec<String>,
    },
    AssistantText {
        text: String,
    },
    /// Visible reasoning. Redacted (empty) thinking blocks are skipped.
    Thinking {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        /// Short human summary of the input: the command, file path, skill name, ...
        summary: String,
    },
    ToolResult {
        tool_use_id: String,
        is_error: bool,
        /// Start of the tool output.
        summary: String,
    },
    /// A tool call was refused by the permission mode / allowed tools.
    PermissionDenied {
        tool_name: String,
        message: String,
    },
    /// Last event of every run.
    Result {
        is_error: bool,
        /// Final assistant message, or the error description.
        result_text: String,
        session_id: String,
        cost_usd: Option<f64>,
        duration_ms: Option<u64>,
        num_turns: Option<u32>,
    },
    /// A line the agent wrote to stderr.
    Stderr {
        line: String,
    },
    /// A line this crate does not understand (new event type, non-JSON output, ...).
    Unknown {
        /// `type` (and `/subtype`) of the line, or `non_json`.
        kind: String,
        excerpt: String,
    },
}

/// Parses one stdout line into zero or more events. Never fails: keep-alive and
/// bookkeeping lines yield nothing, unrecognised lines yield [`AgentEvent::Unknown`].
pub fn parse_line(line: &str) -> Vec<AgentEvent> {
    let line = line.trim();
    if line.is_empty() {
        return Vec::new();
    }
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return vec![unknown("non_json", line)];
    };
    match str_field(&value, "type") {
        "system" => parse_system(&value, line),
        "assistant" => parse_assistant(&value),
        "user" => parse_user(&value),
        "result" => vec![parse_result(&value)],
        "rate_limit_event" => Vec::new(),
        "" => vec![unknown("untyped", line)],
        kind => vec![unknown(kind, line)],
    }
}

fn parse_system(value: &Value, line: &str) -> Vec<AgentEvent> {
    match str_field(value, "subtype") {
        "init" => vec![AgentEvent::Init {
            session_id: str_field(value, "session_id").to_owned(),
            model: str_field(value, "model").to_owned(),
            cwd: PathBuf::from(str_field(value, "cwd")),
            tools: value
                .get("tools")
                .and_then(Value::as_array)
                .map(|tools| tools.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                .unwrap_or_default(),
        }],
        "permission_denied" => vec![AgentEvent::PermissionDenied {
            tool_name: str_field(value, "tool_name").to_owned(),
            message: str_field(value, "message").to_owned(),
        }],
        "thinking_tokens" => Vec::new(),
        subtype => vec![unknown(&format!("system/{subtype}"), line)],
    }
}

fn parse_assistant(value: &Value) -> Vec<AgentEvent> {
    content_blocks(value)
        .filter_map(|block| match str_field(block, "type") {
            "text" => {
                non_empty(str_field(block, "text")).map(|text| AgentEvent::AssistantText { text: text.to_owned() })
            }
            "thinking" => {
                non_empty(str_field(block, "thinking")).map(|text| AgentEvent::Thinking { text: text.to_owned() })
            }
            "redacted_thinking" => None,
            "tool_use" | "server_tool_use" => {
                let name = str_field(block, "name");
                Some(AgentEvent::ToolUse {
                    id: str_field(block, "id").to_owned(),
                    name: name.to_owned(),
                    summary: tool_use_summary(name, block.get("input").unwrap_or(&Value::Null)),
                })
            }
            kind => Some(unknown(&format!("assistant/{kind}"), &block.to_string())),
        })
        .collect()
}

fn parse_user(value: &Value) -> Vec<AgentEvent> {
    content_blocks(value)
        .filter_map(|block| match str_field(block, "type") {
            "tool_result" => Some(AgentEvent::ToolResult {
                tool_use_id: str_field(block, "tool_use_id").to_owned(),
                is_error: block.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                summary: truncate(&content_text(block.get("content").unwrap_or(&Value::Null)), RESULT_SUMMARY_CHARS),
            }),
            // Prompt echoes and injected skill instructions.
            "text" => None,
            kind => Some(unknown(&format!("user/{kind}"), &block.to_string())),
        })
        .collect()
}

fn parse_result(value: &Value) -> AgentEvent {
    let errors = value
        .get("errors")
        .and_then(Value::as_array)
        .map(|errors| errors.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("; "))
        .unwrap_or_default();
    let result_text =
        non_empty(str_field(value, "result")).or(non_empty(&errors)).unwrap_or(str_field(value, "subtype")).to_owned();
    AgentEvent::Result {
        is_error: value.get("is_error").and_then(Value::as_bool).unwrap_or(false),
        result_text,
        session_id: str_field(value, "session_id").to_owned(),
        cost_usd: value.get("total_cost_usd").and_then(Value::as_f64),
        duration_ms: value.get("duration_ms").and_then(Value::as_u64),
        num_turns: value.get("num_turns").and_then(Value::as_u64).and_then(|turns| u32::try_from(turns).ok()),
    }
}

/// Parses one line of Codex's `exec --json` output, the same way as [`parse_line`]:
/// the thread becomes [`AgentEvent::Init`], messages and shell commands the matching
/// events, the end of the turn [`AgentEvent::Result`] (without text: Codex ends with
/// its last message).
pub fn parse_codex_line(line: &str) -> Vec<AgentEvent> {
    let line = line.trim();
    if line.is_empty() {
        return Vec::new();
    }
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return vec![unknown("non_json", line)];
    };
    let item = value.get("item").unwrap_or(&Value::Null);
    let item_id = || str_field(item, "id").to_owned();
    let result = |is_error: bool, result_text: &str| AgentEvent::Result {
        is_error,
        result_text: result_text.to_owned(),
        session_id: String::new(),
        cost_usd: None,
        duration_ms: None,
        num_turns: None,
    };
    match (str_field(&value, "type"), str_field(item, "type")) {
        ("thread.started", _) => vec![AgentEvent::Init {
            session_id: str_field(&value, "thread_id").to_owned(),
            model: String::new(),
            cwd: PathBuf::new(),
            tools: Vec::new(),
        }],
        ("turn.started", _) => Vec::new(),
        ("turn.completed", _) => vec![result(false, "")],
        ("turn.failed", _) => {
            let message = value.pointer("/error/message").and_then(Value::as_str).unwrap_or("the turn failed");
            vec![result(true, message)]
        }
        // Stream trouble Codex reports (and often retries); it explains a failure.
        ("error", _) => vec![AgentEvent::Stderr { line: str_field(&value, "message").to_owned() }],
        ("item.started", "command_execution") => {
            vec![AgentEvent::ToolUse { id: item_id(), name: "Shell".to_owned(), summary: codex_command(item) }]
        }
        ("item.completed", "command_execution") => vec![AgentEvent::ToolResult {
            tool_use_id: item_id(),
            is_error: item.get("exit_code").and_then(Value::as_i64).is_none_or(|code| code != 0),
            summary: truncate(str_field(item, "aggregated_output"), RESULT_SUMMARY_CHARS),
        }],
        ("item.completed", "agent_message") => non_empty(str_field(item, "text"))
            .map(|text| AgentEvent::AssistantText { text: text.to_owned() })
            .into_iter()
            .collect(),
        ("item.completed", "reasoning") => non_empty(str_field(item, "text"))
            .map(|text| AgentEvent::Thinking { text: text.to_owned() })
            .into_iter()
            .collect(),
        ("item.started" | "item.updated", _) => Vec::new(),
        ("item.completed", kind) => {
            let summary = match kind {
                "file_change" => item.get("changes").and_then(Value::as_array).map(|changes| {
                    changes.iter().map(|change| str_field(change, "path")).collect::<Vec<_>>().join(", ")
                }),
                "web_search" => non_empty(str_field(item, "query")).map(str::to_owned),
                "mcp_tool_call" => Some(format!("{}/{}", str_field(item, "server"), str_field(item, "tool"))),
                _ => None,
            };
            match summary {
                Some(summary) => vec![AgentEvent::ToolUse {
                    id: item_id(),
                    name: kind.to_owned(),
                    summary: truncate(&summary, TOOL_SUMMARY_CHARS),
                }],
                None => vec![unknown(&format!("item/{kind}"), line)],
            }
        }
        ("", _) => vec![unknown("untyped", line)],
        (kind, _) => vec![unknown(kind, line)],
    }
}

/// The first line of a shell command Codex ran, without the `bash -lc` around it.
fn codex_command(item: &Value) -> String {
    let command = str_field(item, "command");
    let inner = command
        .split_once(" -lc ")
        .map(|(_, inner)| inner.trim().trim_matches(|quote| quote == '\'' || quote == '"'))
        .unwrap_or(command);
    let mut lines = inner.lines().map(str::trim).filter(|line| !line.is_empty());
    let first = lines.next().unwrap_or_default();
    let summary = if lines.next().is_some() { format!("{first} …") } else { first.to_owned() };
    truncate(&summary, TOOL_SUMMARY_CHARS)
}

/// Blocks of `message.content`; a plain-string content has none.
fn content_blocks(value: &Value) -> impl Iterator<Item = &Value> {
    value.pointer("/message/content").and_then(Value::as_array).into_iter().flatten()
}

/// One line describing what a tool call does, picked per well-known tool.
fn tool_use_summary(name: &str, input: &Value) -> String {
    let field = |key: &str| non_empty(str_field(input, key));
    let summary = match name {
        "Bash" => field("command").map(|command| {
            let mut lines = command.lines().map(str::trim).filter(|line| !line.is_empty());
            let first = lines.next().unwrap_or_default();
            if lines.next().is_some() { format!("{first} …") } else { first.to_owned() }
        }),
        "Read" | "Write" | "Edit" | "MultiEdit" => field("file_path").map(str::to_owned),
        "NotebookEdit" => field("notebook_path").map(str::to_owned),
        "Glob" | "Grep" => field("pattern").map(str::to_owned),
        "Skill" => field("skill").map(str::to_owned),
        "Agent" | "Task" => field("description").map(str::to_owned),
        "WebFetch" => field("url").map(str::to_owned),
        "WebSearch" | "ToolSearch" => field("query").map(str::to_owned),
        "TaskCreate" => field("subject").map(str::to_owned),
        _ => None,
    };
    let summary = summary.unwrap_or_else(|| match input {
        Value::Null => String::new(),
        Value::Object(map) if map.is_empty() => String::new(),
        input => input.to_string(),
    });
    truncate(&summary, TOOL_SUMMARY_CHARS)
}

/// Text of a tool result: a string, or a list of blocks where non-text blocks
/// (images, tool references) show as `[type]`.
fn content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .map(|block| match str_field(block, "type") {
                "text" => str_field(block, "text").to_owned(),
                kind => format!("[{kind}]"),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn unknown(kind: &str, raw: &str) -> AgentEvent {
    AgentEvent::Unknown { kind: kind.to_owned(), excerpt: truncate(raw, UNKNOWN_EXCERPT_CHARS) }
}

fn str_field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn non_empty(text: &str) -> Option<&str> {
    (!text.trim().is_empty()).then_some(text)
}

/// Trims and cuts `text` to at most `max` characters, marking the cut with `…`.
pub(crate) fn truncate(text: &str, max: usize) -> String {
    let text = text.trim();
    match text.char_indices().nth(max) {
        None => text.to_owned(),
        Some(_) => {
            let cut: String = text.chars().take(max.saturating_sub(1)).collect();
            format!("{}…", cut.trim_end())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_lines_become_the_same_events() {
        let events: Vec<AgentEvent> = [
            r#"{"type":"thread.started","thread_id":"01a1-thread"}"#,
            r#"{"type":"turn.started"}"#,
            r#"{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"Drawing three ideas."}}"#,
            r#"{"type":"item.started","item":{"id":"item_1","type":"command_execution","command":"/bin/bash -lc 'mkdir -p out\ncp a.png out/'","aggregated_output":"","exit_code":null,"status":"in_progress"}}"#,
            r#"{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"/bin/bash -lc 'mkdir -p out'","aggregated_output":"done\n","exit_code":0,"status":"completed"}}"#,
            r#"{"type":"item.completed","item":{"id":"item_2","type":"command_execution","command":"/bin/bash -lc false","aggregated_output":"","exit_code":1,"status":"failed"}}"#,
            r#"{"type":"error","message":"Reconnecting... 1/5"}"#,
            r#"{"type":"item.completed","item":{"id":"item_3","type":"image_generation_call"}}"#,
            r#"{"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":5}}"#,
        ]
        .iter()
        .flat_map(|line| parse_codex_line(line))
        .collect();
        let tool_result = |id: &str, is_error: bool, summary: &str| AgentEvent::ToolResult {
            tool_use_id: id.to_owned(),
            is_error,
            summary: summary.to_owned(),
        };
        assert_eq!(
            events[..6],
            [
                AgentEvent::Init {
                    session_id: "01a1-thread".into(),
                    model: String::new(),
                    cwd: PathBuf::new(),
                    tools: Vec::new()
                },
                AgentEvent::AssistantText { text: "Drawing three ideas.".into() },
                AgentEvent::ToolUse { id: "item_1".into(), name: "Shell".into(), summary: "mkdir -p out …".into() },
                tool_result("item_1", false, "done"),
                tool_result("item_2", true, ""),
                AgentEvent::Stderr { line: "Reconnecting... 1/5".into() },
            ]
        );
        assert!(matches!(&events[6], AgentEvent::Unknown { kind, .. } if kind == "item/image_generation_call"));
        assert!(matches!(&events[7], AgentEvent::Result { is_error: false, .. }));
        let failed = parse_codex_line(r#"{"type":"turn.failed","error":{"message":"You've hit your usage limit."}}"#);
        assert!(
            matches!(&failed[..], [AgentEvent::Result { is_error: true, result_text, .. }] if result_text == "You've hit your usage limit.")
        );
    }

    #[test]
    fn truncate_counts_characters_not_bytes() {
        assert_eq!(truncate("  short  ", 10), "short");
        assert_eq!(truncate("ééééé", 5), "ééééé");
        assert_eq!(truncate("éééééé", 5), "éééé…");
    }

    #[test]
    fn bash_summary_is_first_command_line() {
        let input = serde_json::json!({ "command": "\ncd edit\nffmpeg -i a.mp4 b.mp4" });
        assert_eq!(tool_use_summary("Bash", &input), "cd edit …");
    }

    #[test]
    fn unknown_tool_summary_falls_back_to_input_json() {
        let input = serde_json::json!({ "x": 1 });
        assert_eq!(tool_use_summary("Mystery", &input), r#"{"x":1}"#);
        assert_eq!(tool_use_summary("Mystery", &serde_json::json!({})), "");
    }
}
