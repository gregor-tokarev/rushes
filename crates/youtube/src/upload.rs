//! Resumable uploads (<https://developers.google.com/youtube/v3/guides/using_resumable_upload_protocol>):
//! start a session, PUT the file in chunks, and after any failure ask the server how
//! many bytes it has before continuing.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::Serialize;
use ureq::SendBody;
use url::Url;

use crate::client::{YoutubeClient, extension};
use crate::error::{Error, Result, RetryCause, api_error};
use crate::http::{self, HttpResponse, bearer, body_text};
use crate::types::{Privacy, UploadStatus, VideoResource, watch_url};

/// Chunk sizes must be multiples of this, except for the last chunk.
pub const UPLOAD_GRANULARITY: u64 = 256 * 1024;
pub const DEFAULT_CHUNK_SIZE: u64 = 8 * 1024 * 1024;
/// How often in-flight bytes are reported while a chunk is being sent.
const PROGRESS_STEP: usize = 256 * 1024;
/// Slowest upload speed (bytes/s) before a chunk send counts as stalled and is retried.
const MIN_UPLOAD_SPEED: u64 = 32 * 1024;
const CANCEL_POLL: Duration = Duration::from_millis(50);

/// Metadata for `videos.insert`.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct VideoMetadata {
    pub title: String,
    pub description: String,
    pub tags: Vec<String>,
    /// Category id such as "22" (People & Blogs) or "28" (Science & Technology).
    /// Empty lets YouTube pick its default.
    pub category_id: String,
    pub privacy: Privacy,
    /// Scheduled publishing; YouTube requires `privacy == Private` until then.
    pub publish_at: Option<DateTime<Utc>>,
    pub made_for_kids: bool,
    pub default_language: Option<String>,
}

impl VideoMetadata {
    /// Checks YouTube's documented limits before any quota is spent.
    pub fn validate(&self) -> Result<()> {
        let invalid = |message: &str| Err(Error::InvalidInput(message.to_owned()));
        let has_brackets = |text: &str| text.contains(['<', '>']);
        if self.title.trim().is_empty() {
            return invalid("the title is empty");
        }
        if self.title.chars().count() > 100 {
            return invalid("the title is longer than 100 characters");
        }
        if has_brackets(&self.title) || has_brackets(&self.description) {
            return invalid("the title and description cannot contain < or >");
        }
        if self.description.len() > 5000 {
            return invalid("the description is longer than 5000 bytes");
        }
        if self.tags.iter().any(|tag| has_brackets(tag)) {
            return invalid("tags cannot contain < or >");
        }
        // YouTube counts separators and the quotes around tags with spaces.
        let tags_length =
            self.tags.iter().map(|tag| tag.chars().count() + if tag.contains(' ') { 2 } else { 0 }).sum::<usize>()
                + self.tags.len().saturating_sub(1);
        if tags_length > 500 {
            return invalid("the tags are longer than 500 characters in total");
        }
        if !self.category_id.is_empty() && !self.category_id.bytes().all(|b| b.is_ascii_digit()) {
            return invalid("the category id must be numeric");
        }
        if let Some(publish_at) = self.publish_at {
            if self.privacy != Privacy::Private {
                return invalid("a scheduled video must be private until it is published");
            }
            if publish_at <= Utc::now() {
                return invalid("the scheduled publish time is in the past");
            }
        }
        Ok(())
    }

    fn resource(&self) -> VideoInsert<'_> {
        VideoInsert {
            snippet: SnippetInsert {
                title: &self.title,
                description: &self.description,
                tags: &self.tags,
                category_id: Some(self.category_id.as_str()).filter(|id| !id.is_empty()),
                default_language: self.default_language.as_deref(),
            },
            status: StatusInsert {
                privacy_status: self.privacy,
                publish_at: self.publish_at,
                self_declared_made_for_kids: self.made_for_kids,
            },
        }
    }
}

