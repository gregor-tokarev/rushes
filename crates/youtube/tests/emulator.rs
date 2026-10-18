//! End-to-end tests against the local Google + YouTube emulators (`emulators/`).
//! Every test starts its own emulator pair on free ports. Skipped with a notice when
//! Node or `emulators/node_modules` is missing.

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chrono::{TimeDelta, Timelike as _, Utc};
use rushes_youtube::{
    Account, CredentialStore, Credentials, Details, DetailsChange, Error, MANAGE_SCOPE, Privacy, ProcessingStatus,
    RetryCause, SavedLogin, UPLOAD_GRANULARITY, UploadOptions, UploadProgress, UploadStatus, VideoMetadata, Visibility,
    YoutubeClient, YoutubeConfig, begin_authorization, check_client, refresh,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const PROCESSING_MS: u64 = 1500;
/// A 1×1 PNG.
const PNG_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

/// A running emulator pair, stopped on drop.
struct Emulators {
    child: Child,
    google_url: String,
    youtube_url: String,
    // Spool directory of the YouTube emulator; removed on drop even if node is killed.
    _tmp: tempfile::TempDir,
}

impl Emulators {
    fn start() -> Option<Self> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../emulators");
        if !dir.join("node_modules/emulate").is_dir() {
            eprintln!("skipping emulator test: run `npm ci` in {} first", dir.display());
            return None;
        }
        if Command::new("node").arg("--version").output().is_err() {
            eprintln!("skipping emulator test: `node` is not installed");
            return None;
        }
        // Ports are probed then released, so another process may grab one; retry then.
        let mut failure = String::new();
        for _ in 0..3 {
            match Self::spawn(&dir) {
                Ok(emulators) => return Some(emulators),
                Err(log) => failure = log,
            }
        }
        panic!("emulators did not start:\n{failure}");
    }

    fn spawn(dir: &Path) -> Result<Self, String> {
        let tmp = tempfile::tempdir().map_err(|e| e.to_string())?;
        let (google_port, youtube_port) = free_ports();
        let log_path = tmp.path().join("emulators.log");
        let log = File::create(&log_path).map_err(|e| e.to_string())?;
        let mut child = Command::new("node")
            .args(["node_modules/emulate/dist/index.js", "start", "--config", "emulate.config.ts"])
            .current_dir(dir)
            .env("GOOGLE_EMULATOR_PORT", google_port.to_string())
            .env("YOUTUBE_EMULATOR_PORT", youtube_port.to_string())
            .env("YOUTUBE_EMULATOR_PROCESSING_MS", PROCESSING_MS.to_string())
            .env("YOUTUBE_EMULATOR_DAILY_QUOTA", "100000000")
            .env("TMPDIR", tmp.path())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .map_err(|e| e.to_string())?;

        let google_url = format!("http://127.0.0.1:{google_port}");
        let youtube_url = format!("http://127.0.0.1:{youtube_port}");
        let ready = |url: String| plain_agent().get(&url).call().is_ok_and(|r| r.status() == 200);
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Ok(Some(status)) = child.try_wait() {
                let log = std::fs::read_to_string(&log_path).unwrap_or_default();
                return Err(format!("exited with {status}: {log}"));
            }
            if ready(format!("{google_url}/.well-known/openid-configuration"))
                && ready(format!("{youtube_url}/_emulator/uploads"))
            {
                return Ok(Emulators { child, google_url, youtube_url, _tmp: tmp });
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err("timed out waiting for the emulators to answer".to_owned());
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn config(&self) -> YoutubeConfig {
        YoutubeConfig::emulator(&self.google_url, &self.youtube_url)
    }

    fn uploads(&self) -> Vec<Value> {
        let mut response = plain_agent().get(&format!("{}/_emulator/uploads", self.youtube_url)).call().unwrap();
        let body: Value = response.body_mut().read_json().unwrap();
        body["uploads"].as_array().unwrap().clone()
    }

    fn inject_fault(&self, fault: Value) {
        let response = plain_agent().post(&format!("{}/_emulator/faults", self.youtube_url)).send_json(fault).unwrap();
        assert_eq!(response.status(), 201);
    }
}

impl Drop for Emulators {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_ports() -> (u16, u16) {
    let first = TcpListener::bind("127.0.0.1:0").unwrap();
    let second = TcpListener::bind("127.0.0.1:0").unwrap();
    (first.local_addr().unwrap().port(), second.local_addr().unwrap().port())
}

fn plain_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(10)))
        .build()
        .into()
}

