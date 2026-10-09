//! Parsing real Claude Code `stream-json` output captured in `tests/fixtures/`.

use std::fs;
use std::path::PathBuf;

use videocrm_pipeline::{AgentEvent, parse_line};

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn events(name: &str) -> Vec<AgentEvent> {
    let text = fs::read_to_string(fixture_dir().join(name)).expect("fixture exists");
    text.lines().flat_map(parse_line).collect()
}

fn tool_use_names(events: &[AgentEvent]) -> Vec<(&str, &str)> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolUse { name, summary, .. } => Some((name.as_str(), summary.as_str())),
            _ => None,
        })
        .collect()
}

#[test]
fn every_captured_line_is_understood() {
    let mut files = 0;
    for entry in fs::read_dir(fixture_dir()).expect("fixtures dir") {
        let path = entry.expect("dir entry").path();
        let text = fs::read_to_string(&path).expect("fixture readable");
        for line in text.lines() {
            for event in parse_line(line) {
                assert!(!matches!(event, AgentEvent::Unknown { .. }), "{}: unexpected {event:?}", path.display());
            }
        }
        files += 1;
    }
    assert!(files >= 7, "fixtures missing");
}

#[test]
fn every_run_starts_with_init_and_ends_with_result() {
    for name in [
        "text_reply.jsonl",
        "tool_calls.jsonl",
        "resume.jsonl",
        "permission_denied.jsonl",
        "skill.jsonl",
        "error_bad_model.jsonl",
    ] {
        let events = events(name);
        assert!(matches!(events.first(), Some(AgentEvent::Init { .. })), "{name}");
        assert!(matches!(events.last(), Some(AgentEvent::Result { .. })), "{name}");
    }
}

#[test]
fn text_reply() {
    let events = events("text_reply.jsonl");
    assert!(matches!(
        &events[0],
        AgentEvent::Init { session_id, model, cwd, tools }
            if session_id == "8d866463-5779-4c0b-9025-12efe651c243"
                && model == "claude-haiku-4-5-20251001"
                && cwd == &PathBuf::from("/tmp/videocrm-capture")
                && tools.iter().any(|tool| tool == "Skill")
    ));
    // Redacted thinking blocks, thinking-token counters and rate-limit notices yield nothing.
    assert_eq!(
        events[1..],
        [
            AgentEvent::AssistantText { text: "ok".to_owned() },
            AgentEvent::Result {
                is_error: false,
                result_text: "ok".to_owned(),
                session_id: "8d866463-5779-4c0b-9025-12efe651c243".to_owned(),
                cost_usd: Some(0.0118336),
                duration_ms: Some(4510),
                num_turns: Some(1),
            },
        ]
    );
}

#[test]
fn tool_calls_are_summarised() {
    let events = events("tool_calls.jsonl");
    let AgentEvent::Init { cwd, tools, .. } = &events[0] else {
        panic!("init first");
    };
    assert_eq!(cwd, &PathBuf::from("/tmp/videocrm-capture/footage"));
    assert!(tools.iter().any(|tool| tool == "Bash"));
    assert_eq!(tool_use_names(&events), [("Bash", "ls"), ("Read", "/tmp/videocrm-capture/skilldir/SKILL.md")]);
    let results: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolResult { tool_use_id, is_error, summary } => {
                Some((tool_use_id.as_str(), *is_error, summary.as_str()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        results,
        [
            ("toolu_01U496iZCmKwbdXfkmachH6m", false, "take1.mp4\ntake2.mp4"),
            ("toolu_01CnuSxKSvUDTAhzERJPvHQm", false, "1\t# fake skill\n2"),
        ]
    );
    assert!(matches!(events.last(), Some(AgentEvent::Result { is_error: false, num_turns: Some(3), .. })));
}

#[test]
fn resumed_session_keeps_its_id() {
    let first = events("tool_calls.jsonl");
    let resumed = events("resume.jsonl");
    let session = |events: &[AgentEvent]| match events.last() {
        Some(AgentEvent::Result { session_id, .. }) => session_id.clone(),
        other => panic!("no result: {other:?}"),
    };
    assert_eq!(session(&first), session(&resumed));
    assert!(matches!(
        &resumed[0],
        AgentEvent::Init { session_id, .. } if *session_id == session(&first)
    ));
    assert!(matches!(
        resumed.last(),
        Some(AgentEvent::Result { result_text, .. }) if result_text == "take1.mp4"
    ));
}

#[test]
fn permission_denials_are_reported() {
    let events = events("permission_denied.jsonl");
    assert_eq!(tool_use_names(&events), [("ToolSearch", "select:WebFetch"), ("WebFetch", "https://example.com")]);
    assert!(events.contains(&AgentEvent::PermissionDenied {
        tool_name: "WebFetch".to_owned(),
        message: "Claude requested permissions to use WebFetch, but you haven't granted it yet.".to_owned(),
    }));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolResult { is_error: true, summary, .. } if summary.contains("WebFetch")
    )));
    // Non-text result blocks are named, not dropped.
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolResult { summary, .. } if summary == "[tool_reference]"
    )));
}