#[derive(Serialize)]
struct VideoInsert<'a> {
    snippet: SnippetInsert<'a>,
    status: StatusInsert,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SnippetInsert<'a> {
    title: &'a str,
    description: &'a str,
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    tags: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    category_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_language: Option<&'a str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusInsert {
    privacy_status: Privacy,
    #[serde(skip_serializing_if = "Option::is_none")]
    publish_at: Option<DateTime<Utc>>,
    self_declared_made_for_kids: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UploadProgress {
    /// The session exists. Persist `session_url` to resume after a crash or cancel.
    Started {
        session_url: String,
    },
    /// `sent` bytes are on their way or confirmed. May step back when the server
    /// kept less than was sent.
    Bytes {
        sent: u64,
        total: u64,
    },
    /// A request failed for a passing reason; attempt `attempt` of `max_retries`
    /// starts after `wait`. Progress past the earlier high-water mark means it worked.
    Retrying {
        attempt: u32,
        max_retries: u32,
        wait: Duration,
        cause: RetryCause,
    },
    Finished,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadedVideo {
    pub id: String,
    pub url: String,
    pub channel_id: String,
    pub title: String,
    pub privacy: Privacy,
    pub upload_status: UploadStatus,
}

/// Chunking and retry behaviour of uploads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UploadOptions {
    /// Rounded down to a multiple of [`UPLOAD_GRANULARITY`] (at least one).
    pub chunk_size: u64,
    /// Consecutive transient failures tolerated before giving up.
    pub max_retries: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
}

impl Default for UploadOptions {
    /// About a minute of retries (2, 4, 8, 16 and 32 s) before an outage is reported;
    /// the upload can be resumed after that.
    fn default() -> Self {
        Self {
            chunk_size: DEFAULT_CHUNK_SIZE,
            max_retries: 5,
            initial_backoff: Duration::from_secs(2),
            max_backoff: Duration::from_secs(32),
        }
    }
}

impl UploadOptions {
    pub(crate) fn normalized(self) -> Self {
        let chunk_size = (self.chunk_size / UPLOAD_GRANULARITY).max(1) * UPLOAD_GRANULARITY;
        Self { chunk_size, ..self }
    }

    /// Exponential backoff for the `failures`-th consecutive failure (0-based).
    fn backoff(&self, failures: u32) -> Duration {
        self.initial_backoff.saturating_mul(2u32.saturating_pow(failures)).min(self.max_backoff)
    }

    /// Reports the retry after the `failures`-th consecutive failure (0-based) and
    /// waits out its backoff.
    fn retry(
        &self,
        failures: u32,
        cause: RetryCause,
        progress: &mut dyn FnMut(UploadProgress),
        cancel: &AtomicBool,
        session_url: Option<&str>,
    ) -> Result<()> {
        let wait = self.backoff(failures);
        progress(UploadProgress::Retrying { attempt: failures + 1, max_retries: self.max_retries, wait, cause });
        backoff_sleep(wait, cancel, session_url)
    }
}

/// What the upload server said about a session.
#[derive(Debug)]
enum Step {
    /// Incomplete; the server holds this many bytes.
    Offset(u64),
    Done(UploadedVideo),
}

impl YoutubeClient {
    /// Uploads a video with the resumable protocol. `progress` gets
    /// [`UploadProgress::Started`] with the session URL first; on cancellation the
    /// error carries the same URL for [`YoutubeClient::resume_upload`].
    pub fn upload_video(
        &self,
        path: &Path,
        metadata: &VideoMetadata,
        progress: &mut dyn FnMut(UploadProgress),
        cancel: &AtomicBool,
    ) -> Result<UploadedVideo> {
        metadata.validate()?;
        let mut file = VideoFile::open(path)?;
        let session_url = self.start_session(metadata, &file, progress, cancel)?;
        progress(UploadProgress::Started { session_url: session_url.clone() });
        self.transfer(&session_url, &mut file, Some(0), progress, cancel)
    }

