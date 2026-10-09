//! A run's event log and how it folds into the timeline the panel shows.
//!
//! Every [`AgentEvent`] is persisted as a run event, together with a [`PhaseStarted`]
//! marker each time the app starts a phase (with the creator's notes or feedback).
//! [`fold_timeline`] pairs tool calls with their results and collapses noise
//! (stderr, sub-agent lifecycle and other unknown lines) into single rows.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use videocrm_core::RunEvent;
use videocrm_pipeline::AgentEvent;

/// Which kind of agent phase the app started.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseKind {
    /// Inventory, transcribe and propose a strategy.
    Plan,
    /// Rework the proposed strategy with the creator's feedback; nothing is rendered.
    Revise,
    /// The creator approved the strategy: render.
    Approve,
    /// Change a rendered edit and render again.
    Feedback,
    /// Plan and render in one pass.
    Auto,
}

impl PhaseKind {
    /// Whether the phase ends with a rendered cut rather than a strategy.
    pub fn renders(self) -> bool {
        matches!(self, PhaseKind::Approve | PhaseKind::Feedback | PhaseKind::Auto)
    }
}

/// Marker the app appends to the event log when it starts a phase.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename = "phase_started")]
pub struct PhaseStarted {
    pub phase: PhaseKind,
    /// The creator's notes (approve) or feedback (revise, feedback).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// One persisted run event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LogEvent {
    Agent(AgentEvent),
    Phase(PhaseStarted),
}

impl LogEvent {
    pub fn to_payload(&self) -> serde_json::Value {
        // Both variants are plain data with string keys, which always serialise.
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}

/// A run event with its place in the log.
#[derive(Clone, Debug, PartialEq)]
pub struct TimelineEntry {
    /// 1-based position in the run's log; the stable identity of timeline rows.
    pub seq: u32,
    pub at: DateTime<Utc>,
    pub event: LogEvent,
}

impl From<RunEvent> for TimelineEntry {
    fn from(event: RunEvent) -> Self {
        let parsed = serde_json::from_value(event.payload.clone()).unwrap_or_else(|_| {
            LogEvent::Agent(AgentEvent::Unknown { kind: "unreadable".to_owned(), excerpt: event.payload.to_string() })
        });
        Self { seq: event.seq, at: event.at, event: parsed }
    }
}

/// What a tool call returned.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolOutput {
    pub is_error: bool,
    pub summary: String,
}

/// One row of the timeline. `seq` is the log position of the row's first event.
#[derive(Clone, Debug, PartialEq)]
pub enum TimelineItem {
    Phase {
        seq: u32,
        phase: PhaseKind,
        text: Option<String>,
    },
    Message {
        seq: u32,
        text: String,
    },
    Thinking {
        seq: u32,
        text: String,
    },
    Tool {
        seq: u32,
        name: String,
        summary: String,
        output: Option<ToolOutput>,
    },
    /// A refused tool call or an error the agent reported.
    Problem {
        seq: u32,
        title: String,
        detail: String,
    },
    /// The agent finished a phase.
    Finished {
        seq: u32,
        cost_usd: Option<f64>,
        duration_ms: Option<u64>,
    },
    /// Consecutive stderr lines, sub-agent lifecycle events and other lines the app
    /// does not show individually.
    Background {
        seq: u32,
        lines: Vec<String>,
    },
}

impl TimelineItem {
    pub fn seq(&self) -> u32 {
        match self {
            TimelineItem::Phase { seq, .. }
            | TimelineItem::Message { seq, .. }
            | TimelineItem::Thinking { seq, .. }
            | TimelineItem::Tool { seq, .. }
            | TimelineItem::Problem { seq, .. }
            | TimelineItem::Finished { seq, .. }
            | TimelineItem::Background { seq, .. } => *seq,
        }
    }

    /// Whether the row has details the creator can expand.
    pub fn is_expandable(&self) -> bool {
        match self {
            TimelineItem::Tool { output, .. } => output.as_ref().is_some_and(|output| !output.summary.is_empty()),
            TimelineItem::Thinking { .. } | TimelineItem::Background { .. } => true,
            _ => false,
        }
    }
}