#[test]
fn skill_invocation_hides_injected_instructions() {
    let events = events("skill.jsonl");
    assert_eq!(tool_use_names(&events), [("Skill", "video-use")]);
    let texts: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::AssistantText { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, ["loaded"]);
}

#[test]
fn errors_carry_their_message() {
    let bad_model = events("error_bad_model.jsonl");
    assert!(matches!(
        bad_model.last(),
        Some(AgentEvent::Result { is_error: true, result_text, .. })
            if result_text.contains("issue with the selected model")
    ));

    // A failed `--resume` emits only a result, with the reason in `errors`.
    assert_eq!(
        events("error_bad_resume.jsonl"),
        [AgentEvent::Result {
            is_error: true,
            result_text: "No conversation found with session ID: 00000000-0000-0000-0000-000000000000".to_owned(),
            session_id: "00000000-0000-0000-0000-000000000000".to_owned(),
            cost_usd: Some(0.0),
            duration_ms: Some(0),
            num_turns: Some(0),
        }]
    );
}

#[test]
fn unexpected_input_never_fails() {
    assert_eq!(parse_line(""), []);
    assert_eq!(parse_line("   "), []);
    assert!(matches!(
        &parse_line("Warning: something odd")[..],
        [AgentEvent::Unknown { kind, excerpt }] if kind == "non_json" && excerpt == "Warning: something odd"
    ));
    assert!(matches!(
        &parse_line(r#"{"type":"stream_event","event":{}}"#)[..],
        [AgentEvent::Unknown { kind, .. }] if kind == "stream_event"
    ));
    assert!(matches!(
        &parse_line(r#"{"type":"system","subtype":"task_started","task_id":"a"}"#)[..],
        [AgentEvent::Unknown { kind, .. }] if kind == "system/task_started"
    ));
    assert!(matches!(&parse_line("[1,2]")[..], [AgentEvent::Unknown { kind, .. }] if kind == "untyped"));
    assert_eq!(parse_line(r#"{"type":"assistant","message":{"content":"plain"}}"#), []);
    assert_eq!(parse_line(r#"{"type":"assistant"}"#), []);

    // Unknown blocks do not hide the known ones next to them; missing fields default.
    let mixed = parse_line(
        r#"{"type":"assistant","message":{"content":[
            {"type":"hologram","data":1},
            {"type":"text","text":"hi","new_field":true},
            {"type":"tool_use","name":"Bash"}]}}"#,
    );
    assert!(matches!(&mixed[0], AgentEvent::Unknown { kind, .. } if kind == "assistant/hologram"));
    assert_eq!(mixed[1], AgentEvent::AssistantText { text: "hi".to_owned() });
    assert_eq!(mixed[2], AgentEvent::ToolUse { id: String::new(), name: "Bash".to_owned(), summary: String::new() });
    assert!(matches!(
        &parse_line(r#"{"type":"result"}"#)[..],
        [AgentEvent::Result { is_error: false, cost_usd: None, num_turns: None, .. }]
    ));
}

#[test]
fn long_tool_output_is_truncated() {
    let output = "x".repeat(5000);
    let line = format!(
        r#"{{"type":"user","message":{{"content":[{{"type":"tool_result","tool_use_id":"t","content":"{output}"}}]}}}}"#
    );
    let [AgentEvent::ToolResult { summary, .. }] = &parse_line(&line)[..] else {
        panic!("tool result");
    };
    assert_eq!(summary.chars().count(), 300);
    assert!(summary.ends_with('…'));
}

#[test]
fn events_round_trip_through_json() {
    for name in ["tool_calls.jsonl", "permission_denied.jsonl", "error_bad_resume.jsonl"] {
        for event in events(name) {
            let json = serde_json::to_string(&event).expect("serialize");
            let back: AgentEvent = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, event);
        }
    }
    let json = serde_json::to_value(AgentEvent::Stderr { line: "boom".to_owned() }).unwrap();
    assert_eq!(json, serde_json::json!({ "type": "stderr", "line": "boom" }));
}