    /// Continues an interrupted upload of the same file from wherever the server is.
    pub fn resume_upload(
        &self,
        session_url: &str,
        path: &Path,
        progress: &mut dyn FnMut(UploadProgress),
        cancel: &AtomicBool,
    ) -> Result<UploadedVideo> {
        self.check_session_origin(session_url)?;
        let mut file = VideoFile::open(path)?;
        progress(UploadProgress::Started { session_url: session_url.to_owned() });
        self.transfer(session_url, &mut file, None, progress, cancel)
    }

    fn start_session(
        &self,
        metadata: &VideoMetadata,
        file: &VideoFile,
        progress: &mut dyn FnMut(UploadProgress),
        cancel: &AtomicBool,
    ) -> Result<String> {
        let url = format!("{}/videos", self.config.upload_base);
        let body = serde_json::to_vec(&metadata.resource())?;
        let mut failures = 0;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled { session_url: None });
            }
            let result = self
                .authorized(|token| {
                    self.agent
                        .post(&url)
                        .query("uploadType", "resumable")
                        .query("part", "snippet,status")
                        .header("Authorization", bearer(token))
                        .header("Content-Type", "application/json; charset=UTF-8")
                        .header("X-Upload-Content-Length", file.len.to_string())
                        .header("X-Upload-Content-Type", file.content_type)
                        .send(&body[..])
                })
                .and_then(|response| self.session_location(response));
            match result {
                Err(error) if error.is_transient() && failures < self.upload.max_retries => {
                    self.upload.retry(failures, error.retry_cause(), progress, cancel, None)?;
                    failures += 1;
                }
                result => return result,
            }
        }
    }

    /// The session URL receives the access token, so it must point at the configured
    /// upload server (it may come from the app's database).
    fn check_session_origin(&self, session_url: &str) -> Result<()> {
        let origin = |url: &str| Url::parse(url).map(|url| url.origin());
        match (origin(session_url), origin(&self.config.upload_base)) {
            (Ok(session), Ok(server)) if session == server => Ok(()),
            _ => Err(Error::InvalidInput(format!(
                "{session_url} is not an upload session of {}",
                self.config.upload_base
            ))),
        }
    }

    fn session_location(&self, mut response: HttpResponse) -> Result<String> {
        let status = response.status().as_u16();
        if !response.status().is_success() {
            return Err(api_error(status, &body_text(&mut response)));
        }
        let location = response
            .headers()
            .get("Location")
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| Error::Protocol("upload session response has no Location header".to_owned()))?;
        let base = Url::parse(&self.config.upload_base)
            .map_err(|e| Error::InvalidInput(format!("invalid upload URL {}: {e}", self.config.upload_base)))?;
        let session =
            base.join(location).map_err(|e| Error::Protocol(format!("invalid upload session URL {location}: {e}")))?;
        Ok(session.into())
    }

    /// Sends the file from `offset` (or from the server's offset when `None`) until
    /// the server returns the video, retrying transient failures.
    fn transfer(
        &self,
        session_url: &str,
        file: &mut VideoFile,
        mut offset: Option<u64>,
        progress: &mut dyn FnMut(UploadProgress),
        cancel: &AtomicBool,
    ) -> Result<UploadedVideo> {
        let total = file.len;
        let mut failures = 0;
        // Highest offset the server confirmed; any progress past it resets `failures`.
        let mut confirmed = offset.unwrap_or(0);
        let mut chunk = Vec::new();
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(cancelled(session_url));
            }
            let step = match offset {
                None => self.query_session(session_url, total),
                Some(start) => {
                    progress(UploadProgress::Bytes { sent: start, total });
                    file.read_chunk(start, chunk_len(start, total, self.upload.chunk_size), &mut chunk)?;
                    self.put_chunk(session_url, &chunk, start, total, progress, cancel)
                }
            };
            match step {
                Ok(Step::Done(video)) => {
                    progress(UploadProgress::Bytes { sent: total, total });
                    progress(UploadProgress::Finished);
                    return Ok(video);
                }
                Ok(Step::Offset(next)) => {
                    if next >= total {
                        return Err(Error::Protocol(format!(
                            "the upload server holds {next} of {total} bytes but did not finish the upload"
                        )));
                    }
                    if next > confirmed {
                        failures = 0;
                    } else if offset.is_some() {
                        // A chunk went through but the server kept nothing new.
                        if failures >= self.upload.max_retries {
                            return Err(Error::Protocol(format!(
                                "the upload server keeps rejecting data at byte {next}"
                            )));
                        }
                        self.upload.retry(failures, RetryCause::Server, progress, cancel, Some(session_url))?;
                        failures += 1;
                    }
                    confirmed = next;
                    offset = Some(next);
                }
                Err(_) if cancel.load(Ordering::Relaxed) => return Err(cancelled(session_url)),
                Err(error) if error.is_transient() && failures < self.upload.max_retries => {
                    self.upload.retry(failures, error.retry_cause(), progress, cancel, Some(session_url))?;
                    failures += 1;
                    offset = None;
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn put_chunk(
        &self,
        session_url: &str,
        chunk: &[u8],
        start: u64,
        total: u64,
        progress: &mut dyn FnMut(UploadProgress),
        cancel: &AtomicBool,
    ) -> Result<Step> {
        let range = content_range(start, chunk.len() as u64, total);
        let response = self.authorized(|token| {
            let mut body =
                ProgressReader { chunk, position: 0, reported: 0, start, total, progress: &mut *progress, cancel };
            self.agent
                .put(session_url)
                .config()
                .timeout_send_body(Some(send_timeout(chunk.len())))
                .build()
                .header("Authorization", bearer(token))
                .header("Content-Length", chunk.len().to_string())
                .header("Content-Range", &range)
                .send(SendBody::from_reader(&mut body))
        })?;
        upload_step(response)
    }

    /// Asks how many bytes the server has (`Content-Range: bytes */total`).
    fn query_session(&self, session_url: &str, total: u64) -> Result<Step> {
        let range = format!("bytes */{total}");
        let response = self.authorized(|token| {
            self.agent
                .put(session_url)
                .header("Authorization", bearer(token))
                .header("Content-Length", "0")
                .header("Content-Range", &range)
                .send_empty()
        })?;
        upload_step(response)
    }
}

/// Waits out a backoff while watching `cancel`.
fn backoff_sleep(duration: Duration, cancel: &AtomicBool, session_url: Option<&str>) -> Result<()> {
    // A backoff too long for an `Instant` (from huge `UploadOptions`) waits for cancel.
    let deadline = Instant::now().checked_add(duration);
    while deadline.is_none_or(|deadline| Instant::now() < deadline) {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled { session_url: session_url.map(str::to_owned) });
        }
        let left = deadline.map_or(CANCEL_POLL, |d| d.saturating_duration_since(Instant::now()));
        thread::sleep(CANCEL_POLL.min(left));
    }
    Ok(())
}

