//! [`PublishAssistant`]: the agent writing a video's YouTube details, and Codex
//! drawing its thumbnails in a chat, for the Publish tab.
//!
//! Both run for a while (seconds for the details, minutes for thumbnails), on a
//! thread of their own, and keep going when the creator leaves the tab: the
//! assistant applies the details to the video itself and keeps the chat's
//! answers and thumbnails, so a Publish tab opened later shows the outcome too.
//!
//! A video's [`ThumbnailChat`] is saved in the settings table; Codex remembers
//! it in its own session, which every turn after the first resumes.

use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui_kit::{App, AppContext as _, Context, Entity, EventEmitter, Global, SharedString, Subscription};
use rushes_core::document::to_markdown;
use rushes_core::media::{self, MediaKind};
use rushes_core::{DocumentKind, Video, VideoFormat, VideoId};
use rushes_pipeline::{
    AgentConfig, AgentEvent, CodexConfig, DetailsRequest, PipelineError, ThumbnailOutcome, ThumbnailRequest,
    VideoDetails, WrittenDetails, generate_thumbnails, has_transcript, read_edit_notes, read_transcript, write_details,
};
use serde::{Deserialize, Serialize};

use super::chapters::{CHAPTERS_HEADING, MIN_CHAPTERS};
use super::rules::{DESCRIPTION_MAX_BYTES, fit_details};
use crate::ai_edit::{agent_config, command_path, sentence};
use crate::state::{AppEvent, AppState};

/// How long quitting waits for the agents to stop, beyond their kill grace period.
const QUIT_MARGIN: Duration = Duration::from_secs(1);
/// The settings key the thumbnail chats are saved under.
const CHATS_KEY: &str = "publish.thumbnail_chats";
/// What the creator reads when Codex lost the chat's session.
pub const CHAT_FORGOTTEN: &str =
    "Codex no longer remembers this chat, so your next message starts a new one. Say again what you’d like.";

/// Where the agent writing a video's details stands.
#[derive(Clone, Debug, PartialEq)]
pub enum DetailsJob {
    Writing,
    Failed(SharedString),
    /// Written into the form; what they replaced, for Undo.
    Written(VideoDetails),
}

/// Where Codex answering the creator's last message in a thumbnail chat stands.
#[derive(Clone, Debug, PartialEq)]
pub enum ThumbnailJob {
    Working {
        /// The first line of what Codex last said.
        activity: Option<SharedString>,
    },
    /// Codex couldn't answer; why. The chat says so too, and keeps saying it.
    Failed(SharedString),
}

/// Who said a message of a thumbnail chat.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatRole {
    Creator,
    Codex,
}

/// A message of a thumbnail chat.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Numbers the chat's messages in order, for their element ids.
    pub id: u32,
    pub role: ChatRole,
    pub text: String,
    /// The thumbnails Codex drew in its turn, in the order it drew them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<PathBuf>,
    /// Why Codex stopped before it finished, as the creator reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

/// A video's chat with Codex about its thumbnails.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ThumbnailChat {
    /// Codex's session, which remembers the chat; `None` until a first turn
    /// started, or once Codex lost it.
    pub session_id: Option<String>,
    pub messages: Vec<ChatMessage>,
}

impl ThumbnailChat {
    fn push(&mut self, role: ChatRole, text: String, images: Vec<PathBuf>, problem: Option<String>) {
        let id = self.messages.last().map_or(1, |last| last.id + 1);
        self.messages.push(ChatMessage { id, role, text, images, problem });
    }

    /// The thumbnails Codex drew in its last turn.
    fn last_drawn(&self) -> Vec<PathBuf> {
        let last = self.messages.iter().rev().find(|message| message.role == ChatRole::Codex);
        last.map(|message| message.images.clone()).unwrap_or_default()
    }

    /// Whether the creator's last message got no answer: Codex was cancelled, or
    /// Rushes closed while it worked.
    pub fn unanswered(&self) -> bool {
        self.messages.last().is_some_and(|last| last.role == ChatRole::Creator)
    }
}

/// A thumbnail chat as the settings keep it.
#[derive(Serialize, Deserialize)]
struct StoredChat {
    video_id: VideoId,
    #[serde(flatten)]
    chat: ThumbnailChat,
}

