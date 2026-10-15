//! Uploads: one `UploadJob` per attempt, sent by a worker thread with the resumable
//! protocol, then followed until YouTube has processed the video.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, Utc};
use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui_kit::component::button::Button;
use gpui_kit::{App, AppContext as _, Context, SharedString};
use rushes_core::{FileStamp, Privacy, UploadId, UploadJob, UploadStatus, Video, VideoId, VideoStatus, YoutubeLink};
use rushes_youtube::{
    Error as YoutubeError, ProcessingStatus, RetryCause, UploadProgress, UploadStatus as YoutubeUploadStatus,
    UploadedVideo, VideoInfo, VideoMetadata, YoutubeClient,
};

use super::rules::{self, AccountAccess, BlockingReason};
use super::service::{YoutubeEvent, YoutubeService, describe_error, report_problem};
use super::transfer::{PROGRESS_INTERVAL, Throttle, Transfer};
use crate::state::Problem;
use crate::ui::{notification, push_notification};

/// Status message of jobs a previous run left mid-transfer with a session to resume.
pub const INTERRUPTED_RESUMABLE: &str = "Interrupted — you can resume";
/// Status message of jobs a previous run left before YouTube opened a session.
pub const INTERRUPTED: &str = "Interrupted before the upload started";
const PROCESSING_POLL_INTERVAL: Duration = Duration::from_secs(5);
/// About two minutes of polling; long videos keep processing after that.
const PROCESSING_POLLS: usize = 24;
/// Uploading progress is written to the database in this many steps, so the
/// videos table and an interrupted job show roughly how far it got.
const PERSISTED_STEPS: f32 = 10.0;
/// Why a video's upload file can't be changed now, wherever it is chosen.
pub const UPLOAD_FILE_LOCKED: &str = "The upload file can’t change while the video uploads. Cancel the upload first.";
/// Why a video's account can't be changed now.
pub const UPLOAD_ACCOUNT_LOCKED: &str = "The account can’t change while the video uploads. Cancel the upload first.";
/// What a stopped upload says once YouTube no longer takes the rest of its file.
const SESSION_GONE: &str = "YouTube can’t continue this upload. Upload the video again to send it from the beginning.";

/// Why an upload did not start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UploadError {
    Blocked(Vec<BlockingReason>),
    UnknownVideo,
    UnknownUpload,
    AlreadyUploading,
    NotResumable,
    /// The upload stopped sending a file that is no longer the one to upload.
    Stale(Retired),
    /// The job couldn't be stored; the cause is in the log.
    Storage,
}

impl fmt::Display for UploadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UploadError::Blocked(reasons) => {
                let reasons: Vec<String> = reasons.iter().map(ToString::to_string).collect();
                write!(f, "{}", reasons.join(" "))
            }
            UploadError::UnknownVideo => write!(f, "This video no longer exists."),
            UploadError::UnknownUpload => write!(f, "This upload no longer exists."),
            UploadError::AlreadyUploading => write!(f, "This video is already uploading."),
            UploadError::NotResumable => write!(f, "This upload can’t be resumed. Upload the video again."),
            UploadError::Stale(Retired::OtherFile) => {
                write!(
                    f,
                    "Another file was chosen for this video since the upload stopped. Upload the video to send it."
                )
            }
            UploadError::Stale(Retired::FileChanged) => {
                write!(
                    f,
                    "The file changed since this upload started. Upload it to YouTube again to send the new version."
                )
            }
            UploadError::Stale(Retired::Replaced) => write!(f, "A newer upload of this video took its place."),
            UploadError::Storage => write!(
                f,
                "Couldn’t save the upload. Check that the disk isn’t full and the data folder is writable, then try again."
            ),
        }
    }
}

impl std::error::Error for UploadError {}

/// What YouTube said about the video after the bytes arrived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessingOutcome {
    Processing,
    Ready,
    /// Polling stopped before YouTube finished; it carries on without the app.
    StillProcessing,
}

/// Why a stopped upload no longer offers Resume: its YouTube session would send
/// something other than the video's file as it is now, or a second copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retired {
    /// Another file was chosen for the video since.
    OtherFile,
    /// The file changed on disk since the upload started.
    FileChanged,
    /// A newer upload of the video took its place.
    Replaced,
}

impl Retired {
    /// What happened, as the stopped job's status says it.
    pub fn message(self) -> &'static str {
        match self {
            Retired::OtherFile => "Another file was chosen for the video since, so this upload can’t be resumed.",
            Retired::FileChanged => "The file changed since this upload started, so it can’t be resumed.",
            Retired::Replaced => "A newer upload of the video took its place.",
        }
    }
}

