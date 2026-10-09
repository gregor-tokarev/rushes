//! AI edit runs end to end: `PipelineService` drives `scripts/fake-agent` on generated
//! footage, with the store, the video and the run timeline checked after each phase.
//! The UI integration tests at the end drive the same runs through the real panel,
//! pipelines page and settings in headless windows.

#![cfg(unix)]

use std::cell::RefCell;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, Instant};

use chrono::Utc;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AnyView, AnyWindowHandle, App, AppContext as _, Bounds, Context, ElementId, Entity, ExternalPaths, FileDropEvent,
    InteractiveElement as _, IntoElement, ParentElement as _, PlatformInput, Render, ScrollDelta, ScrollHandle,
    StatefulInteractiveElement as _, Styled as _, TestAppContext, Window, WindowBounds, WindowOptions, div, point, px,
    size,
};
use tempfile::TempDir;
use videocrm::ai_edit::{
    AiEditPanel, AiSettings, INTERRUPTED, LogEvent, PhaseKind, PhaseStarted, PipelineService, PipelinesPage, RunError,
    RunMarker, TimelineEntry, split_failure,
};
use videocrm::route::{Route, VideoTab};
use videocrm::state::{AppEvent, AppState};
use videocrm_core::{Block, DocumentKind, NewVideo, PipelineRun, RunId, RunStatus, Store, VideoId, VideoStatus};
use videocrm_pipeline::AgentEvent;

const TIMEOUT: Duration = Duration::from_secs(90);

/// The fake agent needs python3 and renders with ffmpeg.
fn tools_available() -> bool {
    let ok = ["python3", "ffmpeg", "ffprobe"].iter().all(|tool| Command::new(tool).arg("-version").output().is_ok());
    if !ok {
        eprintln!("skipping: python3, ffmpeg and ffprobe are required");
    }
    ok
}

struct Fixture {
    root: TempDir,
    state: Entity<AppState>,
    service: Entity<PipelineService>,
    video: VideoId,
    footage: PathBuf,
    pid_file: PathBuf,
}

impl Fixture {
    /// A video with one 3 s take, the fake agent and a fake video-use skill.
    fn new(cx: &mut TestAppContext, env: &[(&str, &str)]) -> Self {
        Self::with_store(cx, Store::open_in_memory().expect("store"), env, true)
    }

    fn with_store(cx: &mut TestAppContext, store: Store, env: &[(&str, &str)], footage: bool) -> Self {
        // Agents run on real threads and wake the foreground from there.
        cx.executor().allow_parking();
        cx.update(gpui_kit::init);

        let root = tempfile::tempdir().expect("tempdir");
        let base = fs::canonicalize(root.path()).expect("canonical tempdir");
        let skill = base.join("video-use");
        fs::create_dir_all(skill.join("helpers")).unwrap();
        fs::write(skill.join("SKILL.md"), "# video-use\n").unwrap();
        fs::write(skill.join(".env"), "ELEVENLABS_API_KEY=test-key\n").unwrap();
        let pid_file = base.join("pids");

        let state = cx.new(|_| AppState::new(store).expect("app state"));
        let video = cx.update(|cx| {
            AppState::set_global(state.clone(), cx);
            state.update(cx, |state, cx| {
                state.update_settings(cx, |settings| {
                    settings.media_root = Some(base.join("media"));
                    settings.pipeline.agent_command = fake_agent().display().to_string();
                    settings.pipeline.skill_dir = Some(skill.clone());
                });
                state
                    .create_video(NewVideo { title: "Desk setup 2026".into(), ..NewVideo::default() }, cx)
                    .expect("video created")
            })
        });
        let footage_dir = cx.read(|cx| {
            state.read(cx).video(video).and_then(|video| video.folder.clone()).expect("media folder").join("footage")
        });
        if footage {
            make_clip(&footage_dir.join("take1.mp4"));
        }

        let mut agent_env: Vec<(String, String)> =
            env.iter().map(|(key, value)| ((*key).to_owned(), (*value).to_owned())).collect();
        agent_env.push(("FAKE_AGENT_PID_FILE".into(), pid_file.display().to_string()));
        let service = cx.update(|cx| {
            PipelineService::init(state.clone(), cx);
            let service = PipelineService::global(cx);
            service.update(cx, |service, cx| service.set_agent_env(agent_env, cx));
            service
        });
        Self { root, state, service, video, footage: footage_dir, pid_file }
    }

    fn start(&self, cx: &mut TestAppContext, auto_approve: bool) -> RunId {
        cx.update(|cx| {
            self.service
                .update(cx, |service, cx| service.start_run(self.video, "Cut it tight.".into(), auto_approve, cx))
        })
        .expect("run started")
    }

    fn run(&self, cx: &mut TestAppContext, id: RunId) -> PipelineRun {
        cx.read(|cx| self.service.read(cx).run(id).cloned().expect("run"))
    }

    fn stored_run(&self, cx: &mut TestAppContext, id: RunId) -> PipelineRun {
        cx.read(|cx| self.state.read(cx).store().lock().unwrap().get_run(id).unwrap().expect("stored run"))
    }

    fn stored_events(&self, cx: &mut TestAppContext, id: RunId) -> Vec<TimelineEntry> {
        let events = cx.read(|cx| self.state.read(cx).store().lock().unwrap().list_run_events(id).unwrap());
        events.into_iter().map(TimelineEntry::from).collect()
    }

    fn wait_for_status(&self, cx: &mut TestAppContext, id: RunId, status: RunStatus) -> PipelineRun {
        let service = self.service.clone();
        wait_until(cx, &format!("run status {status:?}"), move |cx| {
            let run = service.read(cx).run(id);
            run.is_some_and(|run| run.status == status) && service.read(cx).live_phase(id).is_none()
        });
        let run = self.run(cx, id);
        assert_eq!(self.stored_run(cx, id), run, "the cached run is the stored run");
        run
    }

    fn video_status(&self, cx: &mut TestAppContext) -> VideoStatus {
        cx.read(|cx| self.state.read(cx).video(self.video).expect("video").status)
    }

    fn upload_file(&self, cx: &mut TestAppContext) -> Option<PathBuf> {
        cx.read(|cx| self.state.read(cx).video(self.video).expect("video").publish.file.clone())
    }

    /// Where the agent renders.
    fn final_cut(&self) -> PathBuf {
        self.footage.join("edit").join("final.mp4")
    }

    /// Where the `n`th render of the video is kept.
    fn cut(&self, n: u32) -> PathBuf {
        self.footage.join("edit").join("cuts").join(format!("final-{n}.mp4"))
    }
}

fn fake_agent() -> PathBuf {
    fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/fake-agent")).expect("fake agent")
}