/// What a browser does on the emulator's consent page: pick the account's form,
/// submit it, follow the redirect to the app's loopback server. Returns that page.
fn consent_in_browser(authorize_url: &str, email: &str) -> String {
    let agent = plain_agent();
    let html = agent.get(authorize_url).call().unwrap().body_mut().read_to_string().unwrap();
    let form = html
        .split("<form")
        .skip(1)
        .map(parse_form)
        .find(|(_, fields)| fields.get("email").map(String::as_str) == Some(email))
        .unwrap_or_else(|| panic!("no consent form for {email}"));
    let action = url::Url::parse(authorize_url).unwrap().join(&form.0).unwrap();
    let response = agent.post(action.as_str()).send_form(form.1).unwrap();
    assert_eq!(response.status(), 302);
    let location = response.headers()["location"].to_str().unwrap().to_owned();
    agent.get(&location).call().unwrap().body_mut().read_to_string().unwrap()
}

/// `(action, hidden fields)` of one `<form ...>...</form>` fragment.
fn parse_form(fragment: &str) -> (String, HashMap<String, String>) {
    let attribute = |tag: &str, name: &str| {
        let start = tag.find(&format!("{name}=\""))? + name.len() + 2;
        let end = start + tag[start..].find('"')?;
        Some(unescape(&tag[start..end]))
    };
    let form_tag = &fragment[..fragment.find('>').unwrap()];
    let action = attribute(form_tag, "action").unwrap();
    let fields = fragment
        .split("<input")
        .skip(1)
        .filter_map(|input| Some((attribute(input, "name")?, attribute(input, "value")?)))
        .collect();
    (action, fields)
}