/// The worker is waiting to try again after a passing failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Retry {
    /// 1-based, up to `max_retries`.
    pub attempt: u32,
    pub max_retries: u32,
    pub cause: RetryCause,
}

impl Retry {
    /// What went wrong, as a short status title.
    pub fn headline(&self) -> &'static str {
        match self.cause {
            RetryCause::Network => "Can’t reach YouTube",
            RetryCause::Server => "YouTube isn’t responding",
            RetryCause::RateLimited => "YouTube is busy",
        }
    }

    /// `Retrying (2 of 5)…`
    pub fn detail(&self) -> String {
        format!("Retrying ({} of {})…", self.attempt, self.max_retries)
    }
}

/// An upload whose worker thread is running.
pub(super) struct ActiveUpload {
    cancel: Arc<AtomicBool>,
    client: Arc<YoutubeClient>,
    transfer: Transfer,
    /// Most bytes sent so far; progress past it after a retry means the retry worked.
    high_water: u64,
    retry: Option<Retrying>,
    /// Continues an earlier session rather than opening one.
    resumed: bool,
    /// The upload title, for a notice if the video is deleted while it uploads.
    title: String,
}

/// A retry in progress and what it is measured against.
#[derive(Clone, Copy, Debug)]
struct Retrying {
    retry: Retry,
    /// The high-water mark when the retry began; getting past it means it worked.
    mark: u64,
    /// What YouTube holds: the worker steps back to it after a failed request.
    confirmed: u64,
}

impl ActiveUpload {
    pub(super) fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

enum Request {
    Upload(Box<VideoMetadata>),
    Resume(String),
}

enum UploadMessage {
    Started(String),
    Progress {
        at: Instant,
        sent: u64,
        total: u64,
    },
    /// `confirmed` is what YouTube holds as the retry begins.
    Retrying {
        retry: Retry,
        confirmed: u64,
    },
    Finished(Result<UploadedVideo, YoutubeError>),
}

impl YoutubeService {
    /// Every upload job, newest first.
    pub fn jobs(&self) -> &[UploadJob] {
        &self.jobs
    }

    pub fn job(&self, id: UploadId) -> Option<&UploadJob> {
        self.jobs.iter().find(|job| job.id == id)
    }

    /// The most recent upload of `video_id`.
    pub fn latest_job(&self, video_id: VideoId) -> Option<&UploadJob> {
        self.jobs.iter().find(|job| job.video_id == video_id)
    }

    /// Live byte counts of a running upload.
    pub fn transfer(&self, id: UploadId) -> Option<&Transfer> {
        self.active.get(&id).map(|upload| &upload.transfer)
    }

    /// Whether the upload's worker is still sending (or stopping after a cancel).
    pub fn is_active(&self, id: UploadId) -> bool {
        self.active.contains_key(&id)
    }

    /// The passing failure a running upload is retrying after, until it gets further.
    pub fn retry(&self, id: UploadId) -> Option<Retry> {
        self.active.get(&id).and_then(|upload| upload.retry).map(|retrying| retrying.retry)
    }

    /// How far a running upload got, `0.0..=1.0`. While it retries after a failure,
    /// only what YouTube confirmed counts: bytes in flight may be lost again.
    pub fn progress(&self, id: UploadId) -> Option<f32> {
        let upload = self.active.get(&id)?;
        let total = upload.transfer.total();
        Some(match upload.retry {
            Some(retrying) if total > 0 => (retrying.confirmed as f64 / total as f64).clamp(0.0, 1.0) as f32,
            _ => upload.transfer.fraction(),
        })
    }

    pub fn is_cancelling(&self, id: UploadId) -> bool {
        self.active.get(&id).is_some_and(|upload| upload.cancel.load(Ordering::Relaxed))
    }

    pub fn active_upload_count(&self) -> usize {
        self.active.len()
    }

    /// Why the custom thumbnail of a finished upload wasn't set.
    pub fn thumbnail_problem(&self, id: UploadId) -> Option<&str> {
        self.thumbnail_problems.get(&id).map(String::as_str)
    }

    /// Processing state of a finished upload, while this run of the app follows it.
    pub fn processing(&self, id: UploadId) -> Option<ProcessingOutcome> {
        self.outcomes.get(&id).copied()
    }

    /// A stopped upload whose YouTube session can take the rest of the file.
    pub fn is_resumable(&self, job: &UploadJob) -> bool {
        job.session_url.is_some()
            && !self.is_active(job.id)
            && matches!(job.status, UploadStatus::Failed { .. } | UploadStatus::Cancelled)
    }