fn cancelled(session_url: &str) -> Error {
    Error::Cancelled { session_url: Some(session_url.to_owned()) }
}

fn upload_step(mut response: HttpResponse) -> Result<Step> {
    match response.status().as_u16() {
        308 => {
            let range = response.headers().get("Range").and_then(|value| value.to_str().ok());
            Ok(Step::Offset(persisted_bytes(range)?))
        }
        200 | 201 => {
            let video: VideoResource = http::json(response)?;
            let snippet = video.snippet.ok_or_else(|| Error::Protocol("uploaded video has no snippet".to_owned()))?;
            let status = video.status.ok_or_else(|| Error::Protocol("uploaded video has no status".to_owned()))?;
            Ok(Step::Done(UploadedVideo {
                url: watch_url(&video.id),
                id: video.id,
                channel_id: snippet.channel_id,
                title: snippet.title,
                privacy: status.privacy_status,
                upload_status: status.upload_status,
            }))
        }
        404 | 410 => Err(Error::SessionExpired),
        status => Err(api_error(status, &body_text(&mut response))),
    }
}

/// Bytes the server holds according to a 308 `Range` header (`bytes=0-N`); no
/// header means nothing was persisted yet.
fn persisted_bytes(range: Option<&str>) -> Result<u64> {
    let Some(range) = range else { return Ok(0) };
    range
        .trim()
        .strip_prefix("bytes=")
        .unwrap_or(range)
        .strip_prefix("0-")
        .and_then(|end| end.parse::<u64>().ok())
        .map(|end| end + 1)
        .ok_or_else(|| Error::Protocol(format!("unexpected Range header from upload server: {range}")))
}

