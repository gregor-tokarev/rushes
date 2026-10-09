//! Uploads: one `UploadJob` per attempt, sent by a worker thread with the resumable
//! protocol, then followed until YouTube has processed the video.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use chrono::Utc;
use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui_kit::component::button::Button;
use gpui_kit::component::notification::Notification;
use gpui_kit::{App, AppContext as _, Context, SharedString};
use videocrm_core::{Privacy, StoreError, UploadId, UploadJob, UploadStatus, VideoId, VideoStatus, YoutubeLink};
use videocrm_youtube::{
    Error as YoutubeError, ProcessingStatus, RetryCause, UploadProgress, UploadStatus as YoutubeUploadStatus,
    UploadedVideo, VideoInfo, VideoMetadata, YoutubeClient,
};

use super::rules::{self, BlockingReason};
use super::service::{YoutubeEvent, YoutubeService, describe_error, lock, push_notification, report_error};
use super::transfer::{PROGRESS_INTERVAL, Throttle, Transfer};

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

/// Why an upload did not start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UploadError {
    Blocked(Vec<BlockingReason>),
    UnknownVideo,
    UnknownUpload,
    AlreadyUploading,
    NotResumable,
    Storage(String),
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
            UploadError::NotResumable => write!(f, "This upload can't be resumed. Upload the video again."),
            UploadError::Storage(error) => write!(f, "Couldn't save the upload: {error}."),
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
            RetryCause::Network => "Can't reach YouTube",
            RetryCause::Server => "YouTube isn't responding",
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
    retry: Option<(Retry, u64)>,
    /// The upload title, for a notice if the video is deleted while it uploads.
    title: String,
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
    Progress { at: Instant, sent: u64, total: u64 },
    Retrying(Retry),
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
        self.active.get(&id).and_then(|upload| upload.retry).map(|(retry, _)| retry)
    }

    pub fn is_cancelling(&self, id: UploadId) -> bool {
        self.active.get(&id).is_some_and(|upload| upload.cancel.load(Ordering::Relaxed))
    }

    pub fn active_upload_count(&self) -> usize {
        self.active.len()
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

    /// What stops `video_id` from being uploaded now; empty when it is ready.
    pub fn blocking_reasons(&self, video_id: VideoId, cx: &App) -> Vec<BlockingReason> {
        let Some(video) = self.state.read(cx).video(video_id) else { return Vec::new() };
        let file_exists = video.publish.file.as_deref().is_some_and(Path::is_file);
        rules::blocking_reasons(video, self.is_connected(), file_exists, Utc::now())
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
        let (Some(client), Some(file)) = (self.client.clone(), video.publish.file.clone()) else {
            return Err(UploadError::Blocked(vec![BlockingReason::NotConnected]));
        };
        let total = file_len(&file).ok_or(UploadError::Blocked(vec![BlockingReason::FileMissing]))?;
        let now = Utc::now();
        let job = UploadJob {
            id: UploadId::new(),
            video_id,
            file: file.clone(),
            status: UploadStatus::Pending,
            session_url: None,
            created_at: now,
            updated_at: now,
        };
        lock(&self.store(cx)).insert_upload(&job).map_err(|error| UploadError::Storage(error.to_string()))?;
        self.reload_jobs(cx);
        self.spawn_transfer(job.id, client, file, total, Request::Upload(Box::new(rules::metadata(&video))), cx);
        Ok(job.id)
    }

    /// Continues a stopped upload from wherever the YouTube session got to.
    pub fn resume_upload(&mut self, id: UploadId, cx: &mut Context<Self>) -> Result<(), UploadError> {
        let job = self.job(id).cloned().ok_or(UploadError::UnknownUpload)?;
        if self.is_active(id) {
            return Err(UploadError::AlreadyUploading);
        }
        let Some(session_url) = job.session_url.clone().filter(|_| self.is_resumable(&job)) else {
            return Err(UploadError::NotResumable);
        };
        let Some(client) = self.client.clone() else {
            return Err(UploadError::Blocked(vec![BlockingReason::NotConnected]));
        };
        let total = file_len(&job.file).ok_or(UploadError::Blocked(vec![BlockingReason::FileMissing]))?;
        if self.spawn_transfer(id, client, job.file, total, Request::Resume(session_url), cx) {
            self.update_job(id, cx, |job| job.status = UploadStatus::Uploading { sent: 0, total });
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
        let worker = (client.clone(), cancel.clone());
        let spawned = thread::Builder::new()
            .name("youtube-upload".into())
            .spawn(move || transfer_file(&worker.0, &file, &request, &worker.1, &sender));
        if let Err(error) = spawned {
            self.update_job(id, cx, |job| {
                job.status = UploadStatus::Failed { error: format!("Couldn't start the upload: {error}.") }
            });
            return false;
        }
        self.active.insert(
            id,
            ActiveUpload { cancel, client, transfer: Transfer::new(total), high_water: 0, retry: None, title },
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
                upload.transfer.record(at, sent, total);
                upload.high_water = upload.high_water.max(sent);
                if upload.retry.is_some_and(|(_, mark)| sent > mark) {
                    upload.retry = None;
                }
                if step(&upload.transfer) > before && sent < total {
                    self.update_job(id, cx, |job| job.status = UploadStatus::Uploading { sent, total });
                }
                cx.notify();
            }
            UploadMessage::Retrying(retry) => {
                let Some(upload) = self.active.get_mut(&id) else { return };
                upload.retry = Some((retry, upload.high_water));
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
                    Err(error) => self.on_upload_failed(id, &error, cx),
                }
            }
        }
    }

    /// Deleting a video cancels its upload, but the last chunk may already have been
    /// on its way: then the video is on YouTube, and the creator should know.
    fn on_orphan_finished(&mut self, upload: ActiveUpload, uploaded: Option<UploadedVideo>, cx: &mut Context<Self>) {
        let Some(uploaded) = uploaded else { return };
        let (title, url): (String, SharedString) = (upload.title, uploaded.url.into());
        push_notification(cx, move || {
            Notification::warning(
                "The upload finished before the video was deleted, so it is on your channel. \
                 Remove it in YouTube Studio if you don't want it there.",
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
        }
        self.jobs.retain(|job| !gone.contains(&job.id));
        cx.emit(YoutubeEvent::UploadsChanged);
        cx.notify();
    }

    /// Whether any of `video_ids` is uploading right now.
    pub fn is_uploading_any(&self, video_ids: &[VideoId]) -> bool {
        self.active.keys().any(|id| self.job(*id).is_some_and(|job| video_ids.contains(&job.video_id)))
    }

    fn on_upload_failed(&mut self, id: UploadId, error: &YoutubeError, cx: &mut Context<Self>) {
        let message = describe_error(error);
        // An expired session, or one the current connection refuses to use (another
        // backend's after a switch) or a changed file, can't take the rest of the file.
        let session_unusable = matches!(error, YoutubeError::SessionExpired | YoutubeError::InvalidInput(_));
        self.update_job(id, cx, |job| {
            job.status = UploadStatus::Failed { error: message.clone() };
            if session_unusable {
                job.session_url = None;
            }
        });
        if error.needs_sign_in() {
            self.require_sign_in(cx);
        }
        let title = self.video_title(id, cx);
        push_notification(cx, move || Notification::error(message).title(format!("Couldn't upload “{title}”")));
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
            if video.publish.publish_at.is_some() {
                video.status = VideoStatus::Scheduled;
            } else if privacy != Privacy::Private {
                video.status = VideoStatus::Published;
            }
            let thumbnail = video.publish.thumbnail.clone();
            state.update_video(video, cx);
            thumbnail
        });

        let title = self.video_title(id, cx);
        let url: SharedString = uploaded.url.clone().into();
        push_notification(cx, move || {
            Notification::success(format!("“{title}”"))
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
                    push_notification(cx, move || {
                        Notification::warning(message).title(format!("The thumbnail of “{title}” wasn't set"))
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
                if let Some(outcome) = processed(&info) {
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
                self.refresh_account(cx);
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
        let store = self.store(cx);
        let recovered = (|| {
            let mut store = lock(&store);
            let mut changed = false;
            for mut job in store.list_uploads(None)? {
                job.status = match job.status {
                    UploadStatus::Pending | UploadStatus::Uploading { .. } => UploadStatus::Failed {
                        error: if job.session_url.is_some() { INTERRUPTED_RESUMABLE } else { INTERRUPTED }.to_owned(),
                    },
                    UploadStatus::Processing { video_id } => UploadStatus::Done { video_id },
                    _ => continue,
                };
                store.update_upload(&mut job)?;
                changed = true;
            }
            Ok::<_, StoreError>(changed)
        })();
        match recovered {
            Ok(changed) => {
                self.jobs = lock(&store).list_uploads(None).unwrap_or_default();
                if changed {
                    self.state.update(cx, |state, cx| state.reload(cx));
                }
            }
            Err(error) => report_error(&self.state, format!("Couldn't check interrupted uploads: {error}"), cx),
        }
    }

    /// Applies `change` to the stored job and tells everyone.
    fn update_job(&mut self, id: UploadId, cx: &mut Context<Self>, change: impl FnOnce(&mut UploadJob)) {
        let Some(mut job) = self.job(id).cloned() else { return };
        change(&mut job);
        match lock(&self.store(cx)).update_upload(&mut job) {
            Ok(()) => {}
            // The video, and its uploads with it, were deleted meanwhile.
            Err(StoreError::NotFound { .. }) => {}
            Err(error) => report_error(&self.state, format!("Couldn't save the upload status: {error}"), cx),
        }
        self.reload_jobs(cx);
    }

    fn reload_jobs(&mut self, cx: &mut Context<Self>) {
        match lock(&self.store(cx)).list_uploads(None) {
            Ok(jobs) => self.jobs = jobs,
            Err(error) => report_error(&self.state, format!("Couldn't load the uploads: {error}"), cx),
        }
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

/// Upload worker: sends the file and reports progress at most every [`PROGRESS_INTERVAL`].
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
    let mut progress = |event: UploadProgress| match event {
        UploadProgress::Started { session_url } => send(UploadMessage::Started(session_url)),
        UploadProgress::Bytes { sent, total } => {
            let at = Instant::now();
            if sent == total || throttle.ready(at) {
                send(UploadMessage::Progress { at, sent, total });
            }
        }
        UploadProgress::Retrying { attempt, max_retries, cause, .. } => {
            send(UploadMessage::Retrying(Retry { attempt, max_retries, cause }))
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
        return Some(Err("YouTube couldn't process the video. Check it in YouTube Studio.".to_owned()));
    }
    let ready = info.processing_status == Some(ProcessingStatus::Succeeded)
        || info.upload_status == YoutubeUploadStatus::Processed;
    ready.then_some(Ok(ProcessingOutcome::Ready))
}

fn file_len(path: &Path) -> Option<u64> {
    fs::metadata(path).ok().filter(|metadata| metadata.is_file()).map(|metadata| metadata.len())
}

#[cfg(test)]
mod tests {
    use videocrm_youtube::Privacy as YoutubePrivacy;

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
        assert_eq!(UploadError::NotResumable.to_string(), "This upload can't be resumed. Upload the video again.");
    }
}