/// What the agent can write the details from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DetailSources {
    pub script: bool,
    pub transcript: bool,
}

impl DetailSources {
    pub fn any(self) -> bool {
        self.script || self.transcript
    }
}

/// Why a request could not start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssistError {
    VideoNotFound,
    AlreadyRunning,
    NothingToWriteFrom,
    NoMediaFolder,
    Spawn(String),
}

impl std::fmt::Display for AssistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AssistError::VideoNotFound => write!(f, "This video no longer exists."),
            AssistError::AlreadyRunning => write!(f, "This is running already. Wait for it to finish or cancel it."),
            AssistError::NothingToWriteFrom => {
                write!(f, "Write the script first, or run an AI edit to transcribe the footage.")
            }
            AssistError::NoMediaFolder => {
                write!(f, "Couldn’t create the video’s media folder. Check the media folder in Settings.")
            }
            AssistError::Spawn(_) => write!(f, "Couldn’t start the agent. Try again."),
        }
    }
}

/// Something the views follow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssistEvent {
    /// New thumbnails are in the video's thumbnails folder.
    ThumbnailsDrawn(VideoId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Work {
    Details,
    Thumbnails,
}

/// A running agent: how to stop it, and how to tell it stopped.
struct Worker {
    cancel: Arc<AtomicBool>,
    /// Signalled by the thread once the agent and its children are gone.
    stopped: mpsc::Receiver<()>,
}

enum Message<T> {
    Event(AgentEvent),
    Finished(Result<T, PipelineError>),
}

/// App-wide owner of the details and thumbnail requests, one of each per video.
pub struct PublishAssistant {
    state: Entity<AppState>,
    details: HashMap<VideoId, DetailsJob>,
    thumbnails: HashMap<VideoId, ThumbnailJob>,
    workers: HashMap<(VideoId, Work), Worker>,
    /// What the creator asked for last, so the dialogs open with it again.
    instructions: HashMap<VideoId, String>,
    chats: HashMap<VideoId, ThumbnailChat>,
    agent_env: Vec<(String, String)>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<AssistEvent> for PublishAssistant {}

struct GlobalPublishAssistant(Entity<PublishAssistant>);

impl Global for GlobalPublishAssistant {}

impl PublishAssistant {
    /// Creates the assistant and registers it globally. Call once at startup, after
    /// `AppState::set_global`.
    pub fn init(state: Entity<AppState>, cx: &mut App) -> Entity<Self> {
        let assistant = cx.new(|cx| {
            let subscriptions = vec![
                cx.subscribe(&state, |this: &mut Self, _, event: &AppEvent, cx| {
                    if *event == AppEvent::DataChanged {
                        this.forget_deleted_videos(cx);
                    }
                }),
                cx.on_app_quit(|this: &mut Self, _| {
                    this.stop_all();
                    async {}
                }),
            ];
            let mut assistant = Self {
                state,
                details: HashMap::new(),
                thumbnails: HashMap::new(),
                workers: HashMap::new(),
                instructions: HashMap::new(),
                chats: HashMap::new(),
                agent_env: Vec::new(),
                _subscriptions: subscriptions,
            };
            assistant.load_chats(cx);
            assistant
        });
        cx.set_global(GlobalPublishAssistant(assistant.clone()));
        assistant
    }

    /// The assistant, once [`Self::init`] ran.
    pub fn try_global(cx: &App) -> Option<Entity<Self>> {
        cx.try_global::<GlobalPublishAssistant>().map(|global| global.0.clone())
    }

    /// Extra environment for the agents and every tool they run.
    pub fn set_agent_env(&mut self, env: Vec<(String, String)>) {
        self.agent_env = env;
    }

    /// Whether a running request is being stopped.
    fn is_cancelling(&self, video_id: VideoId, work: Work) -> bool {
        self.workers.get(&(video_id, work)).is_some_and(|worker| worker.cancel.load(Ordering::SeqCst))
    }

    fn cancel(&mut self, video_id: VideoId, work: Work, cx: &mut Context<Self>) {
        if let Some(worker) = self.workers.get(&(video_id, work)) {
            worker.cancel.store(true, Ordering::SeqCst);
            cx.notify();
        }
    }

    fn stop_all(&mut self) {
        for worker in self.workers.values() {
            worker.cancel.store(true, Ordering::SeqCst);
        }
        let deadline = Instant::now() + AgentConfig::default().kill_grace + QUIT_MARGIN;
        for worker in self.workers.values() {
            _ = worker.stopped.recv_timeout(deadline.saturating_duration_since(Instant::now()));
        }
    }

    /// Deleting a video stops what runs for it and forgets its chat.
    fn forget_deleted_videos(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let gone: Vec<VideoId> = self
            .details
            .keys()
            .chain(self.thumbnails.keys())
            .chain(self.chats.keys())
            .copied()
            .filter(|video_id| state.video(*video_id).is_none())
            .collect();
        let chats = self.chats.len();
        for video_id in gone {
            for work in [Work::Details, Work::Thumbnails] {
                if let Some(worker) = self.workers.remove(&(video_id, work)) {
                    worker.cancel.store(true, Ordering::SeqCst);
                }
            }
            self.details.remove(&video_id);
            self.thumbnails.remove(&video_id);
            self.chats.remove(&video_id);
        }
        if self.chats.len() != chats {
            self.save_chats(cx);
        }
    }

    /// Starts `work` for `video_id` on a thread of its own and hands its events and
    /// result to `on_event` and `on_finished`, on the foreground.
    fn spawn<T: Send + 'static>(
        &mut self,
        video_id: VideoId,
        work: Work,
        run: impl FnOnce(&mut dyn FnMut(&AgentEvent), &AtomicBool) -> Result<T, PipelineError> + Send + 'static,
        on_event: fn(&mut Self, VideoId, AgentEvent, &mut Context<Self>),
        on_finished: fn(&mut Self, VideoId, Result<T, PipelineError>, &mut Context<Self>),
        cx: &mut Context<Self>,
    ) -> Result<(), AssistError> {
        let (sender, mut receiver) = unbounded();
        let (stopped_tx, stopped) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let thread_cancel = cancel.clone();
        let name = match work {
            Work::Details => "publish-details",
            Work::Thumbnails => "publish-thumbnails",
        };
        std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                let result = run(&mut |event| send(&sender, Message::Event(event.clone())), &thread_cancel);
                send(&sender, Message::Finished(result));
                // The quit handler may already have stopped waiting.
                _ = stopped_tx.send(());
            })
            .map_err(|error| {
                log_line!("rushes: couldn't start the {name} thread: {error}");
                AssistError::Spawn(error.to_string())
            })?;
        self.workers.insert((video_id, work), Worker { cancel, stopped });
        cx.spawn(async move |this, cx| {
            while let Some(message) = receiver.next().await {
                let handled = this.update(cx, |this, cx| match message {
                    Message::Event(event) => on_event(this, video_id, event, cx),
                    Message::Finished(result) => {
                        this.workers.remove(&(video_id, work));
                        on_finished(this, video_id, result, cx);
                        cx.notify();
                    }
                });
                if handled.is_err() {
                    break;
                }
            }
        })
        .detach();
        Ok(())
    }

    // Details

    pub fn details(&self, video_id: VideoId) -> Option<&DetailsJob> {
        self.details.get(&video_id)
    }

    pub fn is_cancelling_details(&self, video_id: VideoId) -> bool {
        self.is_cancelling(video_id, Work::Details)
    }

    /// The instructions the creator gave last for `video_id`.
    pub fn last_instructions(&self, video_id: VideoId) -> &str {
        self.instructions.get(&video_id).map_or("", String::as_str)
    }

    /// Has the agent write the video's title, description and tags from its script
    /// and transcript, following `instructions`, and puts them in the video's
    /// publish settings. A chapter list in the description stays.
    pub fn write_details(
        &mut self,
        video_id: VideoId,
        instructions: String,
        cx: &mut Context<Self>,
    ) -> Result<(), AssistError> {
        if self.workers.contains_key(&(video_id, Work::Details)) {
            return Err(AssistError::AlreadyRunning);
        }
        let state = self.state.read(cx);
        let video = state.video(video_id).cloned().ok_or(AssistError::VideoNotFound)?;
        let script = script_markdown(state, video_id);
        let footage_dir = media::footage_dir(&video);
        if script.trim().is_empty() && !footage_dir.as_deref().is_some_and(has_transcript) {
            return Err(AssistError::NothingToWriteFrom);
        }
        let config = agent_config(&state.settings().pipeline, self.agent_env.clone());
        let publish = &video.publish;
        let request = DetailsRequest {
            video_title: video.title.clone(),
            short: video.format == VideoFormat::Short,
            script,
            transcript: None,
            edit_notes: None,
            current: VideoDetails {
                title: publish.title.clone(),
                description: publish.description.clone(),
                tags: publish.tags.clone(),
            },
            instructions: instructions.trim().to_owned(),
        };
        // The agent has no tools; it runs where the video's files are, or anywhere.
        let cwd = video.folder.clone().filter(|folder| folder.is_dir()).unwrap_or_else(std::env::temp_dir);
        self.spawn(
            video_id,
            Work::Details,
            move |on_event, cancel| {
                let request = DetailsRequest {
                    transcript: footage_dir.as_deref().and_then(read_transcript),
                    edit_notes: footage_dir.as_deref().and_then(read_edit_notes),
                    ..request
                };
                write_details(&config, &cwd, &request, on_event, cancel)
            },
            |_, _, _, _| {},
            Self::on_details_written,
            cx,
        )?;
        self.instructions.insert(video_id, instructions);
        self.details.insert(video_id, DetailsJob::Writing);
        cx.notify();
        Ok(())
    }

    pub fn cancel_details(&mut self, video_id: VideoId, cx: &mut Context<Self>) {
        self.cancel(video_id, Work::Details, cx);
    }

    fn on_details_written(
        &mut self,
        video_id: VideoId,
        result: Result<WrittenDetails, PipelineError>,
        cx: &mut Context<Self>,
    ) {
        let written = match result {
            Ok(written) => written,
            Err(PipelineError::Cancelled) => {
                self.details.remove(&video_id);
                return;
            }
            Err(error) => {
                log_line!("rushes: writing the details of {video_id} failed: {error}");
                let problem = problem_text(&error, "the agent", "Check the agent command in Settings > AI editing.");
                self.details.insert(video_id, DetailsJob::Failed(problem.into()));
                return;
            }
        };
        let Some(mut video) = self.state.read(cx).video(video_id).cloned() else {
            self.details.remove(&video_id);
            return;
        };
        let previous = VideoDetails {
            title: video.publish.title.clone(),
            description: video.publish.description.clone(),
            tags: video.publish.tags.clone(),
        };
        let details = apply_details(&mut video, written.details);
        log_line!(
            "rushes: wrote the details of {video_id} ({} tags){}",
            details.tags.len(),
            written.cost_usd.map(|cost| format!(", ${cost:.4}")).unwrap_or_default()
        );
        self.state.update(cx, |state, cx| state.update_video(video, cx));
        self.details.insert(video_id, DetailsJob::Written(previous));
    }

    /// Puts back the title, description and tags the agent replaced.
    pub fn undo_details(&mut self, video_id: VideoId, cx: &mut Context<Self>) {
        let Some(DetailsJob::Written(previous)) = self.details.remove(&video_id) else { return };
        if let Some(mut video) = self.state.read(cx).video(video_id).cloned() {
            video.publish.title = previous.title;
            video.publish.description = previous.description;
            video.publish.tags = previous.tags;
            self.state.update(cx, |state, cx| state.update_video(video, cx));
        }
        cx.notify();
    }

    /// Forgets how the last request ended: an error, or the details to undo.
    pub fn dismiss_details(&mut self, video_id: VideoId, cx: &mut Context<Self>) {
        if matches!(self.details.get(&video_id), Some(DetailsJob::Failed(_) | DetailsJob::Written(_))) {
            self.details.remove(&video_id);
            cx.notify();
        }
    }

    // Thumbnails

    pub fn thumbnails(&self, video_id: VideoId) -> Option<&ThumbnailJob> {
        self.thumbnails.get(&video_id)
    }

    pub fn is_cancelling_thumbnails(&self, video_id: VideoId) -> bool {
        self.is_cancelling(video_id, Work::Thumbnails)
    }

    /// The video's chat with Codex about its thumbnails, `None` before its first message.
    pub fn thumbnail_chat(&self, video_id: VideoId) -> Option<&ThumbnailChat> {
        self.chats.get(&video_id)
    }

    /// Sends the creator's `message` to Codex in the video's thumbnail chat: Codex
    /// draws what it asks for into the video's thumbnails folder, or answers it;
    /// views hear [`AssistEvent::ThumbnailsDrawn`] once it did. A chat's first
    /// message shows Codex frames of the video when `use_frames` is set.
    pub fn send_thumbnail_message(
        &mut self,
        video_id: VideoId,
        message: String,
        use_frames: bool,
        cx: &mut Context<Self>,
    ) -> Result<(), AssistError> {
        if self.workers.contains_key(&(video_id, Work::Thumbnails)) {
            return Err(AssistError::AlreadyRunning);
        }
        let message = message.trim().to_owned();
        if self.state.read(cx).video(video_id).is_none() {
            return Err(AssistError::VideoNotFound);
        }
        self.state.update(cx, |state, cx| state.ensure_video_folder(video_id, cx)).ok_or(AssistError::NoMediaFolder)?;
        let state = self.state.read(cx);
        let video = state.video(video_id).cloned().ok_or(AssistError::VideoNotFound)?;
        let dir = media::thumbnails_dir(&video).ok_or(AssistError::NoMediaFolder)?;
        let settings = &state.settings().pipeline;
        let config = CodexConfig {
            command: command_path(&settings.codex_command, &CodexConfig::default().command),
            env: self.agent_env.clone(),
            ..CodexConfig::default()
        };
        let chat = self.chats.get(&video_id).cloned().unwrap_or_default();
        let request = ThumbnailRequest {
            dir,
            session_id: chat.session_id.clone(),
            video_title: video.upload_title().trim().to_owned(),
            short: video.format == VideoFormat::Short,
            script: script_markdown(state, video_id),
            message: message.clone(),
            frames_from: None,
            last_drawn: chat.last_drawn(),
        };
        let frames =
            (use_frames && chat.session_id.is_none()).then(|| (video.publish.file.clone(), media::footage_dir(&video)));
        self.spawn(
            video_id,
            Work::Thumbnails,
            move |on_event, cancel| {
                let request = ThumbnailRequest {
                    frames_from: frames.and_then(|(file, footage)| frame_source(file, footage)),
                    ..request
                };
                generate_thumbnails(&config, &request, on_event, cancel)
            },
            Self::on_thumbnail_event,
            Self::on_thumbnail_reply,
            cx,
        )?;
        self.update_chat(video_id, cx, |chat| chat.push(ChatRole::Creator, message, Vec::new(), None));
        self.thumbnails.insert(video_id, ThumbnailJob::Working { activity: None });
        cx.notify();
        Ok(())
    }

    pub fn cancel_thumbnails(&mut self, video_id: VideoId, cx: &mut Context<Self>) {
        self.cancel(video_id, Work::Thumbnails, cx);
    }

    /// Forgets the video's thumbnail chat, Codex's session with it: the next message
    /// starts a new one. The thumbnails stay. Nothing happens while Codex works.
    pub fn new_thumbnail_chat(&mut self, video_id: VideoId, cx: &mut Context<Self>) {
        if self.workers.contains_key(&(video_id, Work::Thumbnails)) {
            return;
        }
        self.thumbnails.remove(&video_id);
        if self.chats.remove(&video_id).is_some() {
            self.save_chats(cx);
        }
        cx.notify();
    }

    fn on_thumbnail_event(&mut self, video_id: VideoId, event: AgentEvent, cx: &mut Context<Self>) {
        match event {
            // Saved at once: a turn that never finishes still leaves Codex's memory of the chat.
            AgentEvent::Init { session_id, .. } if !session_id.is_empty() => {
                if self.chats.get(&video_id).is_some_and(|chat| chat.session_id.as_ref() != Some(&session_id)) {
                    self.update_chat(video_id, cx, |chat| chat.session_id = Some(session_id));
                }
            }
            AgentEvent::AssistantText { text } => {
                let Some(ThumbnailJob::Working { activity }) = self.thumbnails.get_mut(&video_id) else { return };
                let line = text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or_default();
                *activity = (!line.is_empty()).then(|| SharedString::from(line.to_owned()));
                cx.notify();
            }
            _ => {}
        }
    }

    fn on_thumbnail_reply(
        &mut self,
        video_id: VideoId,
        result: Result<ThumbnailOutcome, PipelineError>,
        cx: &mut Context<Self>,
    ) {
        if self.state.read(cx).video(video_id).is_none() {
            return;
        }
        match result {
            Ok(outcome) => {
                self.thumbnails.remove(&video_id);
                self.update_chat(video_id, cx, |chat| {
                    if outcome.session_id.is_some() {
                        chat.session_id = outcome.session_id;
                    }
                    let problem = outcome.problem.map(|problem| sentence(&problem));
                    chat.push(ChatRole::Codex, outcome.reply.trim().to_owned(), outcome.made, problem);
                });
                cx.emit(AssistEvent::ThumbnailsDrawn(video_id));
            }
            // The creator's message stays unanswered.
            Err(PipelineError::Cancelled) => {
                self.thumbnails.remove(&video_id);
            }
            Err(error) => {
                log_line!("rushes: Codex couldn't answer in the thumbnail chat of {video_id}: {error}");
                let problem = problem_text(
                    &error,
                    "Codex",
                    "Install the Codex CLI and sign in, or set the Codex command in Settings > AI editing.",
                );
                self.update_chat(video_id, cx, |chat| {
                    if matches!(error, PipelineError::SessionNotFound(_)) {
                        chat.session_id = None;
                    }
                    chat.push(ChatRole::Codex, String::new(), Vec::new(), Some(problem.clone()));
                });
                self.thumbnails.insert(video_id, ThumbnailJob::Failed(problem.into()));
            }
        }
    }

    /// Forgets that Codex couldn't answer; the chat still says so.
    pub fn dismiss_thumbnails(&mut self, video_id: VideoId, cx: &mut Context<Self>) {
        if matches!(self.thumbnails.get(&video_id), Some(ThumbnailJob::Failed(_))) {
            self.thumbnails.remove(&video_id);
            cx.notify();
        }
    }

    /// Changes the video's chat and saves the chats.
    fn update_chat(&mut self, video_id: VideoId, cx: &mut Context<Self>, change: impl FnOnce(&mut ThumbnailChat)) {
        change(self.chats.entry(video_id).or_default());
        self.save_chats(cx);
        cx.notify();
    }

    fn load_chats(&mut self, cx: &mut Context<Self>) {
        match self.state.read(cx).read_setting::<Vec<StoredChat>>(CHATS_KEY) {
            Ok(stored) => {
                let state = self.state.read(cx);
                self.chats = stored
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|stored| state.video(stored.video_id).is_some())
                    .map(|stored| (stored.video_id, stored.chat))
                    .collect();
            }
            Err(error) => log_line!("rushes: couldn't read the saved thumbnail chats: {error}"),
        }
    }

    fn save_chats(&mut self, cx: &mut Context<Self>) {
        let mut stored: Vec<StoredChat> = self
            .chats
            .iter()
            .filter(|(_, chat)| !chat.messages.is_empty() || chat.session_id.is_some())
            .map(|(video_id, chat)| StoredChat { video_id: *video_id, chat: chat.clone() })
            .collect();
        stored.sort_by_key(|stored| stored.video_id);
        // A refusal is reported by the state; the chats stay until Rushes closes.
        self.state.update(cx, |state, cx| state.save_setting(CHATS_KEY, &stored, cx));
    }
}

