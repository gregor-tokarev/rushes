//! [`PipelineService`]: starts and steers AI edit runs and keeps their state.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, PoisonError, mpsc};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use futures::StreamExt as _;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui_kit::{App, AppContext as _, Context, Entity, Global, SharedString, Subscription, Task};
use videocrm_core::media::{self, MediaKind};
use videocrm_core::settings::PipelineSettings;
use videocrm_core::{PipelineRun, RunId, RunStatus, Store, StoreError, VideoId, VideoStatus};
use videocrm_pipeline::{
    AgentConfig, AgentEvent, Doctor, Phase, PhaseOutcome, PipelineError, RunContext, default_skill_dir, doctor,
    run_phase,
};

use super::timeline::{LogEvent, PhaseKind, PhaseStarted, TimelineEntry};
use crate::state::{AppEvent, AppState};

/// Error of runs a previous session left running.
pub const INTERRUPTED: &str = "Interrupted when VideoCRM closed";

/// Separates the agent's error message from its stderr tail in [`PipelineRun::error`].
const STDERR_SEPARATOR: &str = "\n\nAgent output:\n";

/// How long quitting waits for agents to stop, beyond their kill grace period.
const QUIT_MARGIN: Duration = Duration::from_secs(1);

/// Why a run could not be started or continued.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunError {
    VideoNotFound,
    RunNotFound,
    /// Another run of the same video is still working.
    AlreadyRunning,
    /// The media folder could not be created.
    NoMediaFolder,
    /// The footage folder has no video or audio files.
    NoFootage(PathBuf),
    /// No video-use skill folder is configured and there is no home directory.
    NoSkillFolder,
    /// The run is not in a state that allows the request.
    NotAvailable(RunStatus),
    Store(String),
    Spawn(String),
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunError::VideoNotFound => write!(f, "This video no longer exists."),
            RunError::RunNotFound => write!(f, "This edit no longer exists."),
            RunError::AlreadyRunning => {
                write!(f, "An AI edit is already running for this video. Wait for it to finish or cancel it.")
            }
            RunError::NoMediaFolder => {
                write!(f, "Couldn't create the video's media folder. Check the media folder in Settings.")
            }
            RunError::NoFootage(dir) => {
                write!(f, "Add footage first: video-use edits the raw takes in {}.", dir.display())
            }
            RunError::NoSkillFolder => write!(f, "Choose the video-use skill folder in Settings."),
            RunError::NotAvailable(status) => {
                write!(f, "This isn't possible while the edit is {}.", status.label().to_lowercase())
            }
            RunError::Store(error) => write!(f, "Couldn't save the edit: {error}"),
            RunError::Spawn(error) => write!(f, "Couldn't start the agent: {error}"),
        }
    }
}

impl std::error::Error for RunError {}

impl From<StoreError> for RunError {
    fn from(error: StoreError) -> Self {
        RunError::Store(error.to_string())
    }
}

/// The phase a running agent works on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LivePhase {
    pub kind: PhaseKind,
    pub started_at: DateTime<Utc>,
    /// Cancel was requested; the agent is being stopped.
    pub cancelling: bool,
}

struct LiveRun {
    video_id: VideoId,
    phase: LivePhase,
    cancel: Arc<AtomicBool>,
    /// Signalled by the agent thread once the agent and its children are gone.
    stopped: mpsc::Receiver<()>,
}

enum AgentMessage {
    Event(AgentEvent),
    Finished(Result<PhaseOutcome, PipelineError>),
}

/// App-wide pipeline runner: starts phases on background threads, persists runs
/// and events, survives navigation, stops agents on quit.
///
/// Agent phases run for minutes, so each gets its own thread rather than a slot of
/// the background executor; its events reach the foreground through a channel,
/// where they are persisted and appended to the run's timeline. Observe the
/// service to follow runs, the timeline and the requirement check.
pub struct PipelineService {
    state: Entity<AppState>,
    /// Every run, newest first.
    runs: Vec<PipelineRun>,
    live: HashMap<RunId, LiveRun>,
    /// Event logs loaded so far, by run.
    timelines: HashMap<RunId, Vec<TimelineEntry>>,
    doctor: Option<Doctor>,
    doctor_task: Option<Task<()>>,
    /// The settings the last requirement check ran with.
    checked: Option<PipelineSettings>,
    agent_env: Vec<(String, String)>,
    /// A run another view asked to show; taken by the panel of its video.
    revealed: Option<RunId>,
    _subscriptions: Vec<Subscription>,
}

