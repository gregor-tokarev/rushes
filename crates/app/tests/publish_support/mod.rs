//! Shared fixtures of the publish tests: an emulator pair, a workspace on a temp
//! data dir, and a scripted browser for the consent page.

// Each test binary uses a different subset of these helpers.
#![allow(dead_code)]

use std::collections::HashMap;
use std::fs::File;
use std::io::Write as _;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use gpui_kit::{AppContext as _, Entity, TestAppContext};
use serde_json::Value;
use sha2::{Digest, Sha256};
use videocrm::publish::{Connection, YoutubeService};
use videocrm::state::AppState;
use videocrm_core::settings::{AppSettings, YoutubeBackend};
use videocrm_core::{NewVideo, Store, UploadJob, Video, VideoId};

/// YouTube marks uploads processed this long after they finish.
const PROCESSING_MS: u64 = 300;
/// A 1×1 PNG.
const PNG_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

/// A running Google + YouTube emulator pair on free ports, stopped on drop.
pub struct Emulators {
    child: Child,
    pub google_url: String,
    pub youtube_url: String,
    // Spool directory of the YouTube emulator.
    _tmp: tempfile::TempDir,
}

impl Emulators {
    /// `None` (with a notice) when Node or the emulator dependencies are missing.
    pub fn start() -> Option<Self> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../emulators");
        if !dir.join("node_modules/emulate").is_dir() {
            eprintln!("skipping emulator test: run `npm ci` in {} first", dir.display());
            return None;
        }
        if Command::new("node").arg("--version").output().is_err() {
            eprintln!("skipping emulator test: `node` is not installed");
            return None;
        }
        // Ports are probed then released, so another process may take one; retry then.
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
        let ready = |url: String| agent().get(&url).call().is_ok_and(|r| r.status() == 200);
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

    pub fn backend(&self) -> YoutubeBackend {
        YoutubeBackend::Emulator { google_url: self.google_url.clone(), youtube_url: self.youtube_url.clone() }
    }

    /// Completed uploads as the YouTube emulator recorded them.
    pub fn uploads(&self) -> Vec<Value> {
        let body = agent()
            .get(&format!("{}/_emulator/uploads", self.youtube_url))
            .call()
            .unwrap()
            .body_mut()
            .read_to_string()
            .unwrap();
        let body: Value = serde_json::from_str(&body).unwrap();
        body["uploads"].as_array().unwrap().clone()
    }

    pub fn inject_fault(&self, fault: Value) {
        let response = agent()
            .post(&format!("{}/_emulator/faults", self.youtube_url))
            .header("Content-Type", "application/json")
            .send(fault.to_string())
            .unwrap();
        assert_eq!(response.status(), 201);
    }

    /// Deletes a video the way YouTube Studio does, as the account `token` belongs to.
    pub fn delete_video(&self, token: &str, youtube_id: &str) {
        let response = agent()
            .delete(&format!("{}/youtube/v3/videos?id={youtube_id}", self.youtube_url))
            .header("Authorization", format!("Bearer {token}"))
            .call()
            .unwrap();
        assert_eq!(response.status(), 204);
    }

    pub fn clear_faults(&self) {
        let response = agent().delete(&format!("{}/_emulator/faults", self.youtube_url)).call().unwrap();
        assert!(response.status().is_success());
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

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(10)))
        .build()
        .into()
}

/// What a browser does on the emulator's consent page: submit the account's form and
/// follow the redirect to the app's loopback server. Returns that page.
pub fn consent_in_browser(authorize_url: &str, email: &str) -> String {
    try_consent_in_browser(authorize_url, email).unwrap()
}

pub fn try_consent_in_browser(authorize_url: &str, email: &str) -> Result<String, ureq::Error> {
    let agent = agent();
    let html = agent.get(authorize_url).call()?.body_mut().read_to_string()?;
    let (action, fields) = html
        .split("<form")
        .skip(1)
        .map(parse_form)
        .find(|(_, fields)| fields.get("email").map(String::as_str) == Some(email))
        .unwrap_or_else(|| panic!("no consent form for {email}"));
    let action = url::Url::parse(authorize_url).unwrap().join(&action).unwrap();
    let fields: Vec<(&str, &str)> = fields.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let response = agent.post(action.as_str()).send_form(fields)?;
    assert_eq!(response.status(), 302);
    let location = response.headers()["location"].to_str().unwrap().to_owned();
    agent.get(&location).call()?.body_mut().read_to_string()
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

/// Runs the app's tasks until `check` returns a value. Workers are real threads, so
/// this waits in real time as well.
pub fn wait_until<T>(
    cx: &mut TestAppContext,
    what: &str,
    mut check: impl FnMut(&mut TestAppContext) -> Option<T>,
) -> T {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        cx.run_until_parked();
        if let Some(value) = check(cx) {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(20));
    }
}