/// Time allowed to send one chunk; a dead connection otherwise blocks in a socket
/// write for many minutes, during which the cancel flag is not looked at either.
fn send_timeout(len: usize) -> Duration {
    Duration::from_secs(30 + len as u64 / MIN_UPLOAD_SPEED)
}

fn content_range(start: u64, len: u64, total: u64) -> String {
    format!("bytes {start}-{}/{total}", start + len - 1)
}

fn chunk_len(start: u64, total: u64, chunk_size: u64) -> u64 {
    chunk_size.min(total - start)
}

fn video_content_type(path: &Path) -> &'static str {
    match extension(path).as_deref() {
        Some("mp4") => "video/mp4",
        Some("m4v") => "video/x-m4v",
        Some("mov") => "video/quicktime",
        Some("webm") => "video/webm",
        Some("mkv") => "video/x-matroska",
        Some("avi") => "video/x-msvideo",
        _ => "application/octet-stream",
    }
}

struct VideoFile {
    file: File,
    len: u64,
    content_type: &'static str,
    path: String,
}

impl VideoFile {
    fn open(path: &Path) -> Result<Self> {
        let display = path.display().to_string();
        let file = File::open(path).map_err(|e| Error::io(format!("opening {display}"), e))?;
        let len = file.metadata().map_err(|e| Error::io(format!("reading {display}"), e))?.len();
        if len == 0 {
            return Err(Error::InvalidInput(format!("{display} is empty")));
        }
        Ok(Self { file, len, content_type: video_content_type(path), path: display })
    }

    fn read_chunk(&mut self, start: u64, len: u64, buffer: &mut Vec<u8>) -> Result<()> {
        let len =
            usize::try_from(len).map_err(|_| Error::InvalidInput("chunk size does not fit in memory".to_owned()))?;
        buffer.resize(len, 0);
        self.file
            .seek(SeekFrom::Start(start))
            .and_then(|_| self.file.read_exact(buffer))
            .map_err(|e| Error::io(format!("reading {}", self.path), e))
    }
}

/// Chunk body that reports progress as the HTTP client pulls it, and aborts the
/// request at its next read once `cancel` is set.
struct ProgressReader<'a> {
    chunk: &'a [u8],
    position: usize,
    reported: usize,
    start: u64,
    total: u64,
    progress: &'a mut dyn FnMut(UploadProgress),
    cancel: &'a AtomicBool,
}

