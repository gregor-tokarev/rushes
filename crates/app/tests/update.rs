//! UI integration tests for updates: the real `Workspace` with an
//! `UpdateService` whose updater asks a local stand-in for GitHub and installs
//! into a temp folder.

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, ElementId, Entity, Focusable as _, TestAppContext, WindowBounds,
    WindowOptions, px, size,
};
use sha2::{Digest as _, Sha256};
use videocrm::ai_edit::PipelineService;
use videocrm::publish::YoutubeService;
use videocrm::route::{Route, SettingsSection};
use videocrm::shell::{self, Workspace};
use videocrm::state::AppState;
use videocrm::update::{UpdateService, UpdateStatus};
use videocrm_core::Store;
use videocrm_core::settings::AppSettings;
use videocrm_update::{Installation, ManualReason, Updater, Version};

/// Status and body by request path.
type Routes = Arc<Mutex<HashMap<String, (u16, Vec<u8>)>>>;

/// Answers like GitHub: the newest release at `/latest`, its files under `/download/`.
struct FakeGithub {
    base: String,
    routes: Routes,
}

impl FakeGithub {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("address"));
        let routes = Routes::default();
        let served = routes.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
                let mut request_line = String::new();
                if reader.read_line(&mut request_line).is_err() {
                    continue;
                }
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok_and(|read| read > 2) {
                    line.clear();
                }
                let path = request_line.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let (status, body) = served.lock().expect("routes").get(&path).cloned().unwrap_or((404, Vec::new()));
                let head =
                    format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                _ = stream.write_all(head.as_bytes()).and_then(|()| stream.write_all(&body));
            }
        });
        Self { base, routes }
    }

    fn latest_url(&self) -> String {
        format!("{}/latest", self.base)
    }

    /// Publishes release `version` with one file.
    fn publish(&self, version: &str, name: &str, bytes: Vec<u8>) {
        let digest: String = Sha256::digest(&bytes).iter().map(|byte| format!("{byte:02x}")).collect();
        let release = serde_json::json!({
            "tag_name": format!("v{version}"),
            "html_url": format!("https://github.com/gregor-tokarev/videocrm/releases/tag/v{version}"),
            "body": "Notes",
            "assets": [{
                "name": name,
                "size": bytes.len(),
                "digest": format!("sha256:{digest}"),
                "browser_download_url": format!("{}/download/{name}", self.base),
            }],
        });
        let mut routes = self.routes.lock().expect("routes");
        routes.insert("/latest".into(), (200, release.to_string().into_bytes()));
        routes.insert(format!("/download/{name}"), (200, bytes));
    }
}

/// An AppImage's first bytes (type 2), then `payload`.
fn appimage(payload: &[u8]) -> Vec<u8> {
    let mut bytes = b"\x7fELF\x02\x01\x01\x00AI\x02".to_vec();
    bytes.extend_from_slice(payload);
    bytes
}

fn version(text: &str) -> Version {
    Version::parse(text).expect("version")
}

struct Fixture {
    window: AnyWindowHandle,
    state: Entity<AppState>,
    service: Entity<UpdateService>,
    /// The AppImage the updater replaces, when the copy is installed.
    appimage: Option<PathBuf>,
    _dir: tempfile::TempDir,
}