struct GlobalPipelineService(Entity<PipelineService>);

impl Global for GlobalPipelineService {}

impl PipelineService {
    /// Creates the service and registers it globally. Call once at startup, after
    /// `AppState::set_global`.
    ///
    /// Runs a previous session left working are marked failed ([`INTERRUPTED`]); they
    /// can be continued when the agent session is known.
    pub fn init(state: Entity<AppState>, cx: &mut App) {
        let service = cx.new(|cx| {
            let subscriptions = vec![
                cx.subscribe(&state, |this: &mut Self, _, event: &AppEvent, cx| {
                    if *event == AppEvent::SettingsChanged {
                        this.on_settings_changed(cx);
                    }
                }),
                cx.on_app_quit(|this: &mut Self, _| {
                    this.stop_all();
                    async {}
                }),
            ];
            let mut service = PipelineService {
                state,
                runs: Vec::new(),
                live: HashMap::new(),
                timelines: HashMap::new(),
                doctor: None,
                doctor_task: None,
                checked: None,
                agent_env: Vec::new(),
                revealed: None,
                _subscriptions: subscriptions,
            };
            if let Err(error) = service.recover_interrupted(cx) {
                service.report(error, cx);
            }
            service.reload_runs(cx);
            service.refresh_doctor(cx);
            service
        });
        cx.set_global(GlobalPipelineService(service));
    }

    pub fn global(cx: &App) -> Entity<PipelineService> {
        cx.global::<GlobalPipelineService>().0.clone()
    }

    pub fn state(&self) -> &Entity<AppState> {
        &self.state
    }

    // Runs

    /// Every run, newest first.
    pub fn runs(&self) -> &[PipelineRun] {
        &self.runs
    }

    /// Runs of one video, newest first.
    pub fn video_runs(&self, video_id: VideoId) -> impl Iterator<Item = &PipelineRun> {
        self.runs.iter().filter(move |run| run.video_id == video_id)
    }

    pub fn run(&self, id: RunId) -> Option<&PipelineRun> {
        self.runs.iter().find(|run| run.id == id)
    }

    /// The phase the agent of `id` is working on, while it runs.
    pub fn live_phase(&self, id: RunId) -> Option<LivePhase> {
        self.live.get(&id).map(|live| live.phase)
    }

    /// The run of `video_id` whose agent is working, if any.
    pub fn live_run_of(&self, video_id: VideoId) -> Option<RunId> {
        self.live.iter().find(|(_, live)| live.video_id == video_id).map(|(id, _)| *id)
    }

    /// The loaded event log of a run; see [`Self::load_timeline`].
    pub fn timeline(&self, id: RunId) -> &[TimelineEntry] {
        self.timelines.get(&id).map_or(&[], Vec::as_slice)
    }

    /// Reads a run's event log from the store unless it is loaded already. Live runs
    /// keep their log up to date as events arrive.
    pub fn load_timeline(&mut self, id: RunId, cx: &mut Context<Self>) {
        if self.timelines.contains_key(&id) {
            return;
        }
        let events = self.with_store(cx, |store| store.list_run_events(id));
        match events {
            Ok(events) => {
                self.timelines.insert(id, events.into_iter().map(TimelineEntry::from).collect());
                cx.notify();
            }
            Err(error) => self.report(error, cx),
        }
    }

    /// Asks the AI edit panel of the run's video to select it.
    pub fn reveal_run(&mut self, id: RunId, cx: &mut Context<Self>) {
        self.revealed = Some(id);
        cx.notify();
    }

    /// The run to select in `video_id`'s panel, if one was revealed; clears the request.
    pub fn take_revealed(&mut self, video_id: VideoId) -> Option<RunId> {
        let id = self.revealed.filter(|id| self.run(*id).is_some_and(|run| run.video_id == video_id))?;
        self.revealed = None;
        Some(id)
    }

    /// Extra environment for the agent and every tool it runs, such as a complete
    /// `PATH` when the app was started from a desktop launcher.
    pub fn set_agent_env(&mut self, env: Vec<(String, String)>, cx: &mut Context<Self>) {
        self.agent_env = env;
        self.refresh_doctor(cx);
    }