impl Read for ProgressReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(io::Error::other("upload cancelled"));
        }
        let n = buf.len().min(self.chunk.len() - self.position);
        buf[..n].copy_from_slice(&self.chunk[self.position..self.position + n]);
        self.position += n;
        if self.position - self.reported >= PROGRESS_STEP || (n > 0 && self.position == self.chunk.len()) {
            self.reported = self.position;
            (self.progress)(UploadProgress::Bytes { sent: self.start + self.position as u64, total: self.total });
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata() -> VideoMetadata {
        VideoMetadata { title: "A video".into(), privacy: Privacy::Unlisted, ..Default::default() }
    }

    #[test]
    fn parses_range_headers() {
        assert_eq!(persisted_bytes(None).unwrap(), 0);
        assert_eq!(persisted_bytes(Some("bytes=0-262143")).unwrap(), 262_144);
        assert_eq!(persisted_bytes(Some("0-0")).unwrap(), 1);
        assert!(persisted_bytes(Some("bytes=5-10")).is_err());
        assert!(persisted_bytes(Some("bytes=0-")).is_err());
    }

    #[test]
    fn chunk_math() {
        let chunk = 4 * UPLOAD_GRANULARITY;
        assert_eq!(chunk_len(0, 10 * UPLOAD_GRANULARITY, chunk), chunk);
        assert_eq!(chunk_len(8 * UPLOAD_GRANULARITY, 10 * UPLOAD_GRANULARITY, chunk), 2 * UPLOAD_GRANULARITY);
        assert_eq!(chunk_len(9, 10, chunk), 1);
        assert_eq!(content_range(0, 262_144, 1_000_000), "bytes 0-262143/1000000");
        assert_eq!(content_range(999_999, 1, 1_000_000), "bytes 999999-999999/1000000");
    }

    #[test]
    fn chunk_size_is_rounded_to_the_granularity() {
        let options = |chunk_size| UploadOptions { chunk_size, ..UploadOptions::default() }.normalized().chunk_size;
        assert_eq!(options(DEFAULT_CHUNK_SIZE), DEFAULT_CHUNK_SIZE);
        assert_eq!(options(UPLOAD_GRANULARITY * 3 + 17), UPLOAD_GRANULARITY * 3);
        assert_eq!(options(1), UPLOAD_GRANULARITY);
    }

    #[test]
    fn backoff_doubles_up_to_the_cap() {
        let options = UploadOptions::default();
        assert_eq!(options.backoff(0), Duration::from_secs(2));
        assert_eq!(options.backoff(3), Duration::from_secs(16));
        assert_eq!(options.backoff(10), Duration::from_secs(32));
        assert_eq!(options.backoff(40), Duration::from_secs(32));
        // All retries together take about a minute.
        let total: Duration = (0..options.max_retries).map(|n| options.backoff(n)).sum();
        assert_eq!(total, Duration::from_secs(62));
    }

    #[test]
    fn retries_are_reported_before_the_wait() {
        let options = UploadOptions { initial_backoff: Duration::from_millis(1), ..UploadOptions::default() };
        let mut events = Vec::new();
        options.retry(1, RetryCause::Network, &mut |event| events.push(event), &AtomicBool::new(false), None).unwrap();
        assert_eq!(
            events,
            [UploadProgress::Retrying {
                attempt: 2,
                max_retries: 5,
                wait: Duration::from_millis(2),
                cause: RetryCause::Network
            }]
        );
    }

    #[test]
    fn backoff_longer_than_an_instant_can_hold_is_cancellable() {
        let cancel = AtomicBool::new(true);
        assert!(matches!(
            backoff_sleep(Duration::MAX, &cancel, Some("session")),
            Err(Error::Cancelled { session_url: Some(url) }) if url == "session"
        ));
    }

    #[test]
    fn metadata_validation() {
        assert!(metadata().validate().is_ok());
        let check = |change: fn(&mut VideoMetadata)| {
            let mut metadata = metadata();
            change(&mut metadata);
            metadata.validate()
        };
        assert!(check(|m| m.title = "  ".into()).is_err());
        assert!(check(|m| m.title = "é".repeat(100)).is_ok());
        assert!(check(|m| m.title = "x".repeat(101)).is_err());
        assert!(check(|m| m.title = "a <b>".into()).is_err());
        assert!(check(|m| m.description = "é".repeat(2501)).is_err());
        assert!(check(|m| m.tags = vec!["x".repeat(250), "y".repeat(249)]).is_ok());
        assert!(check(|m| m.tags = vec!["x".repeat(250), "y".repeat(250)]).is_err());
        assert!(check(|m| m.tags = vec!["with space".repeat(50)]).is_err());
        assert!(check(|m| m.category_id = "abc".into()).is_err());
        assert!(check(|m| m.publish_at = Some(Utc::now() + chrono::TimeDelta::days(1))).is_err());
        assert!(
            check(|m| {
                m.privacy = Privacy::Private;
                m.publish_at = Some(Utc::now() + chrono::TimeDelta::days(1));
            })
            .is_ok()
        );
        assert!(
            check(|m| {
                m.privacy = Privacy::Private;
                m.publish_at = Some(Utc::now() - chrono::TimeDelta::days(1));
            })
            .is_err()
        );
    }

    #[test]
    fn metadata_serializes_to_the_insert_body() {
        let metadata = VideoMetadata {
            title: "Title".into(),
            description: "Desc".into(),
            tags: vec!["a".into()],
            category_id: "28".into(),
            privacy: Privacy::Private,
            publish_at: Some("2030-01-02T03:04:05Z".parse().unwrap()),
            made_for_kids: false,
            default_language: Some("en".into()),
        };
        assert_eq!(
            serde_json::to_value(metadata.resource()).unwrap(),
            serde_json::json!({
                "snippet": {"title": "Title", "description": "Desc", "tags": ["a"], "categoryId": "28", "defaultLanguage": "en"},
                "status": {"privacyStatus": "private", "publishAt": "2030-01-02T03:04:05Z", "selfDeclaredMadeForKids": false}
            })
        );
        let minimal = serde_json::to_value(self::metadata().resource()).unwrap();
        assert_eq!(minimal["snippet"], serde_json::json!({"title": "A video", "description": ""}));
    }

    #[test]
    fn progress_reader_reports_and_cancels() {
        let chunk = vec![7u8; PROGRESS_STEP * 2 + 10];
        let cancel = AtomicBool::new(false);
        let mut events = Vec::new();
        let mut record = |event| events.push(event);
        let mut reader = ProgressReader {
            chunk: &chunk,
            position: 0,
            reported: 0,
            start: 100,
            total: 10_000_000,
            progress: &mut record,
            cancel: &cancel,
        };
        let mut sink = Vec::new();
        reader.read_to_end(&mut sink).unwrap();
        assert_eq!(sink, chunk);
        let last = PROGRESS_STEP as u64 * 2 + 110;
        assert_eq!(events.last(), Some(&UploadProgress::Bytes { sent: last, total: 10_000_000 }));
        assert!(events.len() >= 3);

        cancel.store(true, Ordering::Relaxed);
        let mut reader = ProgressReader {
            chunk: &chunk,
            position: 0,
            reported: 0,
            start: 0,
            total: 1,
            progress: &mut |_| {},
            cancel: &cancel,
        };
        assert!(reader.read(&mut [0; 16]).is_err());
    }

    #[test]
    fn resume_refuses_session_urls_of_other_servers() {
        let config = crate::YoutubeConfig::emulator("http://127.0.0.1:1", "http://127.0.0.1:2");
        let credentials = crate::Credentials {
            access_token: "token".into(),
            refresh_token: None,
            expires_at: Utc::now() + chrono::TimeDelta::hours(1),
            scope: String::new(),
            id_token: None,
        };
        let client = YoutubeClient::new(config, credentials, |_| {});
        let resume =
            |url: &str| client.resume_upload(url, Path::new("missing.mp4"), &mut |_| {}, &AtomicBool::new(false));
        assert!(matches!(resume("https://elsewhere.example/upload?upload_id=x"), Err(Error::InvalidInput(_))));
        // Same server: passes the check and fails on the missing file instead.
        assert!(matches!(resume("http://127.0.0.1:2/upload/youtube/v3/videos?upload_id=x"), Err(Error::Io { .. })));
    }

    #[test]
    fn content_types_by_extension() {
        assert_eq!(video_content_type(Path::new("final.MP4")), "video/mp4");
        assert_eq!(video_content_type(Path::new("take.mov")), "video/quicktime");
        assert_eq!(video_content_type(Path::new("raw")), "application/octet-stream");
    }
}