fn send<T>(sender: &UnboundedSender<Message<T>>, message: Message<T>) {
    // A closed channel means the app is going away; the agent is stopped by the
    // cancel flag, not by its events.
    _ = sender.unbounded_send(message);
}

/// The video's script as Markdown, with the edits not written yet.
fn script_markdown(state: &AppState, video_id: VideoId) -> String {
    state
        .documents(video_id)
        .iter()
        .find(|document| document.kind == DocumentKind::Script)
        .map(|script| to_markdown(&script.blocks))
        .unwrap_or_default()
}

/// The video to show Codex frames of: the file chosen for upload, or else the
/// first video take of the footage.
fn frame_source(file: Option<PathBuf>, footage_dir: Option<PathBuf>) -> Option<PathBuf> {
    file.filter(|file| file.is_file()).or_else(|| {
        let footage = media::list_footage(&footage_dir?).ok()?;
        footage.into_iter().find(|file| file.kind == MediaKind::Video).map(|file| file.path)
    })
}

/// Puts `written` into the video's publish settings, made to fit YouTube, keeping
/// the chapter list its description has. Returns what was put there.
fn apply_details(video: &mut Video, written: VideoDetails) -> VideoDetails {
    let publish = &mut video.publish;
    let chapters = chapter_list(&publish.description).map(str::to_owned);
    let room = chapters.as_ref().map_or(DESCRIPTION_MAX_BYTES, |list| DESCRIPTION_MAX_BYTES - list.len() - 2);
    let mut details = fit_details(written, room);
    if let Some(list) = chapters {
        details.description = keep_chapters(&details.description, &list);
    }
    publish.title.clone_from(&details.title);
    publish.description.clone_from(&details.description);
    publish.tags.clone_from(&details.tags);
    details
}