    /// Starts an AI edit of `video_id`: plans (or, with `auto_approve`, plans and
    /// renders in one pass) from the footage in the video's media folder.
    pub fn start_run(
        &mut self,
        video_id: VideoId,
        brief: String,
        auto_approve: bool,
        cx: &mut Context<Self>,
    ) -> Result<RunId, RunError> {
        if self.live_run_of(video_id).is_some() {
            return Err(RunError::AlreadyRunning);
        }
        if self.state.read(cx).video(video_id).is_none() {
            return Err(RunError::VideoNotFound);
        }
        let folder = self
            .state
            .update(cx, |state, cx| state.ensure_video_folder(video_id, cx))
            .ok_or(RunError::NoMediaFolder)?;
        let footage_dir = folder.join("footage");
        if !has_footage(&footage_dir) {
            return Err(RunError::NoFootage(footage_dir));
        }

        let now = Utc::now();
        let run = PipelineRun {
            id: RunId::new(),
            video_id,
            status: RunStatus::Queued,
            brief,
            footage_dir,
            session_id: None,
            summary: None,
            output: None,
            error: None,
            cost_usd: None,
            created_at: now,
            updated_at: now,
        };
        self.with_store(cx, |store| store.insert_run(&run))?;
        let id = run.id;
        self.runs.insert(0, run);
        self.timelines.insert(id, Vec::new());
        self.mark_editing(video_id, cx);

        let phase = if auto_approve { PhaseKind::Auto } else { PhaseKind::Plan };
        if let Err(error) = self.launch(id, phase, None, cx) {
            self.finish(id, Err(error.to_string()), cx);
            return Err(error);
        }
        Ok(id)
    }

    /// Renders the strategy a run proposed, with the creator's optional notes.
    pub fn approve(&mut self, id: RunId, notes: Option<String>, cx: &mut Context<Self>) -> Result<(), RunError> {
        let run = self.run(id).ok_or(RunError::RunNotFound)?;
        if run.status != RunStatus::AwaitingApproval {
            return Err(RunError::NotAvailable(run.status));
        }
        let notes = notes.map(|notes| notes.trim().to_owned()).filter(|notes| !notes.is_empty());
        self.continue_run(id, PhaseKind::Approve, notes, cx)
    }

    /// Sends the creator's feedback. While the strategy awaits approval this asks for a
    /// revised strategy; after a render (or a failure) it changes the edit and renders
    /// again.
    pub fn send_feedback(&mut self, id: RunId, message: String, cx: &mut Context<Self>) -> Result<(), RunError> {
        let run = self.run(id).ok_or(RunError::RunNotFound)?;
        let phase = match run.status {
            RunStatus::AwaitingApproval => PhaseKind::Revise,
            RunStatus::Succeeded | RunStatus::Failed | RunStatus::Cancelled => PhaseKind::Feedback,
            status => return Err(RunError::NotAvailable(status)),
        };
        self.continue_run(id, phase, Some(message.trim().to_owned()), cx)
    }

    /// Runs the phase that failed or was cancelled again, resuming the agent session
    /// when there is one.
    pub fn retry(&mut self, id: RunId, cx: &mut Context<Self>) -> Result<(), RunError> {
        let run = self.run(id).ok_or(RunError::RunNotFound)?;
        if !matches!(run.status, RunStatus::Failed | RunStatus::Cancelled) {
            return Err(RunError::NotAvailable(run.status));
        }
        self.load_timeline(id, cx);
        let last = self.timeline(id).iter().rev().find_map(|entry| match &entry.event {
            LogEvent::Phase(started) => Some(started.clone()),
            LogEvent::Agent(_) => None,
        });
        let (phase, text) = last.map_or((PhaseKind::Plan, None), |started| (started.phase, started.text));
        self.continue_run(id, phase, text, cx)
    }

    /// Stops the agent of a working run, or discards a strategy awaiting approval.
    pub fn cancel(&mut self, id: RunId, cx: &mut Context<Self>) {
        if let Some(live) = self.live.get_mut(&id) {
            live.cancel.store(true, Ordering::SeqCst);
            live.phase.cancelling = true;
            cx.notify();
        } else if self.run(id).is_some_and(|run| run.status == RunStatus::AwaitingApproval) {
            self.update_run(id, cx, |run| run.status = RunStatus::Cancelled);
            self.after_status_change(cx);
        }
    }

    fn continue_run(
        &mut self,
        id: RunId,
        phase: PhaseKind,
        text: Option<String>,
        cx: &mut Context<Self>,
    ) -> Result<(), RunError> {
        let video_id = self.run(id).ok_or(RunError::RunNotFound)?.video_id;
        if self.live_run_of(video_id).is_some() {
            return Err(RunError::AlreadyRunning);
        }
        self.launch(id, phase, text, cx)
    }