impl Fixture {
    /// A workspace running version 0.1.1, installed as an AppImage in a temp
    /// folder (`installed`) or built from source, with automatic updates `automatic`.
    fn open(cx: &mut TestAppContext, github: &FakeGithub, installed: bool, automatic: bool) -> Self {
        // The updater's HTTP calls wait on the fake GitHub's thread.
        cx.executor().allow_parking();
        cx.update(|cx| {
            gpui_kit::init(cx);
            shell::init(cx);
            cx.set_reduce_motion(true);
        });
        let dir = tempfile::tempdir().expect("temp dir");
        let appimage = installed.then(|| {
            let file = dir.path().join("VideoCRM.AppImage");
            std::fs::write(&file, appimage(b"0.1.1")).expect("old AppImage");
            file
        });
        let installation = appimage.clone().map(|file| Installation::AppImage { file });
        let updater = Updater::new(github.latest_url(), version("0.1.1"), installation);

        let mut store = Store::open_in_memory().expect("in-memory store");
        let mut settings = AppSettings {
            media_root: Some(dir.path().join("media")),
            onboarding_completed: true,
            ..AppSettings::default()
        };
        settings.updates.automatic = automatic;
        store.save_settings(&settings).expect("save settings");
        let state = AppState::new(store).expect("load state");

        let (window, state, service) = cx.update(|cx| {
            let state = cx.new(|_| state);
            AppState::set_global(state.clone(), cx);
            YoutubeService::init(state.clone(), cx);
            PipelineService::init(state.clone(), cx);
            let service = UpdateService::init_with(state.clone(), updater, cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(Default::default(), size(px(1440.), px(900.))))),
                ..Default::default()
            };
            let handle = state.clone();
            let (window, workspace) =
                gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| Workspace::new(handle, window, cx)))
                    .expect("open test window");
            cx.update_window(window, |_, window, cx| workspace.read(cx).focus_handle(cx).focus(window, cx))
                .expect("window is open");
            (window, state, service)
        });
        let fixture = Self { window, state, service, appimage, _dir: dir };
        fixture.settle(cx);
        fixture
    }

    fn settle(&self, cx: &mut TestAppContext) {
        cx.run_until_parked();
        cx.update_window(self.window, |_, window, cx| window.render_frame(cx)).expect("window is open");
    }

    fn navigate(&self, cx: &mut TestAppContext, route: Route) {
        cx.update(|cx| self.state.update(cx, |state, cx| state.navigate(route, cx)));
        self.settle(cx);
    }

    fn click(&self, cx: &mut TestAppContext, id: impl Into<ElementId>) {
        let id = id.into();
        cx.update_window(self.window, |_, window, cx| window.click(id, cx)).expect("window is open");
        self.settle(cx);
    }

    fn shows(&self, cx: &mut TestAppContext, id: impl Into<ElementId>) -> bool {
        let id = id.into();
        cx.update_window(self.window, |_, window, _| window.try_find(id).is_some_and(|element| element.visible()))
            .expect("window is open")
    }

    fn notifications(&self, cx: &mut TestAppContext) -> usize {
        cx.update_window(self.window, |_, window, cx| window.notifications(cx).len()).expect("window is open")
    }

    fn status(&self, cx: &mut TestAppContext) -> UpdateStatus {
        cx.read(|cx| self.service.read(cx).status().clone())
    }

    fn installed(&self, cx: &mut TestAppContext) -> Option<Version> {
        cx.read(|cx| self.service.read(cx).installed().cloned())
    }

    /// Lets the update's background work finish, then redraws.
    fn wait_until(&self, cx: &mut TestAppContext, what: &str, mut done: impl FnMut(&mut TestAppContext) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            cx.run_until_parked();
            if done(cx) {
                break;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(20));
        }
        self.settle(cx);
    }
}

fn appimage_name(version: &str) -> String {
    format!("VideoCRM-{version}-{}.AppImage", std::env::consts::ARCH)
}

fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).expect("read")
}

#[gpui_kit::test]
fn checking_by_hand_installs_the_new_version_and_offers_a_restart(cx: &mut TestAppContext) {
    let github = FakeGithub::start();
    github.publish("0.2.0", &appimage_name("0.2.0"), appimage(&vec![2; 64 * 1024]));
    let app = Fixture::open(cx, &github, true, false);
    app.navigate(cx, Route::Settings { section: SettingsSection::About });
    assert_eq!(app.status(cx), UpdateStatus::Idle);
    assert!(!app.shows(cx, "restart-to-update"), "nothing to restart into yet");

    app.click(cx, "check-for-updates");
    app.wait_until(cx, "the update", |cx| app.installed(cx).is_some());

    assert_eq!(app.installed(cx), Some(version("0.2.0")));
    assert!(matches!(app.status(cx), UpdateStatus::UpToDate { .. }), "{:?}", app.status(cx));
    let file = app.appimage.as_deref().expect("installed copy");
    assert_eq!(read(file), appimage(&vec![2; 64 * 1024]), "the AppImage is the new version");
    assert!(app.shows(cx, "restart-to-update"), "the title bar offers the restart");
    assert!(app.shows(cx, "restart-to-update-settings"), "so does Settings");
    assert_eq!(app.notifications(cx), 1, "a notification says it's ready");

    // A later (automatic) check finds nothing newer than what's installed.
    cx.update(|cx| app.service.update(cx, |service, cx| service.check_now(cx)));
    app.wait_until(cx, "the second check", |cx| !matches!(app.status(cx), UpdateStatus::Checking));
    assert!(matches!(app.status(cx), UpdateStatus::UpToDate { .. }), "{:?}", app.status(cx));
    assert_eq!(app.notifications(cx), 1, "and doesn't announce it again");
}