/// The chapter list in `description`: the one Rushes inserts, from its heading to the
/// first blank line, or one typed by hand, a paragraph of timed lines from 0:00.
fn chapter_list(description: &str) -> Option<&str> {
    chapter_span(description).map(|span| &description[span])
}

fn chapter_span(description: &str) -> Option<Range<usize>> {
    if let Some(start) = description.find(CHAPTERS_HEADING) {
        let end = description[start..].find("\n\n").map_or(description.len(), |offset| start + offset);
        return Some(start..start + description[start..end].trim_end().len());
    }
    let mut offset = 0;
    for paragraph in description.split("\n\n") {
        let lines: Vec<&str> = paragraph.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
        let timed = lines.len() >= MIN_CHAPTERS
            && (lines[0].starts_with("0:00") || lines[0].starts_with("00:00"))
            && lines.iter().all(|line| starts_with_time(line));
        if timed {
            let start = offset + paragraph.len() - paragraph.trim_start().len();
            return Some(start..offset + paragraph.trim_end().len());
        }
        offset += paragraph.len() + 2;
    }
    None
}

/// Whether `line` starts with a chapter time: `1:05`, `12:05` or `1:02:05`.
fn starts_with_time(line: &str) -> bool {
    let digits = |part: &str, lengths: std::ops::RangeInclusive<usize>| {
        lengths.contains(&part.len()) && part.bytes().all(|byte| byte.is_ascii_digit())
    };
    let mut parts = line.split_whitespace().next().unwrap_or_default().split(':');
    let hours_or_minutes = parts.next().is_some_and(|part| digits(part, 1..=2));
    let rest: Vec<&str> = parts.collect();
    hours_or_minutes && (1..=2).contains(&rest.len()) && rest.iter().all(|part| digits(part, 2..=2))
}