/// Folds a run's log into timeline rows: tool results attach to their call, and runs
/// of background lines become one row.
pub fn fold_timeline(entries: &[TimelineEntry]) -> Vec<TimelineItem> {
    let mut items: Vec<TimelineItem> = Vec::new();
    for (ix, entry) in entries.iter().enumerate() {
        let seq = entry.seq;
        let agent = match &entry.event {
            LogEvent::Phase(started) => {
                items.push(TimelineItem::Phase { seq, phase: started.phase, text: started.text.clone() });
                continue;
            }
            LogEvent::Agent(event) => event,
        };
        match agent {
            AgentEvent::Init { .. } => {}
            AgentEvent::AssistantText { text } => items.push(TimelineItem::Message { seq, text: text.clone() }),
            AgentEvent::Thinking { text } => items.push(TimelineItem::Thinking { seq, text: text.clone() }),
            AgentEvent::ToolUse { name, summary, .. } => {
                items.push(TimelineItem::Tool { seq, name: name.clone(), summary: summary.clone(), output: None })
            }
            AgentEvent::ToolResult { tool_use_id, is_error, summary } => {
                let output = ToolOutput { is_error: *is_error, summary: summary.clone() };
                match tool_call(&entries[..ix], &mut items, tool_use_id) {
                    Some(slot) => *slot = Some(output),
                    None => background(&mut items, seq, format!("tool result: {summary}")),
                }
            }
            AgentEvent::PermissionDenied { tool_name, message } => items.push(TimelineItem::Problem {
                seq,
                title: format!("{tool_name} was not allowed"),
                detail: message.clone(),
            }),
            AgentEvent::Result { is_error: true, result_text, .. } => items.push(TimelineItem::Problem {
                seq,
                title: "The agent reported an error".to_owned(),
                detail: result_text.clone(),
            }),
            AgentEvent::Result { cost_usd, duration_ms, .. } => {
                items.push(TimelineItem::Finished { seq, cost_usd: *cost_usd, duration_ms: *duration_ms })
            }
            AgentEvent::Stderr { line } => background(&mut items, seq, line.clone()),
            AgentEvent::Unknown { kind, .. } => background(&mut items, seq, kind.clone()),
        }
    }
    items
}

/// The output slot of the not yet answered call `tool_use_id` among the `earlier`
/// entries, searched back to the start of the current phase. Ids repeat across agent
/// processes, so only the latest call with the id counts.
fn tool_call<'a>(
    earlier: &[TimelineEntry],
    items: &'a mut [TimelineItem],
    tool_use_id: &str,
) -> Option<&'a mut Option<ToolOutput>> {
    let call_seq = earlier.iter().rev().find_map(|entry| match &entry.event {
        LogEvent::Agent(AgentEvent::ToolUse { id, .. }) if id == tool_use_id => Some(entry.seq),
        _ => None,
    })?;
    items.iter_mut().rev().take_while(|item| !matches!(item, TimelineItem::Phase { .. })).find_map(|item| match item {
        TimelineItem::Tool { seq, output, .. } if *seq == call_seq && output.is_none() => Some(output),
        _ => None,
    })
}