#[gpui_kit::test]
fn automatic_updates_start_a_minute_after_launch(cx: &mut TestAppContext) {
    let github = FakeGithub::start();
    github.publish("0.2.0", &appimage_name("0.2.0"), appimage(b"0.2.0"));
    let app = Fixture::open(cx, &github, true, true);

    cx.executor().advance_clock(Duration::from_secs(50));
    app.settle(cx);
    assert_eq!(app.status(cx), UpdateStatus::Idle, "not while VideoCRM starts");

    cx.executor().advance_clock(Duration::from_secs(15));
    app.wait_until(cx, "the automatic update", |cx| app.installed(cx).is_some());
    assert_eq!(read(app.appimage.as_deref().expect("installed copy")), appimage(b"0.2.0"));
    assert!(app.shows(cx, "restart-to-update"));
}

#[gpui_kit::test]
fn turning_automatic_updates_off_stops_the_checks(cx: &mut TestAppContext) {
    let github = FakeGithub::start();
    github.publish("0.2.0", &appimage_name("0.2.0"), appimage(b"0.2.0"));
    let app = Fixture::open(cx, &github, true, true);
    cx.update(|cx| {
        app.state.update(cx, |state, cx| state.update_settings(cx, |settings| settings.updates.automatic = false))
    });

    cx.executor().advance_clock(Duration::from_secs(5 * 60 * 60));
    app.settle(cx);
    assert_eq!(app.status(cx), UpdateStatus::Idle);
    assert_eq!(read(app.appimage.as_deref().expect("installed copy")), appimage(b"0.1.1"));
}

#[gpui_kit::test]
fn a_copy_built_from_source_checks_only_when_asked_and_links_to_the_release(cx: &mut TestAppContext) {
    let github = FakeGithub::start();
    github.publish("0.2.0", &appimage_name("0.2.0"), appimage(b"0.2.0"));
    let app = Fixture::open(cx, &github, false, true);

    cx.executor().advance_clock(Duration::from_secs(5 * 60 * 60));
    app.settle(cx);
    assert_eq!(app.status(cx), UpdateStatus::Idle, "no automatic checks without an installation");

    app.navigate(cx, Route::Settings { section: SettingsSection::About });
    app.click(cx, "check-for-updates");
    app.wait_until(cx, "the check", |cx| !matches!(app.status(cx), UpdateStatus::Checking));

    assert_eq!(
        app.status(cx),
        UpdateStatus::Available {
            version: version("0.2.0"),
            page_url: "https://github.com/gregor-tokarev/videocrm/releases/tag/v0.2.0".into(),
            reason: ManualReason::NotInstalled,
        }
    );
    assert!(app.shows(cx, "update-download"), "Settings links to the release");
    assert_eq!(app.installed(cx), None);
    assert_eq!(app.notifications(cx), 0, "a check by hand answers where it was asked");
}

#[gpui_kit::test]
fn a_failed_check_offers_to_try_again(cx: &mut TestAppContext) {
    let github = FakeGithub::start();
    let app = Fixture::open(cx, &github, true, false);
    app.navigate(cx, Route::Settings { section: SettingsSection::About });

    // Nothing published: GitHub answers 404.
    app.click(cx, "check-for-updates");
    app.wait_until(cx, "the check", |cx| !matches!(app.status(cx), UpdateStatus::Checking));
    let UpdateStatus::Failed(failure) = app.status(cx) else { panic!("{:?}", app.status(cx)) };
    assert_eq!(failure.title, "Couldn’t check for updates");
    assert_eq!(app.notifications(cx), 0);

    github.publish("0.1.1", &appimage_name("0.1.1"), appimage(b"0.1.1"));
    app.click(cx, "check-for-updates");
    app.wait_until(cx, "the check", |cx| !matches!(app.status(cx), UpdateStatus::Checking));
    assert!(matches!(app.status(cx), UpdateStatus::UpToDate { .. }), "{:?}", app.status(cx));
}