/// `description` with the chapter `list` it had before the agent wrote it: as the
/// agent kept it, in place of a list the agent changed, or after the text.
fn keep_chapters(description: &str, list: &str) -> String {
    if description.contains(list) {
        return description.to_owned();
    }
    if let Some(span) = chapter_span(description) {
        return format!("{}{list}{}", &description[..span.start], &description[span.end..]);
    }
    let body = description.trim_end();
    if body.is_empty() { list.to_owned() } else { format!("{body}\n\n{list}") }
}

/// What the creator reads when `who` ("the agent", "Codex") couldn't finish.
fn problem_text(error: &PipelineError, who: &str, fix: &str) -> String {
    match error {
        PipelineError::AgentNotFound(command) => {
            format!("Couldn’t find {who} at “{}”. {fix}", command.display())
        }
        PipelineError::Spawn { .. } => format!("Couldn’t start {who}. {fix}"),
        PipelineError::AgentFailed { message, stderr_tail, no_result, .. } => {
            let mut text = sentence(message);
            if *no_result {
                text = format!("{text} {fix}");
                // The last thing it said usually explains it (not signed in, no network).
                if let Some(line) = stderr_tail.iter().rev().find(|line| !line.trim().is_empty()) {
                    text = format!("{text} It said: “{}”", line.trim());
                }
            }
            text
        }
        PipelineError::Io(error) => format!("Couldn’t write to the video’s media folder: {error}."),
        PipelineError::SessionNotFound(_) => CHAT_FORGOTTEN.to_owned(),
        PipelineError::Cancelled | PipelineError::SkillNotFound(_) | PipelineError::FootageNotFound(_) => {
            sentence(&error.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn written_details_keep_the_chapter_list() {
        let mut video = Video::new("Sourdough");
        video.publish.description = "Old words.\n\nChapters:\n0:00 Intro\n1:10 Starter\n\nhttps://example.com".into();
        let written = VideoDetails {
            title: "Sourdough <in> 24 hours".into(),
            description: "New words.\n\nhttps://example.com".into(),
            tags: vec!["bread".into(), "#Bread".into()],
        };
        let applied = apply_details(&mut video, written);
        assert_eq!(video.publish.title, "Sourdough in 24 hours");
        assert_eq!(
            video.publish.description,
            "New words.\n\nhttps://example.com\n\nChapters:\n0:00 Intro\n1:10 Starter"
        );
        assert_eq!(video.publish.tags, ["bread"]);
        assert_eq!(applied.description, video.publish.description);
    }

    #[test]
    fn a_chapter_list_typed_by_hand_is_kept_too() {
        let list = "0:00 The experiment\n0:42 Specs and price\n1:30 Week one";
        let mut video = Video::new("Mini PC");
        video.publish.description = format!("Old words.\n\n{list}\n\nhttps://example.com");
        let written = |description: &str| VideoDetails { description: description.into(), ..VideoDetails::default() };

        apply_details(&mut video, written("New words.\n\nhttps://example.com"));
        assert_eq!(video.publish.description, format!("New words.\n\nhttps://example.com\n\n{list}"));
        // Kept as it was, it stays where the agent put it.
        apply_details(&mut video, written(&format!("Newer words.\n\n{list}")));
        assert_eq!(video.publish.description, format!("Newer words.\n\n{list}"));
        // Changed, the creator's list takes its place.
        apply_details(&mut video, written("Words.\n\n0:00 Intro\n0:40 Specs\n1:30 Week 1\n\nBye."));
        assert_eq!(video.publish.description, format!("Words.\n\n{list}\n\nBye."));
    }

    #[test]
    fn a_chapter_list_runs_to_the_first_blank_line() {
        assert_eq!(chapter_list("Hi.\n\nChapters:\n0:00 Intro\n\nBye."), Some("Chapters:\n0:00 Intro"));
        assert_eq!(chapter_list("Chapters:\n0:00 Intro\n"), Some("Chapters:\n0:00 Intro"));
        assert_eq!(chapter_list("Hi.\n\n0:00 A\n1:05 B\n1:02:05 - C\n\nBye."), Some("0:00 A\n1:05 B\n1:02:05 - C"));
        assert_eq!(chapter_list("1:00 A\n2:00 B\n3:00 C"), None, "YouTube needs the first at 0:00");
        assert_eq!(chapter_list("0:00 A\nThen B\n3:00 C"), None);
        assert_eq!(chapter_list("0:00 A\n1:00 B"), None, "too short for YouTube");
        assert_eq!(chapter_list("No list."), None);
    }

    #[test]
    fn problems_say_what_to_do() {
        let missing = PipelineError::AgentNotFound("codex".into());
        assert_eq!(problem_text(&missing, "Codex", "Install it."), "Couldn’t find Codex at “codex”. Install it.");
        let silent = PipelineError::AgentFailed {
            message: "the agent stopped without reporting a result (exit status: 1)".into(),
            stderr_tail: vec!["Not logged in".into(), String::new()],
            session_id: None,
            no_result: true,
        };
        assert_eq!(
            problem_text(&silent, "Codex", "Sign in."),
            "The agent stopped without reporting a result (exit status: 1). Sign in. It said: “Not logged in”"
        );
        let limit = PipelineError::AgentFailed {
            message: "You've hit your usage limit".into(),
            stderr_tail: Vec::new(),
            session_id: None,
            no_result: false,
        };
        assert_eq!(problem_text(&limit, "Codex", "Sign in."), "You've hit your usage limit.");
    }
}