fn background(items: &mut Vec<TimelineItem>, seq: u32, line: String) {
    match items.last_mut() {
        Some(TimelineItem::Background { lines, .. }) => lines.push(line),
        _ => items.push(TimelineItem::Background { seq, lines: vec![line] }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(events: Vec<LogEvent>) -> Vec<TimelineEntry> {
        events
            .into_iter()
            .zip(1..)
            .map(|(event, seq)| TimelineEntry { seq, at: DateTime::<Utc>::MIN_UTC, event })
            .collect()
    }

    fn agent(event: AgentEvent) -> LogEvent {
        LogEvent::Agent(event)
    }

    fn tool_use(id: &str, name: &str, summary: &str) -> LogEvent {
        agent(AgentEvent::ToolUse { id: id.into(), name: name.into(), summary: summary.into() })
    }

    fn tool_result(id: &str, is_error: bool, summary: &str) -> LogEvent {
        agent(AgentEvent::ToolResult { tool_use_id: id.into(), is_error, summary: summary.into() })
    }

    fn unknown(kind: &str) -> LogEvent {
        agent(AgentEvent::Unknown { kind: kind.into(), excerpt: "{}".into() })
    }

    #[test]
    fn tool_results_attach_to_their_calls() {
        let log = entries(vec![
            LogEvent::Phase(PhaseStarted { phase: PhaseKind::Plan, text: None }),
            agent(AgentEvent::Init { session_id: "s".into(), model: "m".into(), cwd: "/".into(), tools: vec![] }),
            tool_use("t1", "Skill", "video-use"),
            tool_use("t2", "Bash", "ffprobe take1.mp4"),
            tool_result("t2", true, "No such file"),
            tool_result("t1", false, "Launching skill: video-use"),
            agent(AgentEvent::AssistantText { text: "**Strategy**".into() }),
        ]);
        assert_eq!(
            fold_timeline(&log),
            vec![
                TimelineItem::Phase { seq: 1, phase: PhaseKind::Plan, text: None },
                TimelineItem::Tool {
                    seq: 3,
                    name: "Skill".into(),
                    summary: "video-use".into(),
                    output: Some(ToolOutput { is_error: false, summary: "Launching skill: video-use".into() }),
                },
                TimelineItem::Tool {
                    seq: 4,
                    name: "Bash".into(),
                    summary: "ffprobe take1.mp4".into(),
                    output: Some(ToolOutput { is_error: true, summary: "No such file".into() }),
                },
                TimelineItem::Message { seq: 7, text: "**Strategy**".into() },
            ]
        );
    }

    #[test]
    fn background_lines_fold_into_one_row_per_run() {
        let log = entries(vec![
            unknown("system/task_started"),
            agent(AgentEvent::Stderr { line: "warning: slow disk".into() }),
            unknown("system/task_notification"),
            agent(AgentEvent::AssistantText { text: "Rendering.".into() }),
            unknown("system/task_started"),
            tool_result("missing", false, "orphan"),
        ]);
        let folded = fold_timeline(&log);
        assert_eq!(
            folded,
            vec![
                TimelineItem::Background {
                    seq: 1,
                    lines: vec![
                        "system/task_started".into(),
                        "warning: slow disk".into(),
                        "system/task_notification".into()
                    ],
                },
                TimelineItem::Message { seq: 4, text: "Rendering.".into() },
                TimelineItem::Background {
                    seq: 5,
                    lines: vec!["system/task_started".into(), "tool result: orphan".into()],
                },
            ]
        );
        assert!(folded[0].is_expandable() && !folded[1].is_expandable());
    }

    #[test]
    fn results_become_finish_rows_or_problems() {
        let result = |is_error: bool, text: &str| {
            agent(AgentEvent::Result {
                is_error,
                result_text: text.into(),
                session_id: "s".into(),
                cost_usd: Some(0.25),
                duration_ms: Some(61_000),
                num_turns: Some(4),
            })
        };
        let log = entries(vec![
            result(false, "Strategy"),
            agent(AgentEvent::PermissionDenied { tool_name: "WebFetch".into(), message: "denied".into() }),
            result(true, "Turn limit reached"),
        ]);
        assert_eq!(
            fold_timeline(&log),
            vec![
                TimelineItem::Finished { seq: 1, cost_usd: Some(0.25), duration_ms: Some(61_000) },
                TimelineItem::Problem { seq: 2, title: "WebFetch was not allowed".into(), detail: "denied".into() },
                TimelineItem::Problem {
                    seq: 3,
                    title: "The agent reported an error".into(),
                    detail: "Turn limit reached".into(),
                },
            ]
        );
    }

    #[test]
    fn tool_ids_reused_by_a_later_phase_stay_apart() {
        let log = entries(vec![
            tool_use("t1", "Read", "edit/project.md"),
            LogEvent::Phase(PhaseStarted { phase: PhaseKind::Approve, text: Some("Shorter intro".into()) }),
            tool_result("t1", false, "late"),
            tool_use("t1", "Write", "edit/edl.json"),
            tool_result("t1", false, "written"),
        ]);
        let folded = fold_timeline(&log);
        assert!(matches!(&folded[0], TimelineItem::Tool { output: None, .. }));
        assert_eq!(folded[2], TimelineItem::Background { seq: 3, lines: vec!["tool result: late".into()] });
        assert!(matches!(
            &folded[3],
            TimelineItem::Tool { seq: 4, output: Some(ToolOutput { summary, .. }), .. } if summary == "written"
        ));
    }

    #[test]
    fn log_events_round_trip_through_run_event_payloads() {
        let events = [
            LogEvent::Phase(PhaseStarted { phase: PhaseKind::Feedback, text: Some("Cut the outro".into()) }),
            LogEvent::Phase(PhaseStarted { phase: PhaseKind::Plan, text: None }),
            agent(AgentEvent::AssistantText { text: "Hi".into() }),
            unknown("system/task_started"),
        ];
        for (seq, event) in (1..).zip(events) {
            let payload = event.to_payload();
            let stored = RunEvent { run_id: videocrm_core::RunId::new(), seq, at: Utc::now(), payload };
            assert_eq!(TimelineEntry::from(stored).event, event);
        }
        let marker = LogEvent::Phase(PhaseStarted { phase: PhaseKind::Approve, text: None }).to_payload();
        assert_eq!(marker, serde_json::json!({ "type": "phase_started", "phase": "approve" }));

        let garbage = RunEvent {
            run_id: videocrm_core::RunId::new(),
            seq: 9,
            at: Utc::now(),
            payload: serde_json::json!({ "type": "from_the_future" }),
        };
        assert!(matches!(
            TimelineEntry::from(garbage).event,
            LogEvent::Agent(AgentEvent::Unknown { ref kind, .. }) if kind == "unreadable"
        ));
    }
}