    /// Why a stopped upload stopped offering Resume during this run of the app.
    pub fn retired(&self, id: UploadId) -> Option<Retired> {
        self.retired.get(&id).copied()
    }

    /// Stops offering Resume for uploads whose video has another file now, and
    /// checks in the background that the files of the others haven't changed: a
    /// session continues the bytes it was opened for, whatever the file holds now.
    pub fn check_stopped_uploads(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let mut other_file = Vec::new();
        let mut stamped = Vec::new();
        for job in self.jobs.iter().filter(|job| self.is_resumable(job)) {
            let chosen = state.video(job.video_id).and_then(|video| video.publish.file.as_ref());
            if chosen != Some(&job.file) {
                other_file.push(job.id);
            } else if let Some(stamp) = job.file_stamp {
                stamped.push((job.id, job.file.clone(), stamp));
            }
        }
        for id in other_file {
            self.retire(id, Retired::OtherFile, cx);
        }
        if stamped.is_empty() || self.checking_files {
            return;
        }
        self.checking_files = true;
        cx.spawn(async move |this, cx| {
            let changed: Vec<UploadId> = cx
                .background_spawn(async move {
                    // A missing file is not a changed one: it may come back, and
                    // resuming says it is missing.
                    stamped
                        .into_iter()
                        .filter(|(_, file, stamp)| file_stamp(file).is_some_and(|now| now != *stamp))
                        .map(|(id, ..)| id)
                        .collect()
                })
                .await;
            this.update(cx, |this, cx| {
                this.checking_files = false;
                for id in changed {
                    // It may have been resumed or retired meanwhile.
                    if this.job(id).is_some_and(|job| this.is_resumable(job)) {
                        this.retire(id, Retired::FileChanged, cx);
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Drops the session of a stopped upload, so it no longer offers Resume.
    fn retire(&mut self, id: UploadId, reason: Retired, cx: &mut Context<Self>) {
        self.retired.insert(id, reason);
        self.update_job(id, cx, |job| {
            job.session_url = None;
            // "Interrupted — you can resume" and the like no longer hold.
            if let UploadStatus::Failed { error } = &mut job.status {
                *error = reason.message().to_owned();
            }
        });
    }

    /// What stops `video_id` from being uploaded now; empty when it is ready.
    pub fn blocking_reasons(&self, video_id: VideoId, cx: &App) -> Vec<BlockingReason> {
        let Some(video) = self.state.read(cx).video(video_id) else { return Vec::new() };
        let file_exists = video.publish.file.as_deref().is_some_and(Path::is_file);
        let access = self.account_access_for(video.publish.channel.as_ref().map(|channel| channel.id.as_str()));
        rules::blocking_reasons(video, access, file_exists, Utc::now())
    }

    /// What uploads need from the account `key` before they can start.
    fn account_blocker(&self, key: Option<&str>, video: &Video) -> BlockingReason {
        match self.account_access_for(key) {
            AccountAccess::NeedsSignIn => BlockingReason::NeedsSignIn,
            AccountAccess::SigningIn => BlockingReason::SigningIn,
            AccountAccess::Gone => BlockingReason::AccountGone(
                video.publish.channel.as_ref().map_or_else(|| "The chosen account".to_owned(), |c| c.name.clone()),
            ),
            _ => BlockingReason::NotConnected,
        }
    }

    /// Validates `video_id`, records a new upload job and starts sending the file.
    pub fn start_upload(&mut self, video_id: VideoId, cx: &mut Context<Self>) -> Result<UploadId, UploadError> {
        let video = self.state.read(cx).video(video_id).cloned().ok_or(UploadError::UnknownVideo)?;
        if self.active.keys().any(|id| self.job(*id).is_some_and(|job| job.video_id == video_id)) {
            return Err(UploadError::AlreadyUploading);
        }
        let reasons = self.blocking_reasons(video_id, cx);
        if !reasons.is_empty() {
            return Err(UploadError::Blocked(reasons));
        }
        let channel = self.channel_for(&video);
        let client = channel.as_deref().and_then(|key| self.client_for(key));
        let (Some(channel), Some(client), Some(file)) = (channel, client, video.publish.file.clone()) else {
            let key = video.publish.channel.as_ref().map(|channel| channel.id.as_str());
            return Err(UploadError::Blocked(vec![self.account_blocker(key, &video)]));
        };
        let stamp = file_stamp(&file).ok_or(UploadError::Blocked(vec![BlockingReason::FileMissing]))?;
        let now = Utc::now();
        let job = UploadJob {
            id: UploadId::new(),
            video_id,
            file: file.clone(),
            file_stamp: Some(stamp),
            status: UploadStatus::Pending,
            session_url: None,
            channel: Some(channel.clone()),
            created_at: now,
            updated_at: now,
        };
        // A refusal is logged and reported by the state.
        if !self.state.update(cx, |state, cx| state.insert_upload(job.clone(), cx)) {
            return Err(UploadError::Storage);
        }
        self.jobs.insert(0, job.clone());
        // The video stays with the account it went to, whatever the default becomes.
        if video.publish.channel.is_none() {
            self.choose_channel(video_id, &channel, cx);
        }
        // Resuming an earlier, stopped upload as well would put a second copy on YouTube.
        let stopped: Vec<UploadId> = self
            .jobs
            .iter()
            .filter(|earlier| earlier.video_id == video_id && self.is_resumable(earlier))
            .map(|earlier| earlier.id)
            .collect();
        for id in stopped {
            self.retire(id, Retired::Replaced, cx);
        }
        self.jobs_changed(cx);
        self.spawn_transfer(job.id, client, file, stamp.size, Request::Upload(Box::new(rules::metadata(&video))), cx);
        Ok(job.id)
    }

    /// Continues a stopped upload from wherever the YouTube session got to.
    pub fn resume_upload(&mut self, id: UploadId, cx: &mut Context<Self>) -> Result<(), UploadError> {
        let job = self.job(id).cloned().ok_or(UploadError::UnknownUpload)?;
        if self.is_uploading_any(&[job.video_id]) {
            return Err(UploadError::AlreadyUploading);
        }
        let Some(session_url) = job.session_url.clone().filter(|_| self.is_resumable(&job)) else {
            return Err(UploadError::NotResumable);
        };
        let video = self.state.read(cx).video(job.video_id).cloned().ok_or(UploadError::UnknownVideo)?;
        // The session belongs to the account that opened it; jobs from before
        // accounts were recorded went to the video's.
        let channel = job.channel.clone().or_else(|| self.channel_for(&video));
        let Some((channel, client)) = channel.and_then(|key| Some((key.clone(), self.client_for(&key)?))) else {
            let key = job.channel.as_deref().or(video.publish.channel.as_ref().map(|channel| channel.id.as_str()));
            return Err(UploadError::Blocked(vec![self.account_blocker(key, &video)]));
        };
        // The session takes the rest of the bytes it was opened for, and nothing else.
        let chosen = video.publish.file.clone();
        if chosen.as_ref() != Some(&job.file) {
            self.retire(id, Retired::OtherFile, cx);
            return Err(UploadError::Stale(Retired::OtherFile));
        }
        let stamp = file_stamp(&job.file).ok_or(UploadError::Blocked(vec![BlockingReason::FileMissing]))?;
        if job.file_stamp.is_some_and(|recorded| recorded != stamp) {
            self.retire(id, Retired::FileChanged, cx);
            return Err(UploadError::Stale(Retired::FileChanged));
        }
        let total = stamp.size;
        if self.spawn_transfer(id, client, job.file, total, Request::Resume(session_url), cx) {
            self.update_job(id, cx, |job| {
                job.status = UploadStatus::Uploading { sent: 0, total };
                job.channel = Some(channel);
            });
        }
        Ok(())
    }

    /// Stops a running upload. The job keeps its session so it can be resumed.
    pub fn cancel_upload(&mut self, id: UploadId, cx: &mut Context<Self>) {
        if let Some(upload) = self.active.get(&id) {
            upload.cancel();
            cx.notify();
        }
    }

    /// Starts the worker thread; `false` (with the job marked failed) when it couldn't start.
    fn spawn_transfer(
        &mut self,
        id: UploadId,
        client: Arc<YoutubeClient>,
        file: PathBuf,
        total: u64,
        request: Request,
        cx: &mut Context<Self>,
    ) -> bool {
        let cancel = Arc::new(AtomicBool::new(false));
        let (sender, mut messages) = mpsc::unbounded();
        let title = self.video_title(id, cx);
        let resumed = matches!(request, Request::Resume(_));
        let worker = (client.clone(), cancel.clone());
        let spawned = thread::Builder::new()
            .name("youtube-upload".into())
            .spawn(move || transfer_file(&worker.0, &file, &request, &worker.1, &sender));
        if let Err(error) = spawned {
            log_line!("rushes: couldn't start the upload thread: {error}");
            self.update_job(id, cx, |job| {
                job.status = UploadStatus::Failed {
                    error: "Couldn’t start the upload. Quit Rushes and open it again, then retry.".to_owned(),
                }
            });
            return false;
        }
        self.active.insert(
            id,
            ActiveUpload { cancel, client, transfer: Transfer::new(total), high_water: 0, retry: None, resumed, title },
        );
        cx.spawn(async move |this, cx| {
            while let Some(message) = messages.next().await {
                if this.update(cx, |this, cx| this.on_upload_message(id, message, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        cx.emit(YoutubeEvent::UploadsChanged);
        cx.notify();
        true
    }

    fn on_upload_message(&mut self, id: UploadId, message: UploadMessage, cx: &mut Context<Self>) {
        match message {
            UploadMessage::Started(session_url) => {
                if let Some(upload) = self.active.get_mut(&id) {
                    // The session opened, so whatever was retried before worked.
                    upload.retry = None;
                }
                let (sent, total) = self.transfer(id).map_or((0, 0), |transfer| (transfer.sent(), transfer.total()));
                self.update_job(id, cx, |job| {
                    job.session_url = Some(session_url);
                    job.status = UploadStatus::Uploading { sent, total };
                });
            }
            UploadMessage::Progress { at, sent, total } => {
                let Some(upload) = self.active.get_mut(&id) else { return };
                let step = |transfer: &Transfer| (transfer.fraction() * PERSISTED_STEPS).floor();
                let before = step(&upload.transfer);
                // After a failed request the worker steps back to what YouTube holds.
                if let Some(retrying) = upload.retry.as_mut()
                    && sent < upload.transfer.sent()
                {
                    retrying.confirmed = sent;
                }
                upload.transfer.record(at, sent, total);
                upload.high_water = upload.high_water.max(sent);
                if upload.retry.is_some_and(|retrying| sent > retrying.mark) {
                    upload.retry = None;
                }
                if step(&upload.transfer) > before && sent < total {
                    self.update_job(id, cx, |job| job.status = UploadStatus::Uploading { sent, total });
                }
                cx.notify();
            }
            UploadMessage::Retrying { retry, confirmed } => {
                let Some(upload) = self.active.get_mut(&id) else { return };
                let mark = upload.retry.map_or(upload.high_water, |retrying| retrying.mark);
                upload.retry = Some(Retrying { retry, mark, confirmed });
                cx.notify();
            }
            UploadMessage::Finished(result) => {
                let Some(upload) = self.active.remove(&id) else { return };
                if self.job(id).is_none() {
                    // The video was deleted while it uploaded.
                    self.on_orphan_finished(upload, result.ok(), cx);
                    cx.notify();
                    return;
                }
                match result {
                    Ok(video) => self.on_uploaded(id, video, upload.client, cx),
                    Err(YoutubeError::Cancelled { session_url }) => self.update_job(id, cx, |job| {
                        job.status = UploadStatus::Cancelled;
                        job.session_url = session_url.or(job.session_url.take());
                    }),
                    Err(error) => self.on_upload_failed(id, &error, upload.resumed, cx),
                }
            }
        }
    }

    /// Deleting a video cancels its upload, but the last chunk may already have been
    /// on its way: then the video is on YouTube, and the creator should know.
    fn on_orphan_finished(&mut self, upload: ActiveUpload, uploaded: Option<UploadedVideo>, cx: &mut Context<Self>) {
        let Some(uploaded) = uploaded else { return };
        let (title, url): (String, SharedString) = (upload.title, uploaded.url.into());
        push_notification(cx, move |cx| {
            notification::warning(
                "The upload finished before the video was deleted, so it is on your channel. \
                 Remove it in YouTube Studio if you don’t want it there.",
                cx,
            )
            .title(format!("“{title}” reached YouTube"))
            .action(move |_, _, _| {
                let url = url.clone();
                Button::new("open-orphan-on-youtube")
                    .label("Open on YouTube")
                    .on_click(move |_, _, cx| cx.open_url(&url))
            })
        });
    }

    /// Stops the uploads of deleted videos and forgets their jobs: the store deleted
    /// them with the video.
    pub(super) fn forget_deleted_videos(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let gone: Vec<UploadId> =
            self.jobs.iter().filter(|job| state.video(job.video_id).is_none()).map(|job| job.id).collect();
        if gone.is_empty() {
            return;
        }
        for id in &gone {
            if let Some(upload) = self.active.get(id) {
                upload.cancel();
            }
            self.outcomes.remove(id);
            self.thumbnail_problems.remove(id);
            self.retired.remove(id);
        }
        self.jobs.retain(|job| !gone.contains(&job.id));
        cx.emit(YoutubeEvent::UploadsChanged);
        cx.notify();
    }

    /// Whether any of `video_ids` is uploading right now.
    pub fn is_uploading_any(&self, video_ids: &[VideoId]) -> bool {
        self.active.keys().any(|id| self.job(*id).is_some_and(|job| video_ids.contains(&job.video_id)))
    }

    fn on_upload_failed(&mut self, id: UploadId, error: &YoutubeError, resumed: bool, cx: &mut Context<Self>) {
        // A session YouTube no longer has (404/410), one that refuses the bytes sent
        // (400, such as a file that changed size), or one the current connection
        // can't use (another backend's after a switch) can't take the rest of the file.
        let had_session = self.job(id).is_some_and(|job| job.session_url.is_some());
        let session_gone = had_session
            && match error {
                YoutubeError::SessionExpired => true,
                YoutubeError::Api { status, .. } => *status == 400,
                YoutubeError::InvalidInput(_) => resumed,
                _ => false,
            };
        let message = if session_gone {
            log_line!("rushes: youtube: {error}");
            SESSION_GONE.to_owned()
        } else {
            describe_error(error)
        };
        self.update_job(id, cx, |job| {
            job.status = UploadStatus::Failed { error: message.clone() };
            if session_gone {
                job.session_url = None;
            }
        });
        if error.needs_sign_in()
            && let Some(channel) = self.job(id).and_then(|job| job.channel.clone())
        {
            self.require_sign_in(&channel, cx);
        }
        let title = self.video_title(id, cx);
        let headline = if resumed { "Couldn’t resume" } else { "Couldn’t upload" };
        push_notification(cx, move |cx| notification::error(message, cx).title(format!("{headline} “{title}”")));
    }

    fn on_uploaded(
        &mut self,
        id: UploadId,
        uploaded: UploadedVideo,
        client: Arc<YoutubeClient>,
        cx: &mut Context<Self>,
    ) {
        let youtube_id = uploaded.id.clone();
        self.update_job(id, cx, |job| job.status = UploadStatus::Processing { video_id: youtube_id.clone() });
        let Some(video_id) = self.job(id).map(|job| job.video_id) else { return };

        let thumbnail = self.state.update(cx, |state, cx| {
            let mut video = state.video(video_id)?.clone();
            let privacy = rules::core_privacy(uploaded.privacy);
            video.youtube = Some(YoutubeLink {
                video_id: uploaded.id.clone(),
                url: uploaded.url.clone(),
                channel_id: uploaded.channel_id.clone(),
                privacy,
                uploaded_at: Utc::now(),
            });
            if let Some(publish_at) = video.publish.publish_at {
                video.status = VideoStatus::Scheduled;
                // YouTube publishes it then, whatever the planner said before.
                video.publish_date = Some(publish_at.with_timezone(&Local).date_naive());
            } else if privacy != Privacy::Private {
                video.status = VideoStatus::Published;
            }
            let thumbnail = video.publish.thumbnail.clone();
            state.update_video(video, cx);
            thumbnail
        });

        let title = self.video_title(id, cx);
        let url: SharedString = uploaded.url.clone().into();
        push_notification(cx, move |cx| {
            notification::success(format!("“{title}”"), cx)
                .title("Uploaded to YouTube")
                .action(move |_, _, _| {
                    let url = url.clone();
                    Button::new("open-upload-on-youtube")
                        .label("Open on YouTube")
                        .on_click(move |_, _, cx| cx.open_url(&url))
                })
                // The Upload card and the Uploads page keep the link; the toast need not stay.
                .autohide(true)
        });

        if let Some(thumbnail) = thumbnail {
            self.set_thumbnail(id, client.clone(), youtube_id.clone(), thumbnail, cx);
        }
        self.follow_processing(id, client, youtube_id, cx);
    }

    /// Custom thumbnails need a verified channel; failing to set one does not fail the upload.
    fn set_thumbnail(
        &mut self,
        id: UploadId,
        client: Arc<YoutubeClient>,
        youtube_id: String,
        thumbnail: PathBuf,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { client.set_thumbnail(&youtube_id, &thumbnail) }).await;
            if let Err(error) = result {
                this.update(cx, |this, cx| {
                    let title = this.video_title(id, cx);
                    let message = describe_error(&error);
                    // The toast goes away; the Upload card and the Uploads page keep the reason.
                    this.thumbnail_problems.insert(id, message.clone());
                    cx.notify();
                    push_notification(cx, move |cx| {
                        notification::warning(message, cx).title(format!("The thumbnail of “{title}” wasn’t set"))
                    });
                })
                .ok();
            }
        })
        .detach();
    }

    /// Asks YouTube about the video every few seconds until it is processed.
    fn follow_processing(
        &mut self,
        id: UploadId,
        client: Arc<YoutubeClient>,
        youtube_id: String,
        cx: &mut Context<Self>,
    ) {
        self.outcomes.insert(id, ProcessingOutcome::Processing);
        cx.spawn(async move |this, cx| {
            for _ in 0..PROCESSING_POLLS {
                cx.background_executor().timer(PROCESSING_POLL_INTERVAL).await;
                let (client, youtube_id) = (client.clone(), youtube_id.clone());
                // Errors are retried at the next poll: processing goes on without the app.
                let state = cx.background_spawn(async move { client.get_video(&youtube_id) }).await;
                let Ok(Some(info)) = state else { continue };
                let outcome = processed(&info);
                this.update(cx, |this, cx| {
                    this.remote.insert(info.id.clone(), info);
                    cx.notify();
                })
                .ok();
                if let Some(outcome) = outcome {
                    this.update(cx, |this, cx| this.finish_processing(id, outcome, cx)).ok();
                    return;
                }
            }
            this.update(cx, |this, cx| this.finish_processing(id, Ok(ProcessingOutcome::StillProcessing), cx)).ok();
        })
        .detach();
    }

    fn finish_processing(&mut self, id: UploadId, outcome: Result<ProcessingOutcome, String>, cx: &mut Context<Self>) {
        match outcome {
            Ok(outcome) => {
                self.outcomes.insert(id, outcome);
                self.update_job(id, cx, |job| {
                    if let UploadStatus::Processing { video_id } = &job.status {
                        job.status = UploadStatus::Done { video_id: video_id.clone() };
                    }
                });
                // The channel's video count may have changed.
                if let Some(channel) = self.job(id).and_then(|job| job.channel.clone()) {
                    self.refresh_account(&channel, cx);
                }
            }
            Err(error) => {
                self.outcomes.remove(&id);
                self.update_job(id, cx, |job| job.status = UploadStatus::Failed { error });
            }
        }
    }

    /// Jobs a previous run left mid-transfer become failed (resumable when YouTube
    /// had opened a session); jobs left processing are done, the bytes arrived.
    pub(super) fn recover_interrupted_uploads(&mut self, cx: &mut Context<Self>) {
        let jobs = match self.state.read(cx).read_uploads() {
            Ok(jobs) => jobs,
            Err(error) => {
                return report_problem(&self.state, Problem::store("Couldn’t load the uploads", &error), cx);
            }
        };
        self.jobs = jobs;
        let mut changed = false;
        for job in &mut self.jobs {
            job.status = match &job.status {
                UploadStatus::Pending | UploadStatus::Uploading { .. } => UploadStatus::Failed {
                    error: if job.session_url.is_some() { INTERRUPTED_RESUMABLE } else { INTERRUPTED }.to_owned(),
                },
                UploadStatus::Processing { video_id } => UploadStatus::Done { video_id: video_id.clone() },
                _ => continue,
            };
            job.updated_at = Utc::now();
            let job = job.clone();
            self.state.update(cx, |state, cx| state.save_upload(job, cx));
            changed = true;
        }
        if changed {
            self.state.update(cx, |state, cx| state.reload(cx));
        }
    }

    /// Applies `change` to the job, saves it and tells everyone. The state
    /// saves it through its write queue, so a lock on the database holds up
    /// neither the upload nor the UI.
    pub(super) fn update_job(&mut self, id: UploadId, cx: &mut Context<Self>, change: impl FnOnce(&mut UploadJob)) {
        let Some(job) = self.jobs.iter_mut().find(|job| job.id == id) else { return };
        change(job);
        job.updated_at = Utc::now();
        let job = job.clone();
        self.state.update(cx, |state, cx| state.save_upload(job, cx));
        self.jobs_changed(cx);
    }

    fn jobs_changed(&mut self, cx: &mut Context<Self>) {
        // Rows carry the latest upload status.
        self.state.update(cx, |state, cx| state.reload(cx));
        cx.emit(YoutubeEvent::UploadsChanged);
        cx.notify();
    }

    fn video_title(&self, id: UploadId, cx: &App) -> String {
        self.job(id)
            .and_then(|job| self.state.read(cx).video(job.video_id))
            .map_or_else(|| "Untitled".to_owned(), |video| video.upload_title().to_owned())
    }
}

/// Upload worker: sends the file and reports progress at most every
/// [`PROGRESS_INTERVAL`], except a step back to what YouTube holds after a
/// failed request, which is always reported, as is the end.
fn transfer_file(
    client: &YoutubeClient,
    file: &Path,
    request: &Request,
    cancel: &AtomicBool,
    messages: &mpsc::UnboundedSender<UploadMessage>,
) {
    // The receiver is gone only when the app is shutting down.
    let send = |message| {
        let _ = messages.unbounded_send(message);
    };
    let mut throttle = Throttle::new(PROGRESS_INTERVAL);
    // The worker's latest count; before it reports a retry it steps back to
    // what YouTube holds, which the bar must not stay ahead of.
    let mut latest = 0;
    let mut progress = |event: UploadProgress| match event {
        UploadProgress::Started { session_url } => send(UploadMessage::Started(session_url)),
        UploadProgress::Bytes { sent, total } => {
            let at = Instant::now();
            let stepped_back = sent < latest;
            latest = sent;
            if sent == total || stepped_back || throttle.ready(at) {
                send(UploadMessage::Progress { at, sent, total });
            }
        }
        UploadProgress::Retrying { attempt, max_retries, cause, .. } => {
            send(UploadMessage::Retrying { retry: Retry { attempt, max_retries, cause }, confirmed: latest })
        }
        UploadProgress::Finished => {}
    };
    let result = match request {
        Request::Upload(metadata) => client.upload_video(file, metadata, &mut progress, cancel),
        Request::Resume(session_url) => client.resume_upload(session_url, file, &mut progress, cancel),
    };
    send(UploadMessage::Finished(result));
}

/// `Some` once YouTube is done with the video: ready, or why it failed.
fn processed(info: &VideoInfo) -> Option<Result<ProcessingOutcome, String>> {
    let failed = matches!(info.processing_status, Some(ProcessingStatus::Failed | ProcessingStatus::Terminated))
        || matches!(
            info.upload_status,
            YoutubeUploadStatus::Failed | YoutubeUploadStatus::Rejected | YoutubeUploadStatus::Deleted
        );
    if failed {
        return Some(Err("YouTube couldn’t process the video. Check it in YouTube Studio.".to_owned()));
    }
    let ready = info.processing_status == Some(ProcessingStatus::Succeeded)
        || info.upload_status == YoutubeUploadStatus::Processed;
    ready.then_some(Ok(ProcessingOutcome::Ready))
}

/// Size and modification time of the file at `path`; `None` when there is none.
fn file_stamp(path: &Path) -> Option<FileStamp> {
    let metadata = fs::metadata(path).ok().filter(|metadata| metadata.is_file())?;
    Some(FileStamp { size: metadata.len(), modified: metadata.modified().ok().map(DateTime::<Utc>::from) })
}

#[cfg(test)]
mod tests {
    use rushes_youtube::Privacy as YoutubePrivacy;

    use super::*;

    fn info(upload_status: YoutubeUploadStatus, processing_status: Option<ProcessingStatus>) -> VideoInfo {
        VideoInfo {
            id: "abc".into(),
            title: "T".into(),
            privacy: YoutubePrivacy::Private,
            publish_at: None,
            published_at: None,
            upload_status,
            processing_status,
            url: "https://www.youtube.com/watch?v=abc".into(),
            thumbnail: None,
        }
    }

    #[test]
    fn processing_finishes_on_success_or_failure() {
        assert_eq!(processed(&info(YoutubeUploadStatus::Uploaded, Some(ProcessingStatus::Processing))), None);
        assert_eq!(processed(&info(YoutubeUploadStatus::Uploaded, None)), None);
        assert_eq!(
            processed(&info(YoutubeUploadStatus::Processed, Some(ProcessingStatus::Succeeded))),
            Some(Ok(ProcessingOutcome::Ready))
        );
        assert_eq!(processed(&info(YoutubeUploadStatus::Processed, None)), Some(Ok(ProcessingOutcome::Ready)));
        assert!(matches!(processed(&info(YoutubeUploadStatus::Rejected, None)), Some(Err(_))));
        assert!(matches!(
            processed(&info(YoutubeUploadStatus::Uploaded, Some(ProcessingStatus::Terminated))),
            Some(Err(_))
        ));
    }

    #[test]
    fn upload_errors_read_as_sentences() {
        let blocked = UploadError::Blocked(vec![BlockingReason::NotConnected, BlockingReason::EmptyTitle]);
        assert_eq!(blocked.to_string(), "Connect a YouTube account. Add a title.");
        assert_eq!(UploadError::NotResumable.to_string(), "This upload can’t be resumed. Upload the video again.");
        assert_eq!(
            UploadError::Stale(Retired::FileChanged).to_string(),
            "The file changed since this upload started. Upload it to YouTube again to send the new version."
        );
    }
}