/// A one-second test pattern clip when ffmpeg is installed, otherwise arbitrary bytes:
/// YouTube's emulator does not decode uploads.
pub fn video_file(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    let encoded = Command::new("ffmpeg")
        .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=320x180:rate=15:duration=1"])
        .args(["-pix_fmt", "yuv420p"])
        .arg(&path)
        .status()
        .is_ok_and(|status| status.success());
    if !encoded {
        let bytes: Vec<u8> = (0..300_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
        File::create(&path).unwrap().write_all(&bytes).unwrap();
    }
    path
}

pub fn png_file(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, STANDARD.decode(PNG_BASE64).unwrap()).unwrap();
    path
}

pub fn sha256(path: &Path) -> String {
    Sha256::digest(std::fs::read(path).unwrap()).iter().map(|byte| format!("{byte:02x}")).collect()
}

/// An app state over a database, credentials and media in one temp directory.
pub struct Workspace {
    pub dir: tempfile::TempDir,
    pub state: Entity<AppState>,
    pub service: Entity<YoutubeService>,
}

impl Workspace {
    pub fn open(cx: &mut TestAppContext, backend: YoutubeBackend) -> Self {
        // Sign-in and upload workers are real threads that wake app tasks.
        cx.executor().allow_parking();
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().join("videocrm.db")).unwrap();
        let settings = AppSettings {
            youtube: backend,
            media_root: Some(dir.path().join("media")),
            onboarding_completed: true,
            ..AppSettings::default()
        };
        store.save_settings(&settings).unwrap();
        let state = AppState::new(store).unwrap();
        let data_dir = dir.path().to_owned();
        let (state, service) = cx.update(|cx| {
            gpui_kit::init(cx);
            let state = cx.new(|_| state);
            AppState::set_global(state.clone(), cx);
            let service = YoutubeService::init_in(state.clone(), data_dir, cx);
            (state, service)
        });
        Self { dir, state, service }
    }

    /// A second service over the same data, as after an app restart.
    pub fn restart(&self, cx: &mut TestAppContext) -> Entity<YoutubeService> {
        let data_dir = self.dir.path().to_owned();
        let state = self.state.clone();
        cx.update(|cx| YoutubeService::init_in(state, data_dir, cx))
    }

    /// Signs in through the emulator's consent page.
    pub fn sign_in(&self, cx: &mut TestAppContext, email: &str) {
        self.service.update(cx, |service, cx| service.connect(cx));
        let url =
            wait_until(cx, "the consent page", |cx| match self.service.read_with(cx, |s, _| s.connection().clone()) {
                Connection::Connecting { authorize_url: Some(url) } => Some(url.to_string()),
                _ => None,
            });
        consent_in_browser(&url, email);
        wait_until(cx, "the connection", |cx| self.service.read_with(cx, |s, _| s.is_connected().then_some(())));
    }

    pub fn create_video(&self, cx: &mut TestAppContext, title: &str, edit: impl FnOnce(&mut Video)) -> VideoId {
        self.state.update(cx, |state, cx| {
            let id = state.create_video(NewVideo { title: title.to_owned(), ..NewVideo::default() }, cx).unwrap();
            let mut video = state.video(id).unwrap().clone();
            edit(&mut video);
            state.update_video(video, cx);
            id
        })
    }

    pub fn video(&self, cx: &mut TestAppContext, id: VideoId) -> Video {
        self.state.read_with(cx, |state, _| state.video(id).cloned()).unwrap()
    }

    pub fn stored_jobs(&self, cx: &mut TestAppContext) -> Vec<UploadJob> {
        let store = self.state.read_with(cx, |state, _| state.store());
        store.lock().unwrap().list_uploads(None).unwrap()
    }

    pub fn insert_jobs(&self, cx: &mut TestAppContext, jobs: &[&UploadJob]) {
        let store = self.state.read_with(cx, |state, _| state.store());
        let mut store = store.lock().unwrap();
        for job in jobs {
            store.insert_upload(job).unwrap();
        }
    }
}