    /// Records the phase, sets the run working and starts the agent thread.
    fn launch(
        &mut self,
        id: RunId,
        phase: PhaseKind,
        text: Option<String>,
        cx: &mut Context<Self>,
    ) -> Result<(), RunError> {
        let run = self.run(id).ok_or(RunError::RunNotFound)?.clone();
        let settings = self.state.read(cx).settings().pipeline.clone();
        let skill_dir = settings.skill_dir.clone().or_else(default_skill_dir).ok_or(RunError::NoSkillFolder)?;
        let config = self.agent_config(&settings);
        let context =
            RunContext { footage_dir: run.footage_dir.clone(), skill_dir, session_id: run.session_id.clone() };
        let pipeline_phase = match phase {
            PhaseKind::Plan => Phase::Plan { brief: run.brief.clone() },
            PhaseKind::Auto => Phase::Auto { brief: run.brief.clone() },
            PhaseKind::Revise => Phase::Plan { brief: revision_brief(&run.brief, text.as_deref().unwrap_or_default()) },
            PhaseKind::Approve => Phase::Approve { notes: text.clone() },
            PhaseKind::Feedback => Phase::Feedback { message: text.clone().unwrap_or_default() },
        };

        self.append_event(id, LogEvent::Phase(PhaseStarted { phase, text }), cx);
        let status = if phase.renders() { RunStatus::Rendering } else { RunStatus::Planning };
        self.update_run(id, cx, |run| {
            run.status = status;
            run.error = None;
        });

        let (events, mut receiver) = unbounded();
        let (stopped_tx, stopped) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let agent_cancel = cancel.clone();
        std::thread::Builder::new()
            .name(format!("ai-edit-{}", id.short()))
            .spawn(move || {
                let result = run_phase(
                    &config,
                    &context,
                    pipeline_phase,
                    &mut |event| send(&events, AgentMessage::Event(event.clone())),
                    &agent_cancel,
                );
                send(&events, AgentMessage::Finished(result));
                // The quit handler may already have stopped waiting.
                _ = stopped_tx.send(());
            })
            .map_err(|error| RunError::Spawn(error.to_string()))?;

        let phase = LivePhase { kind: phase, started_at: Utc::now(), cancelling: false };
        self.live.insert(id, LiveRun { video_id: run.video_id, phase, cancel, stopped });
        cx.spawn(async move |this, cx| {
            while let Some(message) = receiver.next().await {
                let handled = this.update(cx, |service, cx| match message {
                    AgentMessage::Event(event) => service.on_agent_event(id, event, cx),
                    AgentMessage::Finished(result) => service.on_phase_finished(id, result, cx),
                });
                if handled.is_err() {
                    break;
                }
            }
        })
        .detach();
        self.after_status_change(cx);
        Ok(())
    }

    fn on_agent_event(&mut self, id: RunId, event: AgentEvent, cx: &mut Context<Self>) {
        match &event {
            AgentEvent::Init { session_id, .. } if !session_id.is_empty() => {
                let session_id = session_id.clone();
                self.update_run(id, cx, |run| run.session_id = Some(session_id));
            }
            AgentEvent::Result { cost_usd: Some(cost), .. } => {
                let cost = *cost;
                self.update_run(id, cx, |run| run.cost_usd = Some(run.cost_usd.unwrap_or(0.0) + cost));
            }
            _ => {}
        }
        self.append_event(id, LogEvent::Agent(event), cx);
        cx.notify();
    }

    fn on_phase_finished(&mut self, id: RunId, result: Result<PhaseOutcome, PipelineError>, cx: &mut Context<Self>) {
        let Some(live) = self.live.remove(&id) else { return };
        let phase = live.phase.kind;
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(PipelineError::Cancelled) => {
                self.update_run(id, cx, |run| run.status = RunStatus::Cancelled);
                self.after_status_change(cx);
                return;
            }
            Err(PipelineError::AgentFailed { message, stderr_tail, session_id }) => {
                if let Some(session_id) = session_id {
                    self.update_run(id, cx, |run| run.session_id = Some(session_id));
                }
                self.finish(id, Err(failure_text(&message, &stderr_tail)), cx);
                return;
            }
            Err(error) => {
                self.finish(id, Err(error.to_string()), cx);
                return;
            }
        };