/// An agent that reports success without doing anything; with `LAZY_IS_ERROR=1` it
/// reports an error result (like a turn limit) but still exits normally.
fn lazy_agent(dir: &Path) -> PathBuf {
    let path = dir.join("lazy-agent");
    fs::write(
        &path,
        r#"#!/bin/sh
echo '{"type":"system","subtype":"init","session_id":"lazy-session","cwd":"/","tools":[],"model":"lazy"}'
if [ "$LAZY_IS_ERROR" = 1 ]; then
  echo '{"type":"result","subtype":"error_max_turns","is_error":true,"result":"Reached the turn limit.","session_id":"lazy-session","total_cost_usd":0.01}'
else
  echo '{"type":"result","subtype":"success","is_error":false,"result":"All done.","session_id":"lazy-session","total_cost_usd":0.01}'
fi
"#,
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// An agent with a real run's log shape: a few dozen tool calls, then a final summary
/// far taller than the activity box, then the result. It renders nothing.
fn chatty_agent(dir: &Path) -> PathBuf {
    let path = dir.join("chatty-agent");
    fs::write(
        &path,
        r#"#!/usr/bin/env python3
import json, sys
def out(event):
    event.setdefault("session_id", "chatty-session")
    print(json.dumps(event), flush=True)
def assistant(block):
    out({"type": "assistant", "message": {"role": "assistant", "content": [block]}})
out({"type": "system", "subtype": "init", "cwd": "/", "tools": ["Bash"], "model": "chatty"})
for ix in range(30):
    assistant({"type": "tool_use", "id": f"t{ix}", "name": "Bash", "input": {"command": f"ffprobe take{ix}.mp4"}})
    out({"type": "user", "message": {"role": "user", "content": [
        {"type": "tool_result", "tool_use_id": f"t{ix}", "content": "ok", "is_error": False}]}})
point = "**Audio sync:** the sound drifted until it was 0.1s behind, so I rebuilt it from the `edit/` segments. "
summary = "The final cut is rendered: **`edit/final.mp4`**, 14.6s.\n\n" + "\n\n".join(
    f"**Section {n}:**\n" + "\n".join(f"- {point * 3}" for _ in range(4)) for n in range(4))
assistant({"type": "text", "text": summary})
out({"type": "result", "subtype": "success", "is_error": False, "result": summary, "total_cost_usd": 0.5})
"#,
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A 3 second 320x240 test pattern with a sine tone.
fn make_clip(path: &Path) {
    let status = Command::new("ffmpeg")
        .args(["-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", "testsrc=duration=3:size=320x240:rate=25"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=3"])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"])
        .args(["-c:a", "aac", "-shortest"])
        .arg(path)
        .status()
        .expect("run ffmpeg");
    assert!(status.success(), "ffmpeg failed for {}", path.display());
}

/// Drives the foreground until `done` holds, letting the agent threads work in real time.
fn wait_until(cx: &mut TestAppContext, what: &str, mut done: impl FnMut(&App) -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        cx.run_until_parked();
        if cx.read(|cx| done(cx)) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn phases(events: &[TimelineEntry]) -> Vec<(PhaseKind, Option<String>)> {
    events
        .iter()
        .filter_map(|entry| match &entry.event {
            LogEvent::Phase(started) => Some((started.phase, started.text.clone())),
            LogEvent::Agent(_) | LogEvent::Marker(_) => None,
        })
        .collect()
}

/// The markers in a run's log, with whether each phase was a retry.
fn markers(events: &[TimelineEntry]) -> Vec<String> {
    events
        .iter()
        .filter_map(|entry| match &entry.event {
            LogEvent::Phase(started) if started.retry => Some(format!("retry {:?}", started.phase)),
            LogEvent::Phase(started) => Some(format!("{:?}", started.phase)),
            LogEvent::Marker(RunMarker { kind }) => Some(format!("{kind:?}")),
            LogEvent::Agent(_) => None,
        })
        .collect()
}

fn tool_names(events: &[TimelineEntry]) -> Vec<String> {
    events
        .iter()
        .filter_map(|entry| match &entry.event {
            LogEvent::Agent(AgentEvent::ToolUse { name, .. }) => Some(name.clone()),
            _ => None,
        })
        .collect()
}

/// Whether `pid` is gone (or a zombie waiting to be reaped).
fn process_gone(pid: u32) -> bool {
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => true,
        Ok(stat) => stat.rsplit_once(')').is_some_and(|(_, rest)| rest.trim_start().starts_with('Z')),
    }
}

fn recorded_pids(file: &Path) -> Vec<u32> {
    fs::read_to_string(file).unwrap_or_default().lines().filter_map(|line| line.trim().parse().ok()).collect()
}

#[gpui_kit::test]
fn plan_revise_approve_renders_and_prepares_the_upload(cx: &mut TestAppContext) {
    if !tools_available() {
        return;
    }
    let fx = Fixture::new(cx, &[]);
    assert_eq!(fx.video_status(cx), VideoStatus::Idea);

    let id = fx.start(cx, false);
    assert_eq!(fx.run(cx, id).status, RunStatus::Planning);
    assert_eq!(fx.video_status(cx), VideoStatus::Editing, "starting an edit moves the video to Editing");

    let planned = fx.wait_for_status(cx, id, RunStatus::AwaitingApproval);
    let strategy = planned.summary.clone().expect("strategy");
    assert!(strategy.contains("**Strategy**"), "{strategy}");
    let session = planned.session_id.clone().expect("session id");
    assert!((planned.cost_usd.expect("cost") - 0.0123).abs() < 1e-9);
    assert_eq!(planned.output, None);
    assert_eq!(planned.brief, "Cut it tight.");
    assert_eq!(planned.footage_dir, fx.footage);

    let events = fx.stored_events(cx, id);
    assert_eq!(phases(&events), [(PhaseKind::Plan, None)]);
    assert!(matches!(events[1].event, LogEvent::Agent(AgentEvent::Init { .. })), "{:?}", events[1]);
    assert_eq!(tool_names(&events), ["Skill", "Bash", "Bash", "Bash", "Write"]);
    assert!(matches!(events.last().unwrap().event, LogEvent::Agent(AgentEvent::Result { is_error: false, .. })));
    assert_eq!(cx.read(|cx| fx.service.read(cx).timeline(id).to_vec()), events, "live timeline matches the store");
    assert!(!fx.final_cut().exists(), "planning renders nothing");

    let empty = cx.update(|cx| fx.service.update(cx, |service, cx| service.send_feedback(id, "  ".into(), cx)));
    assert_eq!(empty, Err(RunError::EmptyFeedback));

    // Feedback on the strategy asks for a revised strategy, in the same session.
    cx.update(|cx| fx.service.update(cx, |service, cx| service.send_feedback(id, "Add an end card.".into(), cx)))
        .expect("feedback sent");
    assert_eq!(fx.run(cx, id).status, RunStatus::Planning);
    let revised = fx.wait_for_status(cx, id, RunStatus::AwaitingApproval);
    assert_eq!(revised.session_id.as_deref(), Some(session.as_str()));
    assert!(!fx.final_cut().exists(), "a revised strategy renders nothing");

    cx.update(|cx| fx.service.update(cx, |service, cx| service.approve(id, Some("  Keep the intro.  ".into()), cx)))
        .expect("approved");
    assert_eq!(fx.run(cx, id).status, RunStatus::Rendering);
    let done = fx.wait_for_status(cx, id, RunStatus::Succeeded);
    assert_eq!(done.output, Some(fx.cut(1)), "the render is kept as the edit's own file");
    assert!(fx.cut(1).is_file());
    assert!(!fx.final_cut().exists(), "nothing is left where the next render writes");
    assert_eq!(done.session_id.as_deref(), Some(session.as_str()), "phases resume one session");
    assert!((done.cost_usd.unwrap() - 3.0 * 0.0123).abs() < 1e-9, "cost adds up over phases");
    assert!(done.summary.unwrap().contains("Rendered edit/final.mp4"));
    assert_eq!(fx.upload_file(cx), Some(fx.cut(1)), "the final cut becomes the upload file");
    assert_eq!(fx.video_status(cx), VideoStatus::Editing);

    let events = fx.stored_events(cx, id);
    assert_eq!(
        phases(&events),
        [
            (PhaseKind::Plan, None),
            (PhaseKind::Revise, Some("Add an end card.".into())),
            (PhaseKind::Approve, Some("Keep the intro.".into())),
        ]
    );
    let seqs: Vec<u32> = events.iter().map(|entry| entry.seq).collect();
    assert_eq!(seqs, (1..=events.len() as u32).collect::<Vec<_>>());
    let row = cx.read(|cx| fx.state.read(cx).row(fx.video).cloned().unwrap());
    assert_eq!(row.latest_run, Some(RunStatus::Succeeded), "rows reload with the run state");
}

#[gpui_kit::test]
fn auto_approve_renders_and_feedback_renders_again(cx: &mut TestAppContext) {
    if !tools_available() {
        return;
    }
    let fx = Fixture::new(cx, &[]);
    let id = fx.start(cx, true);
    assert_eq!(fx.run(cx, id).status, RunStatus::Rendering);
    let first = fx.wait_for_status(cx, id, RunStatus::Succeeded);
    assert_eq!(first.output, Some(fx.cut(1)));
    assert_eq!(phases(&fx.stored_events(cx, id)), [(PhaseKind::Auto, None)]);

    // An upload file chosen by hand is kept.
    let chosen = fx.root.path().join("my-cut.mp4");
    cx.update(|cx| {
        fx.state.update(cx, |state, cx| {
            let mut video = state.video(fx.video).unwrap().clone();
            video.publish.file = Some(chosen.clone());
            state.update_video(video, cx);
        })
    });
    let rendered_at = fs::metadata(fx.cut(1)).unwrap().modified().unwrap();
    std::thread::sleep(Duration::from_millis(20));

    cx.update(|cx| fx.service.update(cx, |service, cx| service.send_feedback(id, "Shorter outro.".into(), cx)))
        .expect("feedback sent");
    assert_eq!(fx.run(cx, id).status, RunStatus::Rendering);
    let again = fx.wait_for_status(cx, id, RunStatus::Succeeded);
    assert_eq!(again.output, Some(fx.cut(2)), "the new render is a new file");
    assert_eq!(fs::metadata(fx.cut(1)).unwrap().modified().unwrap(), rendered_at, "the first render is kept");
    assert!(again.summary.unwrap().contains("feedback applied"));
    assert_eq!(fx.upload_file(cx), Some(chosen));
    assert_eq!(
        phases(&fx.stored_events(cx, id)),
        [(PhaseKind::Auto, None), (PhaseKind::Feedback, Some("Shorter outro.".into()))]
    );
}

#[gpui_kit::test]
fn a_new_edit_keeps_the_cut_of_the_earlier_one(cx: &mut TestAppContext) {
    if !tools_available() {
        return;
    }
    let fx = Fixture::new(cx, &[]);
    let first = fx.start(cx, true);
    fx.wait_for_status(cx, first, RunStatus::Succeeded);
    let first_cut = fs::read(fx.cut(1)).unwrap();

    // A second edit of the same footage, cut differently.
    make_clip(&fx.footage.join("take2.mp4"));
    let second = fx.start(cx, true);
    let done = fx.wait_for_status(cx, second, RunStatus::Succeeded);
    assert_eq!(done.output, Some(fx.cut(2)));
    assert_eq!(fx.run(cx, first).output, Some(fx.cut(1)), "the earlier edit still has its cut");
    assert_eq!(fs::read(fx.cut(1)).unwrap(), first_cut, "and the file is untouched");
    assert_ne!(fs::read(fx.cut(2)).unwrap(), first_cut);
    assert_eq!(fx.upload_file(cx), Some(fx.cut(1)), "the upload keeps the cut it was given");
}

#[gpui_kit::test]
fn a_cut_left_at_the_shared_path_is_kept_before_the_next_render(cx: &mut TestAppContext) {
    if !tools_available() {
        return;
    }
    // A store as an earlier version left it: the edit's output and the upload file
    // are the shared edit/final.mp4.
    let fx = Fixture::new(cx, &[]);
    fs::create_dir_all(fx.final_cut().parent().unwrap()).unwrap();
    fs::copy(fx.footage.join("take1.mp4"), fx.final_cut()).unwrap();
    let old_cut = fs::read(fx.final_cut()).unwrap();
    let now = Utc::now();
    let old = PipelineRun {
        id: RunId::new(),
        video_id: fx.video,
        status: RunStatus::Succeeded,
        brief: String::new(),
        footage_dir: fx.footage.clone(),
        session_id: Some("old".into()),
        summary: Some("Rendered.".into()),
        output: Some(fx.final_cut()),
        error: None,
        cost_usd: None,
        created_at: now,
        updated_at: now,
    };
    cx.read(|cx| fx.state.read(cx).store().lock().unwrap().insert_run(&old).unwrap());
    cx.update(|cx| {
        fx.state.update(cx, |state, cx| {
            let mut video = state.video(fx.video).unwrap().clone();
            video.publish.file = Some(fx.final_cut());
            state.update_video(video, cx);
        });
        PipelineService::init(fx.state.clone(), cx);
    });
    let service = cx.read(PipelineService::global);
    let id = cx
        .update(|cx| service.update(cx, |service, cx| service.start_run(fx.video, "Again.".into(), true, cx)))
        .expect("run started");
    assert_eq!(cx.read(|cx| service.read(cx).run(old.id).cloned().unwrap().output), Some(fx.cut(1)));
    assert_eq!(fx.upload_file(cx), Some(fx.cut(1)), "the upload form follows the kept cut");
    assert_eq!(fs::read(fx.cut(1)).unwrap(), old_cut);

    wait_until(cx, "the new render", |cx| {
        service.read(cx).run(id).is_some_and(|run| run.status == RunStatus::Succeeded)
    });
    assert_eq!(cx.read(|cx| service.read(cx).run(id).cloned().unwrap().output), Some(fx.cut(2)));
    assert_eq!(fs::read(fx.cut(1)).unwrap(), old_cut, "the render did not overwrite it");
}

#[gpui_kit::test]
fn cancel_stops_the_agent_and_its_children(cx: &mut TestAppContext) {
    if !tools_available() {
        return;
    }
    let fx = Fixture::new(cx, &[("FAKE_AGENT_DELAY_MS", "1500")]);
    let id = fx.start(cx, false);
    let pid_file = fx.pid_file.clone();
    // The agent and the `sleep` it waits in between events.
    wait_until(cx, "agent and child pids", |_| recorded_pids(&pid_file).len() >= 2);
    let pids = recorded_pids(&fx.pid_file);
    assert!(pids.iter().any(|pid| !process_gone(*pid)), "the agent is running");

    cx.update(|cx| fx.service.update(cx, |service, cx| service.cancel(id, cx)));
    let live = cx.read(|cx| fx.service.read(cx).live_phase(id)).expect("still stopping");
    assert!(live.cancelling);
    let cancelled = fx.wait_for_status(cx, id, RunStatus::Cancelled);
    assert_eq!(cancelled.error, None);
    assert_eq!(markers(&fx.stored_events(cx, id)), ["Plan", "Cancelled"], "the log says where it stopped");

    let deadline = Instant::now() + Duration::from_secs(10);
    while !pids.iter().all(|pid| process_gone(*pid)) {
        assert!(Instant::now() < deadline, "agent processes still running: {pids:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!fx.footage.join("edit").join("project.md").exists(), "the plan never finished");
}

#[gpui_kit::test]
fn a_second_edit_of_the_same_video_is_refused_while_one_runs(cx: &mut TestAppContext) {
    if !tools_available() {
        return;
    }
    let fx = Fixture::new(cx, &[("FAKE_AGENT_DELAY_MS", "500")]);
    let id = fx.start(cx, false);
    let second =
        cx.update(|cx| fx.service.update(cx, |service, cx| service.start_run(fx.video, "Again".into(), false, cx)));
    assert_eq!(second, Err(RunError::AlreadyRunning));
    assert!(second.unwrap_err().to_string().contains("already running"));
    assert_eq!(cx.read(|cx| fx.service.read(cx).video_runs(fx.video).count()), 1);
    assert_eq!(cx.read(|cx| fx.service.read(cx).live_run_of(fx.video)), Some(id));

    cx.update(|cx| fx.service.update(cx, |service, cx| service.cancel(id, cx)));
    fx.wait_for_status(cx, id, RunStatus::Cancelled);
}

#[gpui_kit::test]
fn a_video_without_footage_cannot_be_edited(cx: &mut TestAppContext) {
    let fx = Fixture::with_store(cx, Store::open_in_memory().unwrap(), &[], false);
    let result =
        cx.update(|cx| fx.service.update(cx, |service, cx| service.start_run(fx.video, String::new(), false, cx)));
    assert_eq!(result, Err(RunError::NoFootage(fx.footage.clone())));
    assert!(cx.read(|cx| fx.service.read(cx).runs().is_empty()));
    assert_eq!(fx.video_status(cx), VideoStatus::Idea);
}

#[gpui_kit::test]
fn a_failing_agent_fails_the_run_with_its_stderr(cx: &mut TestAppContext) {
    if !tools_available() {
        return;
    }
    let fx = Fixture::new(cx, &[("FAKE_AGENT_MODE", "fail")]);
    let id = fx.start(cx, false);
    let failed = fx.wait_for_status(cx, id, RunStatus::Failed);
    let error = failed.error.expect("error");
    let (message, stderr) = split_failure(&error);
    assert!(message.contains("simulated failure"), "{message}");
    assert!(stderr.expect("stderr tail").contains("ELEVENLABS_API_KEY rejected (401)"), "{error}");
    assert!(failed.session_id.is_some(), "the session can be resumed");

    cx.update(|cx| fx.service.update(cx, |service, cx| service.retry(id, cx))).expect("retried");
    assert_eq!(fx.run(cx, id).status, RunStatus::Planning);
    assert_eq!(fx.run(cx, id).error, None);
    fx.wait_for_status(cx, id, RunStatus::Failed);
    assert_eq!(markers(&fx.stored_events(cx, id)), ["Plan", "retry Plan"]);
}

#[gpui_kit::test]
fn an_agent_command_that_is_not_claude_code_is_named_as_the_cause(cx: &mut TestAppContext) {
    if !tools_available() {
        return;
    }
    let fx = Fixture::new(cx, &[]);
    let silent = fx.root.path().join("not-claude");
    fs::write(&silent, "#!/bin/sh\necho 'true (GNU coreutils) 9.7'\n").unwrap();
    fs::set_permissions(&silent, fs::Permissions::from_mode(0o755)).unwrap();
    cx.update(|cx| {
        fx.state.update(cx, |state, cx| {
            state.update_settings(cx, |settings| settings.pipeline.agent_command = silent.display().to_string())
        })
    });
    let service = fx.service.clone();
    wait_until(cx, "the requirement check", |cx| !service.read(cx).is_checking());
    let doctor = cx.read(|cx| service.read(cx).doctor().cloned()).expect("doctor");
    assert!(!doctor.agent.ok && doctor.agent.detail.contains("is not Claude Code"), "{:?}", doctor.agent);

    let id = fx.start(cx, false);
    let failed = fx.wait_for_status(cx, id, RunStatus::Failed);
    let error = failed.error.expect("error");
    assert!(error.starts_with("The agent stopped without reporting a result"), "{error}");
    assert!(error.ends_with("Check the agent command in Settings > AI editing."), "{error}");
}

#[gpui_kit::test]
fn startup_marks_interrupted_runs_failed_and_they_can_continue(cx: &mut TestAppContext) {
    if !tools_available() {
        return;
    }
    // A store as a previous session left it: one video, runs in every state.
    let mut store = Store::open_in_memory().unwrap();
    let video = videocrm_core::Video::new("Old session");
    store.insert_video(&video).unwrap();
    let run = |status: RunStatus, session: Option<&str>| PipelineRun {
        id: RunId::new(),
        video_id: video.id,
        status,
        brief: String::new(),
        footage_dir: PathBuf::from("/nonexistent"),
        session_id: session.map(str::to_owned),
        summary: None,
        output: None,
        error: None,
        cost_usd: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let runs = [
        run(RunStatus::Queued, None),
        run(RunStatus::Planning, Some("s-plan")),
        run(RunStatus::Rendering, Some("s-render")),
        run(RunStatus::AwaitingApproval, Some("s-wait")),
        run(RunStatus::Succeeded, Some("s-done")),
    ];
    for run in &runs {
        store.insert_run(run).unwrap();
    }

    let fx = Fixture::with_store(cx, store, &[], true);
    let status = |cx: &mut TestAppContext, id| fx.stored_run(cx, id);
    for run in &runs[..3] {
        let stored = status(cx, run.id);
        assert_eq!((stored.status, stored.error.as_deref()), (RunStatus::Failed, Some(INTERRUPTED)));
        assert_eq!(fx.run(cx, run.id).status, RunStatus::Failed);
        assert_eq!(markers(&fx.stored_events(cx, run.id)), ["Interrupted"]);
    }
    assert_eq!(status(cx, runs[3].id).status, RunStatus::AwaitingApproval);
    assert_eq!(status(cx, runs[4].id).status, RunStatus::Succeeded);

    // The interrupted render continues in its session, on the fixture's footage.
    let interrupted = runs[2].id;
    let mut moved = fx.stored_run(cx, interrupted);
    moved.footage_dir = fx.footage.clone();
    cx.read(|cx| fx.state.read(cx).store().lock().unwrap().update_run(&mut moved).unwrap());
    cx.update(|cx| {
        PipelineService::init(fx.state.clone(), cx);
        let service = PipelineService::global(cx);
        service.update(cx, |service, cx| {
            service.send_feedback(interrupted, "Finish the render.".into(), cx).expect("continued")
        });
    });
    let service = cx.read(PipelineService::global);
    wait_until(cx, "continued render", |cx| {
        service.read(cx).run(interrupted).is_some_and(|run| run.status == RunStatus::Succeeded)
    });
    let continued = cx.read(|cx| service.read(cx).run(interrupted).cloned().unwrap());
    assert_eq!(continued.session_id.as_deref(), Some("s-render"));
    assert_eq!(continued.output, Some(fx.cut(1)));
}

#[gpui_kit::test]
fn video_rows_show_interrupted_runs_as_failed_from_the_start(cx: &mut TestAppContext) {
    let mut store = Store::open_in_memory().unwrap();
    let video = videocrm_core::Video::new("Quit while planning");
    store.insert_video(&video).unwrap();
    let now = Utc::now();
    let run = PipelineRun {
        id: RunId::new(),
        video_id: video.id,
        status: RunStatus::Planning,
        brief: String::new(),
        footage_dir: PathBuf::from("/nonexistent"),
        session_id: Some("s".into()),
        summary: None,
        output: None,
        error: None,
        cost_usd: None,
        created_at: now,
        updated_at: now,
    };
    store.insert_run(&run).unwrap();
    // The rows are read before the service starts, as at app startup.
    let state = cx.new(|_| AppState::new(store).expect("app state"));
    assert_eq!(cx.read(|cx| state.read(cx).row(video.id).unwrap().latest_run), Some(RunStatus::Planning));

    cx.update(|cx| {
        AppState::set_global(state.clone(), cx);
        PipelineService::init(state.clone(), cx);
    });
    let row = cx.read(|cx| state.read(cx).row(video.id).cloned().unwrap());
    assert_eq!(row.latest_run, Some(RunStatus::Failed), "no edit shows as running after a restart");
}

#[gpui_kit::test]
fn the_requirement_check_passes_with_the_fake_agent(cx: &mut TestAppContext) {
    let fx = Fixture::with_store(cx, Store::open_in_memory().unwrap(), &[], false);
    let service = fx.service.clone();
    wait_until(cx, "requirement check", |cx| !service.read(cx).is_checking());
    let doctor = cx.read(|cx| service.read(cx).doctor().cloned()).expect("doctor result");
    assert!(doctor.agent.ok, "{:?}", doctor.agent);
    assert!(doctor.skill.ok, "{:?}", doctor.skill);
    assert!(doctor.elevenlabs.ok, "{:?}", doctor.elevenlabs);
    if tools_available() {
        assert!(doctor.ready(), "{doctor:?}");
    }

    // Pointing the settings at a missing agent re-runs the check.
    cx.update(|cx| {
        fx.state.update(cx, |state, cx| {
            state.update_settings(cx, |settings| settings.pipeline.agent_command = "/nonexistent/claude".into())
        })
    });
    assert!(cx.read(|cx| service.read(cx).is_checking()));
    wait_until(cx, "second check", |cx| !service.read(cx).is_checking());
    let doctor = cx.read(|cx| service.read(cx).doctor().cloned()).unwrap();
    assert!(!doctor.agent.ok && !doctor.ready());
}

#[gpui_kit::test]
fn an_edit_that_failed_while_planning_plans_again_with_feedback(cx: &mut TestAppContext) {
    if !tools_available() {
        return;
    }
    let fx = Fixture::new(cx, &[("FAKE_AGENT_MODE", "fail")]);
    let id = fx.start(cx, false);
    fx.wait_for_status(cx, id, RunStatus::Failed);

    // The agent works again; the creator continues the failed plan with feedback.
    let pid_file = fx.pid_file.display().to_string();
    cx.update(|cx| {
        fx.service.update(cx, |service, cx| {
            service.set_agent_env(vec![("FAKE_AGENT_PID_FILE".into(), pid_file)], cx);
            service.send_feedback(id, "Use the second take for the intro.".into(), cx)
        })
    })
    .expect("continued");
    let planned = fx.wait_for_status(cx, id, RunStatus::AwaitingApproval);
    assert!(planned.summary.expect("strategy").contains("**Strategy**"));
    assert!(!fx.final_cut().exists(), "nothing is rendered before the strategy is approved");
    assert_eq!(
        phases(&fx.stored_events(cx, id)),
        [(PhaseKind::Plan, None), (PhaseKind::Revise, Some("Use the second take for the intro.".into()))]
    );
}

#[gpui_kit::test]
fn a_render_that_leaves_an_old_final_cut_fails(cx: &mut TestAppContext) {
    if !tools_available() {
        return;
    }
    let fx = Fixture::new(cx, &[]);
    // A final cut from an earlier edit of the same footage.
    fs::create_dir_all(fx.final_cut().parent().unwrap()).unwrap();
    fs::copy(fx.footage.join("take1.mp4"), fx.final_cut()).unwrap();
    let lazy = lazy_agent(fx.root.path());
    cx.update(|cx| {
        fx.state.update(cx, |state, cx| {
            state.update_settings(cx, |settings| settings.pipeline.agent_command = lazy.display().to_string())
        })
    });

    let id = fx.start(cx, true);
    let failed = fx.wait_for_status(cx, id, RunStatus::Failed);
    assert!(failed.error.as_deref().is_some_and(|error| error.contains("without rendering")), "{failed:?}");
    assert_eq!(failed.output, None);
    assert_eq!(fx.upload_file(cx), None, "the old cut is not offered for upload");
}

#[gpui_kit::test]
fn an_error_result_keeps_the_proposed_strategy(cx: &mut TestAppContext) {
    if !tools_available() {
        return;
    }
    let fx = Fixture::new(cx, &[]);
    let id = fx.start(cx, false);
    let strategy = fx.wait_for_status(cx, id, RunStatus::AwaitingApproval).summary.expect("strategy");

    let lazy = lazy_agent(fx.root.path());
    cx.update(|cx| {
        fx.state.update(cx, |state, cx| {
            state.update_settings(cx, |settings| settings.pipeline.agent_command = lazy.display().to_string())
        });
        fx.service.update(cx, |service, cx| service.set_agent_env(vec![("LAZY_IS_ERROR".into(), "1".into())], cx));
        fx.service.update(cx, |service, cx| service.send_feedback(id, "Shorter.".into(), cx))
    })
    .expect("feedback sent");
    let failed = fx.wait_for_status(cx, id, RunStatus::Failed);
    assert_eq!(failed.error.as_deref(), Some("Reached the turn limit."));
    assert_eq!(failed.summary, Some(strategy), "the error does not replace the strategy");
}

#[gpui_kit::test]
fn deleting_a_video_stops_its_edit_quietly(cx: &mut TestAppContext) {
    if !tools_available() {
        return;
    }
    let fx = Fixture::new(cx, &[("FAKE_AGENT_DELAY_MS", "400")]);
    let errors = Rc::new(RefCell::new(Vec::new()));
    let _errors = cx.update(|cx| {
        let errors = errors.clone();
        cx.subscribe(&fx.state, move |_, event: &AppEvent, _| {
            if let AppEvent::Error(message) = event {
                errors.borrow_mut().push(message.to_string());
            }
        })
    });
    let id = fx.start(cx, false);
    let pid_file = fx.pid_file.clone();
    wait_until(cx, "the agent", |_| !recorded_pids(&pid_file).is_empty());

    cx.update(|cx| fx.state.update(cx, |state, cx| state.delete_videos(&[fx.video], cx)));
    assert!(cx.read(|cx| fx.service.read(cx).runs().is_empty()), "the store deleted the run with its video");
    let service = fx.service.clone();
    wait_until(cx, "the agent to stop", |cx| service.read(cx).live_phase(id).is_none());
    let pids = recorded_pids(&fx.pid_file);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !pids.iter().all(|pid| process_gone(*pid)) {
        assert!(Instant::now() < deadline, "agent processes still running: {pids:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(*errors.borrow(), Vec::<String>::new());
}

// UI integration tests

/// Opens `build`'s view in a headless window tall enough for the whole panel.
fn open<V: Render>(
    cx: &mut TestAppContext,
    build: impl FnOnce(&mut Window, &mut Context<V>) -> V + 'static,
) -> AnyWindowHandle {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(Default::default(), size(px(1280.), px(2400.))))),
        ..WindowOptions::default()
    };
    let (handle, _) = cx
        .update(|cx| gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| build(window, cx))))
        .expect("open test window");
    handle
}

/// Shows one view that a test can close or replace, as navigating does.
struct Host {
    view: Option<AnyView>,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().children(self.view.clone())
    }
}

fn open_host(
    cx: &mut TestAppContext,
    build: impl FnOnce(&mut Window, &mut App) -> AnyView + 'static,
) -> (AnyWindowHandle, Entity<Host>) {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(Default::default(), size(px(1280.), px(2400.))))),
        ..WindowOptions::default()
    };
    cx.update(|cx| {
        gpui_kit::open_window(options, cx, |window, cx| {
            let view = build(window, cx);
            cx.new(|_| Host { view: Some(view) })
        })
    })
    .expect("open test window")
}