fn unescape(text: &str) -> String {
    text.replace("&quot;", "\"").replace("&#39;", "'").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

fn sign_in(emulators: &Emulators, email: &str) -> (Credentials, String) {
    let pending = begin_authorization(&emulators.config()).unwrap();
    let url = pending.authorize_url().to_owned();
    let email = email.to_owned();
    let browser = thread::spawn(move || consent_in_browser(&url, &email));
    let credentials = pending.wait(Duration::from_secs(30), &AtomicBool::new(false)).unwrap();
    (credentials, browser.join().unwrap())
}

fn client(emulators: &Emulators, credentials: Credentials) -> YoutubeClient {
    YoutubeClient::new(emulators.config(), credentials, |_| {}).with_upload_options(UploadOptions {
        chunk_size: UPLOAD_GRANULARITY,
        max_retries: 5,
        initial_backoff: Duration::from_millis(20),
        max_backoff: Duration::from_millis(200),
    })
}

/// Deterministic pseudo-random video bytes.
fn video_file(dir: &Path, name: &str, len: usize) -> (PathBuf, String) {
    let mut state = 0x2545_f491_4f6c_dd1du64 ^ len as u64;
    let bytes: Vec<u8> = (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect();
    let path = dir.join(name);
    File::create(&path).unwrap().write_all(&bytes).unwrap();
    (path, hex(&Sha256::digest(&bytes)))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

fn metadata(title: &str) -> VideoMetadata {
    VideoMetadata {
        title: title.to_owned(),
        description: "Made with Rushes".to_owned(),
        tags: vec!["rust".to_owned(), "desktop app".to_owned()],
        category_id: "28".to_owned(),
        privacy: Privacy::Unlisted,
        ..VideoMetadata::default()
    }
}

#[test]
fn signs_in_and_reads_the_channel() {
    let Some(emulators) = Emulators::start() else {
        return;
    };
    let (credentials, page) = sign_in(&emulators, "creator@example.com");
    assert!(page.contains("You’re signed in"), "{page}");
    assert!(credentials.refresh_token.is_some());
    assert!(credentials.id_token.is_some());
    assert!(credentials.has_scope("https://www.googleapis.com/auth/youtube.upload"));
    assert!(credentials.expires_at > Utc::now() + TimeDelta::minutes(50));

    let client = client(&emulators, credentials.clone());
    let user = client.userinfo().unwrap();
    assert_eq!(user.email, "creator@example.com");
    assert_eq!(user.name.as_deref(), Some("Demo Creator"));

    let channel = client.my_channel().unwrap();
    assert_eq!(channel.title, "Demo Creator");
    assert_eq!(channel.custom_url.as_deref(), Some("@creator"));
    assert!(channel.id.starts_with("UC"));
    assert_eq!(channel.uploads_playlist_id, format!("UU{}", &channel.id[2..]));
    assert_eq!(channel.video_count, 0);
    assert!(channel.thumbnail_url.is_some());

    let account = Account::new(&user, Some(&channel), client.config());
    assert!(account.backend.starts_with("Emulator (127.0.0.1:"));
    assert_eq!(account.channel_handle.as_deref(), Some("@creator"));
    assert_eq!(account.channel_thumbnail, channel.thumbnail_url);
    assert_eq!(account.video_count, Some(0));
    let dir = tempfile::tempdir().unwrap();
    let store = CredentialStore::new(dir.path().join("credentials.json"));
    let login = SavedLogin { credentials, account: Some(account) };
    store.save(&login).unwrap();
    assert_eq!(store.load().unwrap(), Some(login));

    // A second account gets its own channel.
    let (other, _) = sign_in(&emulators, "editor@example.com");
    let other_channel = self::client(&emulators, other).my_channel().unwrap();
    assert_eq!(other_channel.title, "Second Channel");
    assert_ne!(other_channel.id, channel.id);
}

#[test]
fn uploads_in_chunks_with_progress() {
    let Some(emulators) = Emulators::start() else {
        return;
    };
    let (credentials, _) = sign_in(&emulators, "creator@example.com");
    let client = client(&emulators, credentials);
    let dir = tempfile::tempdir().unwrap();
    let (path, sha256) = video_file(dir.path(), "final.mp4", 3 * 1024 * 1024 + 12_345);
    let total = std::fs::metadata(&path).unwrap().len();

    let mut events = Vec::new();
    let video = client
        .upload_video(&path, &metadata("Chunked upload"), &mut |event| events.push(event), &AtomicBool::new(false))
        .unwrap();

    assert!(
        matches!(&events[0], UploadProgress::Started { session_url } if session_url.starts_with(&emulators.youtube_url))
    );
    assert_eq!(events.last(), Some(&UploadProgress::Finished));
    let sent: Vec<u64> = events
        .iter()
        .filter_map(|event| match event {
            UploadProgress::Bytes { sent, total: t } => {
                assert_eq!(*t, total);
                Some(*sent)
            }
            _ => None,
        })
        .collect();
    assert!(sent.windows(2).all(|pair| pair[0] <= pair[1]), "progress went backwards: {sent:?}");
    assert_eq!(sent.last(), Some(&total));
    // 13 chunks of 256 KiB, each reported when it starts and when it is sent.
    assert!(sent.len() >= 26, "{sent:?}");

    assert_eq!(video.title, "Chunked upload");
    assert_eq!(video.privacy, Privacy::Unlisted);
    assert_eq!(video.upload_status, UploadStatus::Uploaded);
    assert_eq!(video.url, format!("https://www.youtube.com/watch?v={}", video.id));
    assert_eq!(video.channel_id, client.my_channel().unwrap().id);

    let uploads = emulators.uploads();
    assert_eq!(uploads.len(), 1);
    assert_eq!(uploads[0]["videoId"], video.id.as_str());
    assert_eq!(uploads[0]["bytes"], total);
    assert_eq!(uploads[0]["sha256"], sha256.as_str());
    assert_eq!(uploads[0]["contentType"], "video/mp4");
    assert_eq!(uploads[0]["snippet"]["tags"], json!(["rust", "desktop app"]));
    assert_eq!(uploads[0]["snippet"]["categoryId"], "28");
    assert_eq!(uploads[0]["status"]["selfDeclaredMadeForKids"], false);
}

#[test]
fn cancelled_upload_resumes_from_the_server_offset() {
    let Some(emulators) = Emulators::start() else {
        return;
    };
    let (credentials, _) = sign_in(&emulators, "creator@example.com");
    let client = client(&emulators, credentials);
    let dir = tempfile::tempdir().unwrap();
    let (path, sha256) = video_file(dir.path(), "take.mov", 3 * 1024 * 1024);
    let total = std::fs::metadata(&path).unwrap().len();

    let cancel = AtomicBool::new(false);
    let mut started = None;
    let result = client.upload_video(
        &path,
        &metadata("Resumed"),
        &mut |event| match event {
            UploadProgress::Started { session_url } => started = Some(session_url),
            UploadProgress::Bytes { sent, .. } if sent >= 1024 * 1024 => cancel.store(true, Ordering::Relaxed),
            _ => {}
        },
        &cancel,
    );
    let error = result.unwrap_err();
    let session_url = error.session_url().expect("cancel keeps the session").to_owned();
    assert!(matches!(error, Error::Cancelled { .. }));
    assert_eq!(Some(&session_url), started.as_ref());
    assert!(emulators.uploads().is_empty());

    let mut first_offset = None;
    let video = client
        .resume_upload(
            &session_url,
            &path,
            &mut |event| {
                if let UploadProgress::Bytes { sent, .. } = event {
                    first_offset.get_or_insert(sent);
                }
            },
            &AtomicBool::new(false),
        )
        .unwrap();
    let first_offset = first_offset.unwrap();
    assert!(first_offset >= 1024 * 1024 - UPLOAD_GRANULARITY && first_offset < total, "resumed at {first_offset}");
    assert_eq!(first_offset % UPLOAD_GRANULARITY, 0);

    let uploads = emulators.uploads();
    assert_eq!(uploads.len(), 1);
    assert_eq!(uploads[0]["videoId"], video.id.as_str());
    assert_eq!(uploads[0]["sha256"], sha256.as_str());
    assert_eq!(uploads[0]["contentType"], "video/quicktime");

    // Resuming a finished session just returns the video.
    let again = client.resume_upload(&session_url, &path, &mut |_| {}, &AtomicBool::new(false)).unwrap();
    assert_eq!(again.id, video.id);
}

#[test]
fn cancel_aborts_a_chunk_in_flight() {
    let Some(emulators) = Emulators::start() else {
        return;
    };
    let (credentials, _) = sign_in(&emulators, "creator@example.com");
    let client = client(&emulators, credentials)
        .with_upload_options(UploadOptions { chunk_size: 8 * UPLOAD_GRANULARITY, ..UploadOptions::default() });
    let dir = tempfile::tempdir().unwrap();
    let (path, sha256) = video_file(dir.path(), "long.mp4", 5 * 1024 * 1024);

    // Cancel inside the first 2 MiB chunk: the request is cut off mid-body.
    let cancel = AtomicBool::new(false);
    let error = client
        .upload_video(
            &path,
            &metadata("Cut off"),
            &mut |event| {
                if matches!(event, UploadProgress::Bytes { sent, .. } if sent >= 3 * UPLOAD_GRANULARITY) {
                    cancel.store(true, Ordering::Relaxed);
                }
            },
            &cancel,
        )
        .unwrap_err();
    let session_url = error.session_url().unwrap().to_owned();

    let mut first_offset = None;
    client
        .resume_upload(
            &session_url,
            &path,
            &mut |event| {
                if let UploadProgress::Bytes { sent, .. } = event {
                    first_offset.get_or_insert(sent);
                }
            },
            &AtomicBool::new(false),
        )
        .unwrap();
    let first_offset = first_offset.unwrap();
    assert!(first_offset < 8 * UPLOAD_GRANULARITY, "the first chunk was not aborted: {first_offset}");
    assert_eq!(first_offset % UPLOAD_GRANULARITY, 0);
    assert_eq!(emulators.uploads()[0]["sha256"], sha256.as_str());
}

#[test]
fn the_client_check_needs_a_reachable_sign_in_server() {
    let Some(emulators) = Emulators::start() else {
        return;
    };
    // The emulator knows every client: the made-up code is refused, the client is fine.
    check_client(&emulators.config()).unwrap();
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let nobody = YoutubeConfig::emulator(&format!("http://127.0.0.1:{port}"), &emulators.youtube_url);
    assert!(check_client(&nobody).unwrap_err().is_network());
}

#[test]
fn transient_failures_are_retried_from_the_server_offset() {
    let Some(emulators) = Emulators::start() else {
        return;
    };
    let (credentials, _) = sign_in(&emulators, "creator@example.com");
    let client = client(&emulators, credentials);
    let dir = tempfile::tempdir().unwrap();
    let (path, sha256) = video_file(dir.path(), "final.webm", 2 * 1024 * 1024 + 1);

    emulators.inject_fault(json!({"method": "POST", "path": "/upload/youtube/v3/videos", "status": 503}));
    emulators.inject_fault(json!({"method": "PUT", "path": "/upload/youtube/v3/videos", "status": 500, "times": 2}));
    let mut events = Vec::new();
    let video = client
        .upload_video(&path, &metadata("Flaky network"), &mut |event| events.push(event), &AtomicBool::new(false))
        .unwrap();
    let retries: Vec<(u32, RetryCause)> = events
        .iter()
        .filter_map(|event| match event {
            UploadProgress::Retrying { attempt, cause, .. } => Some((*attempt, *cause)),
            _ => None,
        })
        .collect();
    // Each failure is reported before its backoff, so the app can say what is going on.
    assert_eq!(retries, [(1, RetryCause::Server), (1, RetryCause::Server), (2, RetryCause::Server)]);
    // A failed chunk first steps back to what the server holds: the bytes that were
    // in flight are not progress while the upload retries. (The retry of the session
    // start comes first, with nothing before it.)
    let total = std::fs::metadata(&path).unwrap().len();
    let before_chunk_retries: Vec<&UploadProgress> = events
        .windows(2)
        .filter(|pair| matches!(pair[1], UploadProgress::Retrying { .. }))
        .map(|pair| &pair[0])
        .collect();
    assert_eq!(before_chunk_retries, [&UploadProgress::Bytes { sent: 0, total }; 2]);
    let uploads = emulators.uploads();
    assert_eq!(uploads[0]["videoId"], video.id.as_str());
    assert_eq!(uploads[0]["sha256"], sha256.as_str());

    // The server keeps only part of a chunk: the client continues from its Range.
    let client =
        client.with_upload_options(UploadOptions { chunk_size: 4 * UPLOAD_GRANULARITY, ..UploadOptions::default() });
    emulators.inject_fault(json!({"path": "/upload/youtube/v3/videos", "persistBytes": UPLOAD_GRANULARITY}));
    let mut sent = Vec::new();
    let video = client
        .upload_video(
            &path,
            &metadata("Partial chunk"),
            &mut |event| {
                if let UploadProgress::Bytes { sent: bytes, .. } = event {
                    sent.push(bytes);
                }
            },
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(sent.windows(2).any(|pair| pair[1] < pair[0]), "never resumed from the server offset: {sent:?}");
    assert_eq!(emulators.uploads()[1]["videoId"], video.id.as_str());
    assert_eq!(emulators.uploads()[1]["sha256"], sha256.as_str());

    emulators.inject_fault(
        json!({"method": "POST", "path": "/upload/youtube/v3/videos", "status": 403, "reason": "quotaExceeded"}),
    );
    let error = client.upload_video(&path, &metadata("No quota"), &mut |_| {}, &AtomicBool::new(false)).unwrap_err();
    assert!(matches!(&error, Error::Api { status: 403, reason, .. } if reason == "quotaExceeded"));
    assert!(error.to_string().contains("quota"));
}

#[test]
fn refreshes_expired_and_rejected_tokens() {
    let Some(emulators) = Emulators::start() else {
        return;
    };
    let (credentials, _) = sign_in(&emulators, "creator@example.com");
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(CredentialStore::new(dir.path().join("credentials.json")));
    store.save(&SavedLogin { credentials: credentials.clone(), account: None }).unwrap();

    // Expired: refreshed before the request, and persisted through the callback.
    let expired = Credentials { expires_at: Utc::now() - TimeDelta::minutes(5), ..credentials.clone() };
    let refreshes = Arc::new(AtomicUsize::new(0));
    let client = YoutubeClient::new(emulators.config(), expired, {
        let (store, refreshes) = (store.clone(), refreshes.clone());
        move |fresh| {
            refreshes.fetch_add(1, Ordering::SeqCst);
            store.save_credentials(fresh).unwrap();
        }
    });
    client.my_channel().unwrap();
    client.my_channel().unwrap();
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    let fresh = client.credentials();
    assert_ne!(fresh.access_token, credentials.access_token);
    assert_eq!(fresh.refresh_token, credentials.refresh_token);
    assert!(fresh.expires_at > Utc::now() + TimeDelta::minutes(50));
    assert_eq!(store.load().unwrap().unwrap().credentials, fresh);

    // Rejected with 401: refreshed once and retried; concurrent callers share one refresh.
    let rejected = Credentials { access_token: "revoked-token".to_owned(), ..credentials.clone() };
    let new_tokens = Arc::new(Mutex::new(Vec::new()));
    let client = Arc::new(YoutubeClient::new(emulators.config(), rejected, {
        let new_tokens = new_tokens.clone();
        move |fresh: &Credentials| new_tokens.lock().unwrap().push(fresh.access_token.clone())
    }));
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let client = client.clone();
            thread::spawn(move || client.my_channel().map(|channel| channel.title))
        })
        .collect();
    for worker in workers {
        assert_eq!(worker.join().unwrap().unwrap(), "Demo Creator");
    }
    assert_eq!(new_tokens.lock().unwrap().len(), 1);

    // No refresh token: the 401 surfaces and asks for a new sign-in.
    let stranded = Credentials { access_token: "revoked-token".to_owned(), refresh_token: None, ..credentials.clone() };
    let error = YoutubeClient::new(emulators.config(), stranded, |_| {}).my_channel().unwrap_err();
    assert!(matches!(&error, Error::Api { status: 401, reason, .. } if reason == "authError"), "{error:?}");
    assert!(error.needs_sign_in());

    // Revoking the grant kills the refresh token.
    client.revoke().unwrap();
    let error = refresh(&emulators.config(), &client.credentials()).unwrap_err();
    assert!(matches!(&error, Error::OAuth { error, .. } if error == "invalid_grant"), "{error:?}");
    assert!(error.needs_sign_in());
}

#[test]
fn changes_who_can_watch_an_uploaded_video() {
    let Some(emulators) = Emulators::start() else {
        return;
    };
    let (credentials, _) = sign_in(&emulators, "creator@example.com");
    assert!(credentials.has_scope(MANAGE_SCOPE), "the sign-in asks to change videos: {}", credentials.scope);
    let client = client(&emulators, credentials);
    let dir = tempfile::tempdir().unwrap();
    let (path, _) = video_file(dir.path(), "short.mp4", 4096);
    let metadata = VideoMetadata { made_for_kids: true, ..metadata("Made for kids") };
    let id = client.upload_video(&path, &metadata, &mut |_| {}, &AtomicBool::new(false)).unwrap().id;
    let status = |client: &YoutubeClient| -> Value {
        let url = format!("{}/youtube/v3/videos", emulators.youtube_url);
        let token = format!("Bearer {}", client.credentials().access_token);
        let mut response = plain_agent()
            .get(&url)
            .query("part", "status")
            .query("id", &id)
            .header("Authorization", &token)
            .call()
            .unwrap();
        let body: Value = response.body_mut().read_json().unwrap();
        body["items"][0]["status"].clone()
    };

    let public = Visibility { privacy: Privacy::Public, publish_at: None };
    assert_eq!(client.set_visibility(&id, public).unwrap(), Some(public));
    assert_eq!(client.get_video(&id).unwrap().unwrap().privacy, Privacy::Public);

    let at = (Utc::now() + TimeDelta::days(2)).with_nanosecond(0).unwrap();
    let scheduled = Visibility { privacy: Privacy::Private, publish_at: Some(at) };
    assert_eq!(client.set_visibility(&id, scheduled).unwrap(), Some(scheduled));
    let info = client.get_video(&id).unwrap().unwrap();
    assert_eq!((info.privacy, info.publish_at), (Privacy::Private, Some(at)));
    // The rest of the status stays as it was.
    assert_eq!(status(&client)["selfDeclaredMadeForKids"], true);

    let unlisted = Visibility { privacy: Privacy::Unlisted, publish_at: None };
    assert_eq!(client.set_visibility(&id, unlisted).unwrap(), Some(unlisted));
    assert_eq!(status(&client).get("publishAt"), None, "no longer scheduled");

    // A schedule YouTube would refuse costs no quota.
    let public_later = Visibility { privacy: Privacy::Public, publish_at: Some(at) };
    assert!(matches!(client.set_visibility(&id, public_later), Err(Error::InvalidInput(_))));
    let past = Visibility { privacy: Privacy::Private, publish_at: Some(Utc::now() - TimeDelta::hours(1)) };
    assert!(matches!(client.set_visibility(&id, past), Err(Error::InvalidInput(_))));
    assert_eq!(client.set_visibility("doesNotExist", public).unwrap(), None);

    // Only the channel's owner changes its videos.
    let (other, _) = sign_in(&emulators, "editor@example.com");
    let other = self::client(&emulators, other);
    assert_eq!(other.set_visibility(&id, public).unwrap_err().api_message(), None);
}

#[test]
fn changes_the_details_of_an_uploaded_video() {
    let Some(emulators) = Emulators::start() else {
        return;
    };
    let (credentials, _) = sign_in(&emulators, "creator@example.com");
    let client = client(&emulators, credentials);
    let dir = tempfile::tempdir().unwrap();
    let (path, _) = video_file(dir.path(), "short.mp4", 4096);
    let metadata = VideoMetadata {
        description: "First description".into(),
        tags: vec!["rust".into()],
        category_id: "28".into(),
        default_language: Some("en".into()),
        ..metadata("First title")
    };
    let uploaded = client.upload_video(&path, &metadata, &mut |_| {}, &AtomicBool::new(false)).unwrap();
    let id = uploaded.id.clone();
    let first = Details {
        title: "First title".into(),
        description: "First description".into(),
        tags: vec!["rust".into()],
        category_id: "28".into(),
    };
    assert_eq!(uploaded.details(), first);
    let snippet = |client: &YoutubeClient| -> Value {
        let url = format!("{}/youtube/v3/videos", emulators.youtube_url);
        let token = format!("Bearer {}", client.credentials().access_token);
        let mut response = plain_agent()
            .get(&url)
            .query("part", "snippet")
            .query("id", &id)
            .header("Authorization", &token)
            .call()
            .unwrap();
        let body: Value = response.body_mut().read_json().unwrap();
        body["items"][0]["snippet"].clone()
    };

    // Only the description: the rest stays as YouTube has it.
    let description = DetailsChange { description: Some("Second description".into()), ..Default::default() };
    let second = Details { description: "Second description".into(), ..first.clone() };
    assert_eq!(client.set_details(&id, &description).unwrap(), Some(second.clone()));
    assert_eq!(client.get_video(&id).unwrap().unwrap().details(), second);
    assert_eq!(snippet(&client)["defaultLanguage"], "en", "the language stays");

    let all = DetailsChange {
        title: Some("Third title".into()),
        description: Some(String::new()),
        tags: Some(Vec::new()),
        category_id: Some("27".into()),
    };
    let third =
        Details { title: "Third title".into(), description: String::new(), tags: Vec::new(), category_id: "27".into() };
    assert_eq!(client.set_details(&id, &all).unwrap(), Some(third.clone()));
    assert_eq!(client.get_video(&id).unwrap().unwrap().details(), third);
    // What YouTube has already sends nothing more.
    assert_eq!(client.set_details(&id, &all).unwrap(), Some(third));

    // Details YouTube would refuse cost no quota.
    let long = DetailsChange { title: Some("x".repeat(101)), ..Default::default() };
    assert!(matches!(client.set_details(&id, &long), Err(Error::InvalidInput(_))));
    assert_eq!(client.set_details("doesNotExist", &description).unwrap(), None);

    // Only the channel's owner changes its videos.
    let (other, _) = sign_in(&emulators, "editor@example.com");
    let other = self::client(&emulators, other);
    assert!(other.set_details(&id, &description).is_err());
}

#[test]
fn thumbnails_processing_and_uploads_list() {
    let Some(emulators) = Emulators::start() else {
        return;
    };
    let (credentials, _) = sign_in(&emulators, "creator@example.com");
    let client = client(&emulators, credentials);
    let dir = tempfile::tempdir().unwrap();
    let (path, _) = video_file(dir.path(), "short.mp4", 4096);

    let mut ids = Vec::new();
    for index in 0..52 {
        let video = client
            .upload_video(&path, &metadata(&format!("Video {index}")), &mut |_| {}, &AtomicBool::new(false))
            .unwrap();
        ids.push(video.id);
    }
    let newest = ids.last().unwrap().clone();
    let info = client.get_video(&newest).unwrap().unwrap();
    assert_eq!(info.processing_status, Some(ProcessingStatus::Processing));
    assert_eq!(info.upload_status, UploadStatus::Uploaded);

    let png = STANDARD.decode(PNG_BASE64).unwrap();
    let thumbnail_path = dir.path().join("thumbnail.png");
    std::fs::write(&thumbnail_path, &png).unwrap();
    let thumbnail_url = client.set_thumbnail(&newest, &thumbnail_path).unwrap();
    let served = plain_agent().get(&thumbnail_url).call().unwrap().body_mut().read_to_vec().unwrap();
    assert_eq!(served, png);
    // Channels without phone verification may not set custom thumbnails.
    emulators.inject_fault(
        json!({"method": "POST", "path": "/upload/youtube/v3/thumbnails/set", "status": 403, "reason": "forbidden"}),
    );
    assert!(matches!(client.set_thumbnail(&newest, &thumbnail_path), Err(Error::ThumbnailsNotAllowed)));
    let not_image = dir.path().join("thumbnail.gif");
    std::fs::write(&not_image, b"GIF89a").unwrap();
    assert!(matches!(client.set_thumbnail(&newest, &not_image), Err(Error::InvalidInput(_))));

    let mut info = client.get_video(&newest).unwrap().unwrap();
    assert_eq!(info.title, "Video 51");
    assert_eq!(info.privacy, Privacy::Unlisted);
    assert_eq!(info.thumbnail.as_deref(), Some(thumbnail_url.as_str()));
    let deadline = Instant::now() + Duration::from_millis(PROCESSING_MS * 5);
    while info.processing_status != Some(ProcessingStatus::Succeeded) {
        assert!(Instant::now() < deadline, "processing never finished");
        thread::sleep(Duration::from_millis(200));
        info = client.get_video(&newest).unwrap().unwrap();
    }
    assert_eq!(info.upload_status, UploadStatus::Processed);
    assert_eq!(client.get_video("doesNotExist").unwrap(), None);

    // Two playlist pages (50 + 2), newest first.
    let listed: Vec<String> = client.list_my_uploads(100).unwrap().into_iter().map(|video| video.id).collect();
    let mut expected = ids.clone();
    expected.reverse();
    assert_eq!(listed, expected);
    let latest: Vec<String> = client.list_my_uploads(3).unwrap().into_iter().map(|video| video.id).collect();
    assert_eq!(latest, expected[..3]);
}