        let summary = Some(outcome.summary_text.trim().to_owned()).filter(|summary| !summary.is_empty());
        self.update_run(id, cx, |run| {
            if outcome.session_id.is_some() {
                run.session_id = outcome.session_id.clone();
            }
            if summary.is_some() {
                run.summary = summary.clone();
            }
        });
        let result = if outcome.is_error {
            Err(summary.unwrap_or_else(|| "The agent reported an error.".to_owned()))
        } else if !phase.renders() {
            Ok(None)
        } else {
            match outcome.final_cut {
                Some(final_cut) => Ok(Some(final_cut)),
                None => Err("The agent finished without rendering edit/final.mp4.".to_owned()),
            }
        };
        self.finish(id, result, cx);
    }

    /// Settles a phase: `Ok(None)` is a strategy awaiting approval, `Ok(Some(cut))` a
    /// rendered edit, `Err` a failure message.
    fn finish(&mut self, id: RunId, result: Result<Option<PathBuf>, String>, cx: &mut Context<Self>) {
        self.live.remove(&id);
        let final_cut = result.as_ref().ok().cloned().flatten();
        self.update_run(id, cx, |run| match result {
            Ok(None) => run.status = RunStatus::AwaitingApproval,
            Ok(Some(final_cut)) => {
                run.status = RunStatus::Succeeded;
                run.output = Some(final_cut);
            }
            Err(error) => {
                run.status = RunStatus::Failed;
                run.error = Some(error);
            }
        });
        if let (Some(final_cut), Some(video_id)) = (final_cut, self.run(id).map(|run| run.video_id)) {
            self.state.update(cx, |state, cx| {
                let Some(mut video) = state.video(video_id).cloned() else { return };
                if video.publish.file.is_none() {
                    video.publish.file = Some(final_cut);
                    state.update_video(video, cx);
                }
            });
        }
        self.after_status_change(cx);
    }

    fn after_status_change(&mut self, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| state.reload(cx));
        cx.notify();
    }

    /// Moves a video that is not that far yet to Editing.
    fn mark_editing(&mut self, video_id: VideoId, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            let Some(mut video) = state.video(video_id).cloned() else { return };
            if video.status < VideoStatus::Editing {
                video.status = VideoStatus::Editing;
                state.update_video(video, cx);
            }
        });
    }

    /// Applies `change` to the cached run and saves it.
    fn update_run(&mut self, id: RunId, cx: &mut Context<Self>, change: impl FnOnce(&mut PipelineRun)) {
        let Some(ix) = self.runs.iter().position(|run| run.id == id) else { return };
        let mut run = self.runs[ix].clone();
        change(&mut run);
        let saved = self.with_store(cx, |store| store.update_run(&mut run));
        match saved {
            Ok(()) => self.runs[ix] = run,
            Err(error) => self.report(error, cx),
        }
    }

    fn append_event(&mut self, id: RunId, event: LogEvent, cx: &mut Context<Self>) {
        let stored = self.with_store(cx, |store| store.append_run_event(id, &event.to_payload()));
        match stored {
            Ok(stored) => {
                if let Some(timeline) = self.timelines.get_mut(&id) {
                    timeline.push(TimelineEntry { seq: stored.seq, at: stored.at, event });
                }
            }
            Err(error) => self.report(error, cx),
        }
    }

    fn reload_runs(&mut self, cx: &mut Context<Self>) {
        let runs = self.with_store(cx, |store| store.list_runs(None));
        match runs {
            Ok(runs) => self.runs = runs,
            Err(error) => self.report(error, cx),
        }
    }

    fn recover_interrupted(&mut self, cx: &mut Context<Self>) -> Result<(), StoreError> {
        self.with_store(cx, |store| {
            for mut run in store.list_runs(None)? {
                if run.status.is_active() {
                    run.status = RunStatus::Failed;
                    run.error = Some(INTERRUPTED.to_owned());
                    store.update_run(&mut run)?;
                }
            }
            Ok(())
        })
    }

    /// Cancels every working agent and waits until their process groups are gone, so
    /// no agent keeps running (and spending) after the app quits.
    fn stop_all(&mut self) {
        for live in self.live.values() {
            live.cancel.store(true, Ordering::SeqCst);
        }
        let deadline = Instant::now() + AgentConfig::default().kill_grace + QUIT_MARGIN;
        for live in self.live.values() {
            _ = live.stopped.recv_timeout(deadline.saturating_duration_since(Instant::now()));
        }
    }

    // Requirements

    /// The last requirement check, once one has finished.
    pub fn doctor(&self) -> Option<&Doctor> {
        self.doctor.as_ref()
    }

    pub fn is_checking(&self) -> bool {
        self.doctor_task.is_some()
    }

    /// Checks the agent, the skill, ffmpeg, the ElevenLabs key and Python again on a
    /// background thread.
    pub fn refresh_doctor(&mut self, cx: &mut Context<Self>) {
        let settings = self.state.read(cx).settings().pipeline.clone();
        let config = self.agent_config(&settings);
        let skill_dir = settings.skill_dir.clone();
        self.checked = Some(settings);
        self.doctor_task = Some(cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { doctor(&config, skill_dir.as_deref()) }).await;
            _ = this.update(cx, |this, cx| {
                this.doctor = Some(result);
                this.doctor_task = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn on_settings_changed(&mut self, cx: &mut Context<Self>) {
        let settings = &self.state.read(cx).settings().pipeline;
        let changed = self.checked.as_ref().is_none_or(|checked| {
            checked.agent_command != settings.agent_command || checked.skill_dir != settings.skill_dir
        });
        if changed {
            self.refresh_doctor(cx);
        }
    }

    fn agent_config(&self, settings: &PipelineSettings) -> AgentConfig {
        let command = settings.agent_command.trim();
        let defaults = AgentConfig::default();
        AgentConfig {
            command: if command.is_empty() { defaults.command.clone() } else { PathBuf::from(command) },
            model: settings.model.as_deref().map(str::trim).filter(|model| !model.is_empty()).map(str::to_owned),
            env: self.agent_env.clone(),
            ..defaults
        }
    }

    /// Runs `f` with the store locked; keep `f` short.
    fn with_store<T>(&self, cx: &App, f: impl FnOnce(&mut Store) -> T) -> T {
        let store = self.state.read(cx).store();
        let mut store = store.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut store)
    }

    fn report(&self, error: impl fmt::Display, cx: &mut Context<Self>) {
        let message = SharedString::from(error.to_string());
        self.state.update(cx, |_, cx| cx.emit(AppEvent::Error(message)));
    }
}

fn send(events: &UnboundedSender<AgentMessage>, message: AgentMessage) {
    // A closed channel means the app is going away; the agent is stopped by the
    // cancel flag, not by its events.
    _ = events.unbounded_send(message);
}

/// Whether `dir` holds at least one video or audio take.
fn has_footage(dir: &Path) -> bool {
    media::list_footage(dir)
        .is_ok_and(|files| files.iter().any(|file| matches!(file.kind, MediaKind::Video | MediaKind::Audio)))
}

/// Plan prompt for reworking a proposed strategy: the creator's feedback on top of
/// the original brief.
fn revision_brief(brief: &str, feedback: &str) -> String {
    format!(
        "You proposed an edit strategy in this session (also in ./edit/project.md). The creator \
         reviewed it and asks for these changes:\n\n{}\n\nRevise the strategy accordingly. The \
         original brief was:\n\n{}",
        feedback.trim(),
        brief.trim()
    )
}

/// The text stored in [`PipelineRun::error`] for a failed agent.
fn failure_text(message: &str, stderr_tail: &[String]) -> String {
    if stderr_tail.is_empty() {
        message.to_owned()
    } else {
        format!("{message}{STDERR_SEPARATOR}{}", stderr_tail.join("\n"))
    }
}

/// Splits a run error into the message and the agent's stderr tail, if any.
pub fn split_failure(error: &str) -> (&str, Option<&str>) {
    match error.split_once(STDERR_SEPARATOR) {
        Some((message, tail)) => (message, Some(tail)),
        None => (error, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_keep_the_stderr_tail_apart() {
        let text = failure_text("agent failed: API Error", &["line 1".into(), "line 2".into()]);
        assert_eq!(split_failure(&text), ("agent failed: API Error", Some("line 1\nline 2")));
        assert_eq!(split_failure(&failure_text("cancelled", &[])), ("cancelled", None));
    }

    #[test]
    fn revision_brief_leads_with_the_feedback() {
        let brief = revision_brief("Cut it tight.", "  Keep the intro joke. ");
        assert!(brief.contains("asks for these changes:\n\nKeep the intro joke.\n\nRevise"), "{brief}");
        assert!(brief.ends_with("The original brief was:\n\nCut it tight."), "{brief}");
    }
}