/// Closes the hosted view, as navigating away does.
fn close(cx: &mut TestAppContext, host: &Entity<Host>) {
    cx.update(|cx| {
        host.update(cx, |host, cx| {
            host.view = None;
            cx.notify();
        })
    });
    cx.run_until_parked();
}

/// Hosts a new view, as navigating to a page does.
fn show(
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    host: &Entity<Host>,
    build: impl FnOnce(&mut Window, &mut App) -> AnyView,
) {
    cx.update_window(window, |_, window, cx| {
        let view = build(window, cx);
        host.update(cx, |host, cx| {
            host.view = Some(view);
            cx.notify();
        });
    })
    .expect("window");
    cx.run_until_parked();
}

/// Like [`wait_until`], re-rendering the window and asking it each time.
fn wait_for_window(
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    what: &str,
    mut done: impl FnMut(&mut Window, &mut App) -> bool,
) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        cx.run_until_parked();
        let ready = cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                done(window, cx)
            })
            .expect("window");
        if ready {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn click(cx: &mut TestAppContext, window: AnyWindowHandle, id: impl Into<ElementId>) {
    let id = id.into();
    cx.update_window(window, |_, window, cx| window.click(id, cx)).expect("window");
}

fn shows(window: &Window, id: &str) -> bool {
    window.try_find(ElementId::Name(id.to_owned().into())).is_some_and(|element| element.visible())
}

fn latest_run(cx: &mut TestAppContext, fx: &Fixture) -> Option<RunId> {
    cx.read(|cx| fx.service.read(cx).video_runs(fx.video).next().map(|run| run.id))
}

fn route(cx: &mut TestAppContext, fx: &Fixture) -> Route {
    cx.read(|cx| fx.state.read(cx).route().clone())
}

#[gpui_kit::test]
fn the_panel_takes_an_edit_from_brief_to_publish(cx: &mut TestAppContext) {
    if !tools_available() {
        return;
    }
    let fx = Fixture::new(cx, &[]);
    let video = fx.video;
    let window = open(cx, move |window, cx| AiEditPanel::new(video, window, cx));
    let service = fx.service.clone();
    wait_until(cx, "requirement check", |cx| service.read(cx).doctor().is_some_and(|doctor| doctor.ready()));
    wait_for_window(cx, window, "the composer", |window, _| shows(window, "ai-edit-start"));
    assert!(latest_run(cx, &fx).is_none());

    click(cx, window, "brief-subtitles");
    click(cx, window, "ai-edit-start");
    let id = latest_run(cx, &fx).expect("the panel started a run");
    let planned = fx.wait_for_status(cx, id, RunStatus::AwaitingApproval);
    assert!(planned.brief.contains("- Burn subtitles into the render."), "{}", planned.brief);
    assert!(planned.brief.starts_with("Edit \"Desk setup 2026\""), "{}", planned.brief);

    wait_for_window(cx, window, "the strategy card", |window, _| shows(window, "run-approve"));
    let composing = cx.update_window(window, |_, window, _| shows(window, "ai-edit-start")).unwrap();
    assert!(!composing, "the brief form closes once the edit starts");
    click(cx, window, "run-approve");
    fx.wait_for_status(cx, id, RunStatus::Succeeded);

    wait_for_window(cx, window, "the output card", |window, _| shows(window, "run-use-for-upload"));
    click(cx, window, "run-use-for-upload");
    assert_eq!(route(cx, &fx), Route::Video { id: video, tab: VideoTab::Publish });
    assert_eq!(fx.upload_file(cx), Some(fx.cut(1)));
}

#[gpui_kit::test]
fn the_edit_cannot_start_without_footage(cx: &mut TestAppContext) {
    let fx = Fixture::with_store(cx, Store::open_in_memory().unwrap(), &[], false);
    let video = fx.video;
    let window = open(cx, move |window, cx| AiEditPanel::new(video, window, cx));
    let service = fx.service.clone();
    wait_until(cx, "requirement check", |cx| service.read(cx).doctor().is_some());
    wait_for_window(cx, window, "the composer", |window, _| shows(window, "ai-edit-start"));
    click(cx, window, "ai-edit-start");
    cx.run_until_parked();
    assert!(latest_run(cx, &fx).is_none(), "Start AI edit is disabled without footage");
}

#[gpui_kit::test]
fn requested_changes_revise_the_strategy(cx: &mut TestAppContext) {
    if !tools_available() {
        return;
    }
    let fx = Fixture::new(cx, &[]);
    let id = fx.start(cx, false);
    let video = fx.video;
    let window = open(cx, move |window, cx| AiEditPanel::new(video, window, cx));
    fx.wait_for_status(cx, id, RunStatus::AwaitingApproval);
    wait_for_window(cx, window, "the strategy card", |window, _| shows(window, "run-request-changes"));

    click(cx, window, "run-request-changes");
    cx.update_window(window, |_, window, cx| window.input("Add an end card.", cx)).unwrap();
    click(cx, window, "run-send-feedback");
    assert_eq!(fx.run(cx, id).status, RunStatus::Planning);
    fx.wait_for_status(cx, id, RunStatus::AwaitingApproval);
    assert_eq!(
        phases(&fx.stored_events(cx, id)),
        [(PhaseKind::Plan, None), (PhaseKind::Revise, Some("Add an end card.".into()))]
    );
}

#[gpui_kit::test]
fn a_discarded_strategy_can_be_restored(cx: &mut TestAppContext) {
    if !tools_available() {
        return;
    }
    let fx = Fixture::new(cx, &[]);
    let id = fx.start(cx, false);
    let video = fx.video;
    let window = open(cx, move |window, cx| AiEditPanel::new(video, window, cx));
    let strategy = fx.wait_for_status(cx, id, RunStatus::AwaitingApproval).summary;
    wait_for_window(cx, window, "the strategy card", |window, _| shows(window, "run-discard"));

    click(cx, window, "run-discard");
    assert_eq!(fx.run(cx, id).status, RunStatus::Cancelled);
    wait_for_window(cx, window, "the discarded card", |window, _| shows(window, "run-restore"));
    assert!(!cx.update_window(window, |_, window, _| shows(window, "run-continue")).unwrap());

    click(cx, window, "run-restore");
    let restored = fx.run(cx, id);
    assert_eq!((restored.status, restored.summary), (RunStatus::AwaitingApproval, strategy));
    wait_for_window(cx, window, "the strategy card", |window, _| shows(window, "run-approve"));
    assert_eq!(markers(&fx.stored_events(cx, id)), ["Plan", "Discarded", "Restored"]);
    assert!(latest_run(cx, &fx) == Some(id) && fx.stored_run(cx, id).status == RunStatus::AwaitingApproval);

    // An edit cancelled while the agent worked has no strategy to bring back.
    let restore = cx.update(|cx| fx.service.update(cx, |service, cx| service.restore(id, cx)));
    assert_eq!(restore, Err(RunError::NotAvailable(RunStatus::AwaitingApproval)));
}

#[gpui_kit::test]
fn a_running_edit_can_be_cancelled_and_retried(cx: &mut TestAppContext) {
    if !tools_available() {
        return;
    }
    let fx = Fixture::new(cx, &[("FAKE_AGENT_DELAY_MS", "1500")]);
    let id = fx.start(cx, false);
    let video = fx.video;
    let window = open(cx, move |window, cx| AiEditPanel::new(video, window, cx));
    wait_for_window(cx, window, "the live status", |window, _| shows(window, "run-cancel"));

    click(cx, window, "run-cancel");
    fx.wait_for_status(cx, id, RunStatus::Cancelled);
    wait_for_window(cx, window, "the cancelled card", |window, _| shows(window, "run-retry"));
    assert!(cx.update_window(window, |_, window, _| shows(window, "run-continue")).unwrap(), "a session to resume");

    click(cx, window, "run-retry");
    assert_eq!(fx.run(cx, id).status, RunStatus::Planning);
    cx.update(|cx| fx.service.update(cx, |service, cx| service.cancel(id, cx)));
    fx.wait_for_status(cx, id, RunStatus::Cancelled);
}

#[gpui_kit::test]
fn a_pipelines_row_opens_its_run(cx: &mut TestAppContext) {
    if !tools_available() {
        return;
    }
    let fx = Fixture::new(cx, &[]);
    let id = fx.start(cx, true);
    fx.wait_for_status(cx, id, RunStatus::Succeeded);
    let window = open(cx, PipelinesPage::new);
    let row = format!("pipeline-run-{id}");
    wait_for_window(cx, window, "the run row", |window, _| shows(window, &row));

    // The keyboard reaches the rows like buttons: Tab (the root's `focus_next`), then Enter.
    cx.update_window(window, |_, window, cx| {
        window.focus_next(cx);
        window.press("enter", cx);
    })
    .unwrap();
    assert_eq!(route(cx, &fx), Route::Video { id: fx.video, tab: VideoTab::Footage });
    cx.update(|cx| fx.service.update(cx, |service, _| service.take_revealed(fx.video)));

    cx.update(|cx| fx.state.update(cx, |state, cx| state.navigate(Route::Pipelines, cx)));
    click(cx, window, ElementId::Name(row.into()));
    assert_eq!(route(cx, &fx), Route::Video { id: fx.video, tab: VideoTab::Footage });
    let revealed = cx.update(|cx| fx.service.update(cx, |service, _| service.take_revealed(fx.video)));
    assert_eq!(revealed, Some(id), "the video's panel selects the clicked run");
}

#[gpui_kit::test]
fn settings_save_toggles_at_once_and_text_after_a_pause(cx: &mut TestAppContext) {
    let fx = Fixture::with_store(cx, Store::open_in_memory().unwrap(), &[], false);
    let window = open(cx, AiSettings::new);
    let pipeline = |cx: &mut TestAppContext| cx.read(|cx| fx.state.read(cx).settings().pipeline.clone());
    assert!(!pipeline(cx).auto_approve);

    click(cx, window, "ai-settings-auto-approve");
    assert!(pipeline(cx).auto_approve);
    click(cx, window, "ai-settings-subtitles");
    assert!(pipeline(cx).default_preferences.subtitles);

    let before = pipeline(cx).agent_command;
    click(cx, window, "ai-settings-agent-command");
    cx.update_window(window, |_, window, cx| {
        window.press("secondary-a", cx);
        window.input("/opt/claude/bin/claude", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(pipeline(cx).agent_command, before, "typing is saved after a pause");
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    assert_eq!(pipeline(cx).agent_command, "/opt/claude/bin/claude");
    let service = fx.service.clone();
    wait_until(cx, "the new check", |cx| !service.read(cx).is_checking());
    let doctor = cx.read(|cx| service.read(cx).doctor().cloned()).expect("doctor");
    assert!(!doctor.agent.ok, "the new agent command was checked: {:?}", doctor.agent);
}

#[gpui_kit::test]
fn a_skill_folder_typed_with_a_tilde_is_found_in_the_home_folder(cx: &mut TestAppContext) {
    let fx = Fixture::with_store(cx, Store::open_in_memory().unwrap(), &[], false);
    let window = open(cx, AiSettings::new);
    let pipeline = |cx: &mut TestAppContext| cx.read(|cx| fx.state.read(cx).settings().pipeline.clone());
    let saved = pipeline(cx).skill_dir;
    let type_folder = |cx: &mut TestAppContext, text: &str| {
        click(cx, window, "ai-settings-skill-dir");
        cx.update_window(window, |_, window, cx| {
            window.press("secondary-a", cx);
            window.input(text, cx);
        })
        .unwrap();
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
    };

    // A relative path would resolve against wherever the app was started from.
    type_folder(cx, "video-use");
    assert_eq!(pipeline(cx).skill_dir, saved, "the unusable path is not saved");
    wait_for_window(cx, window, "the reason", |window, _| shows(window, "ai-settings-skill-dir-error"));

    type_folder(cx, "~/video_editing/video-use");
    let home = std::env::home_dir().expect("home folder");
    assert_eq!(pipeline(cx).skill_dir, Some(home.join("video_editing/video-use")));
}

#[gpui_kit::test]
fn takes_dropped_on_the_footage_card_are_copied_in(cx: &mut TestAppContext) {
    let fx = Fixture::with_store(cx, Store::open_in_memory().unwrap(), &[], false);
    let card = fx.root.path().join("sd-card");
    fs::create_dir_all(&card).unwrap();
    let take = card.join("take9.mp4");
    fs::write(&take, vec![0u8; 3 * 1024 * 1024]).unwrap();
    let notes = card.join("notes.txt");
    fs::write(&notes, "not footage").unwrap();
    let video = fx.video;
    let window = open(cx, move |window, cx| AiEditPanel::new(video, window, cx));
    wait_for_window(cx, window, "the footage card", |window, _| shows(window, "footage-drop"));

    cx.update_window(window, |_, window, cx| {
        let position = window.find("footage-drop").bounds().center();
        let paths = ExternalPaths(vec![take.clone(), notes.clone()].into());
        window.dispatch_event(PlatformInput::FileDrop(FileDropEvent::Entered { position, paths }), cx);
        window.dispatch_event(PlatformInput::FileDrop(FileDropEvent::Submit { position }), cx);
    })
    .unwrap();
    let copied = fx.footage.join("take9.mp4");
    wait_until(cx, "the copied take", |_| fs::metadata(&copied).is_ok_and(|meta| meta.len() == 3 * 1024 * 1024));
    assert!(take.is_file(), "the source is copied, not moved");
    assert!(!fx.footage.join("notes.txt").exists(), "only media files are imported");
}

#[gpui_kit::test]
fn text_typed_right_before_leaving_the_settings_is_saved(cx: &mut TestAppContext) {
    let fx = Fixture::with_store(cx, Store::open_in_memory().unwrap(), &[], false);
    let (window, settings) = open_host(cx, |window, cx| cx.new(|cx| AiSettings::new(window, cx)).into());
    click(cx, window, "ai-settings-agent-command");
    cx.update_window(window, |_, window, cx| {
        window.press("secondary-a", cx);
        window.input("/opt/claude/bin/claude", cx);
    })
    .unwrap();
    cx.run_until_parked();

    close(cx, &settings);
    let pipeline = cx.read(|cx| fx.state.read(cx).settings().pipeline.clone());
    assert_eq!(pipeline.agent_command, "/opt/claude/bin/claude");
}

#[gpui_kit::test]
fn the_brief_form_survives_leaving_the_video(cx: &mut TestAppContext) {
    if !tools_available() {
        return;
    }
    let fx = Fixture::new(cx, &[]);
    let video = fx.video;
    let panel =
        move |window: &mut Window, cx: &mut App| -> AnyView { cx.new(|cx| AiEditPanel::new(video, window, cx)).into() };
    let (window, page) = open_host(cx, panel);
    let service = fx.service.clone();
    wait_until(cx, "requirement check", |cx| service.read(cx).doctor().is_some_and(|doctor| doctor.ready()));
    wait_for_window(cx, window, "the composer", |window, _| shows(window, "ai-edit-start"));
    click(cx, window, "brief-subtitles");
    click(cx, window, "brief-auto-approve");

    // Away to another page and back.
    close(cx, &page);
    show(cx, window, &page, panel);
    wait_for_window(cx, window, "the composer", |window, _| shows(window, "ai-edit-start"));
    click(cx, window, "ai-edit-start");
    let id = latest_run(cx, &fx).expect("the panel started a run");
    let run = fx.run(cx, id);
    assert!(run.brief.contains("- Burn subtitles into the render."), "{}", run.brief);
    assert_eq!(run.status, RunStatus::Rendering, "the strategy review stays skipped");
    fx.wait_for_status(cx, id, RunStatus::Succeeded);
}

#[gpui_kit::test]
fn the_brief_follows_the_video_until_the_creator_edits_it(cx: &mut TestAppContext) {
    if !tools_available() {
        return;
    }
    let fx = Fixture::new(cx, &[]);
    let video = fx.video;
    let write_script = |cx: &mut TestAppContext, text: &str| {
        cx.update(|cx| {
            fx.state.update(cx, |state, cx| {
                let mut script = state.ensure_document(video, DocumentKind::Script, cx).expect("script");
                script.blocks = vec![Block::paragraph(text)];
                state.save_document(script, cx);
            })
        });
        cx.run_until_parked();
    };
    let start = |cx: &mut TestAppContext, window: AnyWindowHandle| -> String {
        wait_for_window(cx, window, "the composer", |window, _| shows(window, "ai-edit-start"));
        click(cx, window, "ai-edit-start");
        let id = latest_run(cx, &fx).expect("the panel started a run");
        let brief = fx.run(cx, id).brief;
        fx.wait_for_status(cx, id, RunStatus::AwaitingApproval);
        click(cx, window, "ai-edit-new");
        brief
    };
    write_script(cx, "The template's opening.");
    let panel =
        move |window: &mut Window, cx: &mut App| -> AnyView { cx.new(|cx| AiEditPanel::new(video, window, cx)).into() };
    let (window, page) = open_host(cx, panel);
    let service = fx.service.clone();
    wait_until(cx, "requirement check", |cx| service.read(cx).doctor().is_some_and(|doctor| doctor.ready()));

    // The script changes while the tab is open, the hook while the page is closed.
    write_script(cx, "Creators who film with one camera.");
    close(cx, &page);
    cx.update(|cx| {
        fx.state.update(cx, |state, cx| {
            let mut changed = state.video(video).unwrap().clone();
            changed.hook = "One camera is enough.".into();
            state.update_video(changed, cx);
        })
    });
    show(cx, window, &page, panel);
    let brief = start(cx, window);
    assert!(brief.contains("Creators who film with one camera.") && !brief.contains("template's"), "{brief}");
    assert!(brief.contains("Hook: One camera is enough."), "{brief}");

    // Once the creator edits it, the brief keeps their words and says it is out of date.
    wait_for_window(cx, window, "the composer", |window, _| shows(window, "brief"));
    click(cx, window, "brief");
    cx.update_window(window, |_, window, cx| window.input("Keep my words. ", cx)).unwrap();
    write_script(cx, "A newer line.");
    wait_for_window(cx, window, "the notice", |window, _| shows(window, "brief-stale"));
    let brief = start(cx, window);
    assert!(brief.contains("Keep my words.") && !brief.contains("A newer line."), "{brief}");

    // Reset drafts it from the video again.
    wait_for_window(cx, window, "the composer", |window, _| shows(window, "brief-reset"));
    click(cx, window, "brief-reset");
    assert!(!cx.update_window(window, |_, window, _| shows(window, "brief-stale")).unwrap());
    let brief = start(cx, window);
    assert!(brief.contains("A newer line.") && !brief.contains("Keep my words."), "{brief}");
}

#[gpui_kit::test]
fn a_long_activity_log_lays_out_only_the_rows_in_view(cx: &mut TestAppContext) {
    let fx = Fixture::with_store(cx, Store::open_in_memory().unwrap(), &[], false);
    let now = Utc::now();
    let run = PipelineRun {
        id: RunId::new(),
        video_id: fx.video,
        status: RunStatus::Succeeded,
        brief: String::new(),
        footage_dir: fx.footage.clone(),
        session_id: Some("s".into()),
        summary: Some("Rendered.".into()),
        output: None,
        error: None,
        cost_usd: None,
        created_at: now,
        updated_at: now,
    };
    cx.read(|cx| {
        let store = fx.state.read(cx).store();
        let mut store = store.lock().unwrap();
        store.insert_run(&run).unwrap();
        for ix in 0..1000 {
            let event = if ix % 2 == 0 {
                AgentEvent::ToolUse {
                    id: format!("t{ix}"),
                    name: "Bash".into(),
                    summary: format!("ffmpeg -i take{ix}.mp4"),
                }
            } else {
                AgentEvent::ToolResult { tool_use_id: format!("t{}", ix - 1), is_error: false, summary: "ok".into() }
            };
            store.append_run_event(run.id, &LogEvent::Agent(event).to_payload()).unwrap();
        }
    });
    // A fresh service reads the run the store now holds.
    cx.update(|cx| PipelineService::init(fx.state.clone(), cx));
    let video = fx.video;
    let window = open(cx, move |window, cx| AiEditPanel::new(video, window, cx));
    let (first, last) = (format!("timeline-1-{}", run.id), format!("timeline-999-{}", run.id));
    wait_for_window(cx, window, "the first activity rows", |window, _| shows(window, &first));
    assert!(!cx.update_window(window, |_, window, _| shows(window, &last)).unwrap(), "rows far below are not laid out");
}

/// The panel inside a page that scrolls, as on the video page.
struct ScrollPage {
    panel: Entity<AiEditPanel>,
    scroll: ScrollHandle,
}

impl Render for ScrollPage {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().id("page").size_full().overflow_y_scroll().track_scroll(&self.scroll).child(self.panel.clone())
    }
}

#[gpui_kit::test]
fn the_wheel_scrolls_back_through_a_log_that_followed_a_live_edit(cx: &mut TestAppContext) {
    if !tools_available() {
        return;
    }
    let fx = Fixture::new(cx, &[]);
    let chatty = chatty_agent(fx.root.path());
    cx.update(|cx| {
        fx.state.update(cx, |state, cx| {
            state.update_settings(cx, |settings| settings.pipeline.agent_command = chatty.display().to_string())
        })
    });
    let video = fx.video;
    let scroll = ScrollHandle::new();
    let page_scroll = scroll.clone();
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(Default::default(), size(px(1280.), px(900.))))),
        ..WindowOptions::default()
    };
    let (window, _) = cx
        .update(|cx| {
            gpui_kit::open_window(options, cx, move |window, cx| {
                let panel = cx.new(|cx| AiEditPanel::new(video, window, cx));
                cx.new(|_| ScrollPage { panel, scroll: page_scroll })
            })
        })
        .expect("open test window");
    // The panel watches the edit from its start, so its log follows the newest rows.
    let id = fx.start(cx, true);
    fx.wait_for_status(cx, id, RunStatus::Failed);
    let events = fx.stored_events(cx, id);
    let last = format!("timeline-{}-{id}", events.last().expect("events").seq);
    let summary_seq = events
        .iter()
        .rev()
        .find(|entry| matches!(entry.event, LogEvent::Agent(AgentEvent::AssistantText { .. })))
        .expect("the summary")
        .seq;
    let wheel = |cx: &mut TestAppContext, target: &str, lines: f32| {
        let target = target.to_owned();
        cx.update_window(window, |_, window, cx| window.scroll(target, ScrollDelta::Lines(point(0., lines)), cx))
            .unwrap();
        cx.run_until_parked();
    };
    let visible = |cx: &mut TestAppContext, id: &str| {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            shows(window, id)
        })
        .unwrap()
    };

    // Down the page to the activity box.
    wheel(cx, "footage-drop", -100.);
    wait_for_window(cx, window, "the end of the log", |window, _| shows(window, &last));
    let page_end = scroll.offset();
    assert!(page_end.y < px(0.), "the page scrolled");

    // The final summary shows its first lines; expanded, it is taller than the box.
    let toggle = format!("message-toggle-{summary_seq}-{id}");
    assert!(visible(cx, &toggle), "a long message offers Show more");
    click(cx, window, ElementId::Name(toggle.into()));
    let summary_row = ElementId::Name(format!("timeline-{summary_seq}-{id}").into());
    wait_for_window(cx, window, "the whole summary", |window, _| {
        window.find(summary_row.clone()).bounds().size.height > window.find("run-activity").bounds().size.height
    });

    // Back to the end, where the log follows its newest rows again, then up.
    wheel(cx, "run-activity", -500.);
    assert!(visible(cx, &last), "the wheel reaches the end of the log");
    wheel(cx, "run-activity", 5.);
    assert!(!visible(cx, &last), "wheeling up leaves the end of the log");
    assert!(!visible(cx, &last), "and stays there");
    assert_eq!(scroll.offset(), page_end, "the log takes the wheel, not the page");

    // At the start of the log, the wheel moves the page.
    wheel(cx, "run-activity", 2000.);
    assert_eq!(scroll.offset(), page_end, "the log scrolls to its start first");
    wheel(cx, "run-activity", 3.);
    assert!(scroll.offset().y > page_end.y, "then the page scrolls up");
}

#[gpui_kit::test]
fn a_short_activity_log_takes_only_the_room_it_needs(cx: &mut TestAppContext) {
    let fx = Fixture::with_store(cx, Store::open_in_memory().unwrap(), &[], false);
    let now = Utc::now();
    let run = PipelineRun {
        id: RunId::new(),
        video_id: fx.video,
        status: RunStatus::Failed,
        brief: String::new(),
        footage_dir: fx.footage.clone(),
        session_id: None,
        summary: None,
        output: None,
        error: Some("The agent stopped.".into()),
        cost_usd: None,
        created_at: now,
        updated_at: now,
    };
    cx.read(|cx| {
        let store = fx.state.read(cx).store();
        let mut store = store.lock().unwrap();
        store.insert_run(&run).unwrap();
        let events = [
            LogEvent::Phase(PhaseStarted { phase: PhaseKind::Plan, text: None, retry: false }),
            LogEvent::Agent(AgentEvent::AssistantText { text: "Looking at the footage.".into() }),
            LogEvent::Agent(AgentEvent::ToolUse { id: "t1".into(), name: "Bash".into(), summary: "ls".into() }),
        ];
        for event in events {
            store.append_run_event(run.id, &event.to_payload()).unwrap();
        }
    });
    cx.update(|cx| PipelineService::init(fx.state.clone(), cx));
    let video = fx.video;
    let window = open(cx, move |window, cx| AiEditPanel::new(video, window, cx));
    let last = format!("timeline-3-{}", run.id);
    wait_for_window(cx, window, "the log", |window, _| shows(window, &last));
    wait_for_window(cx, window, "the box to fit the log", |window, _| {
        let row = window.find(ElementId::Name(last.clone().into())).bounds();
        let log = window.find("run-activity").bounds();
        log.size.height < px(200.) && row.bottom() <= log.bottom()
    });
}
