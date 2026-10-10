//! The app state over a database file that another program locks: the real
//! `Workspace` keeps responding, edits wait in the write queue and land once
//! the lock ends, also across a quit.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, ElementId, Entity, Focusable as _, TestAppContext, Window,
    WindowBounds, WindowOptions, px, size,
};
use rusqlite::Connection;
use videocrm::ai_edit::PipelineService;
use videocrm::publish::YoutubeService;
use videocrm::route::VideoTab;
use videocrm::shell::{self, Workspace};
use videocrm::state::{AppEvent, AppState, QUIT_SAVE_TIMEOUT, SAVE_RETRY_DELAY};
use videocrm::video_page::SAVE_DELAY;
use videocrm_core::settings::AppSettings;
use videocrm_core::templates::TemplateKind;
use videocrm_core::{
    BlockId, DocumentKind, NewVideo, PropertyDef, PropertyId, PropertyKind, PropertyValue, SelectOption, Store,
    TagColor, VideoId,
};

/// Longest a UI-thread call may take while the database is locked. The UI
/// connection gives up on a lock after 100 ms; before the fix it waited 5 s.
const PROMPT: Duration = Duration::from_millis(500);

struct Fixture {
    window: AnyWindowHandle,
    workspace: Entity<Workspace>,
    state: Entity<AppState>,
    database: PathBuf,
    _dir: tempfile::TempDir,
}

impl Fixture {
    /// A workspace over a database file holding `titles` as videos.
    fn open(cx: &mut TestAppContext, titles: &[&str]) -> Self {
        cx.update(|cx| {
            gpui_kit::init(cx);
            shell::init(cx);
            cx.set_reduce_motion(true);
        });
        let dir = tempfile::tempdir().expect("temp data folder");
        let database = dir.path().join("videocrm.db");
        let mut store = Store::open(&database).expect("database file");
        let settings = AppSettings {
            media_root: Some(dir.path().join("media")),
            onboarding_completed: true,
            ..AppSettings::default()
        };
        store.save_settings(&settings).expect("save settings");
        for title in titles {
            let new = NewVideo { title: title.to_string(), template: TemplateKind::Blank, ..NewVideo::default() };
            store.create_video(new).expect("create video");
        }
        let state = AppState::new(store).expect("load state");
        let (window, workspace, state) = cx.update(|cx| {
            let state = cx.new(|_| state);
            AppState::set_global(state.clone(), cx);
            YoutubeService::init(state.clone(), cx);
            PipelineService::init(state.clone(), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(Default::default(), size(px(1440.), px(900.))))),
                ..Default::default()
            };
            let handle = state.clone();
            let (window, workspace) =
                gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| Workspace::new(handle, window, cx)))
                    .expect("open test window");
            (window, workspace, state)
        });
        let fixture = Self { window, workspace, state, database, _dir: dir };
        fixture.step(cx, |window, _| window.activate_window());
        let workspace = fixture.workspace.clone();
        fixture.step(cx, move |window, cx| workspace.read(cx).focus_handle(cx).focus(window, cx));
        fixture
    }

    fn step(&self, cx: &mut TestAppContext, interact: impl FnOnce(&mut Window, &mut App)) {
        cx.update_window(self.window, |_, window, cx| interact(window, cx)).expect("window is open");
        cx.run_until_parked();
        cx.update_window(self.window, |_, window, cx| window.render_frame(cx)).expect("window is open");
    }

    fn wait(&self, cx: &mut TestAppContext, duration: Duration) {
        cx.executor().advance_clock(duration);
        self.step(cx, |_, _| {});
    }

    fn click(&self, cx: &mut TestAppContext, id: impl Into<ElementId>) {
        let id = id.into();
        self.step(cx, move |window, cx| window.click(id, cx));
    }

    /// Runs `f` on the UI thread without running the background executor, and
    /// says how long it took.
    fn timed<T>(&self, cx: &mut TestAppContext, f: impl FnOnce(&mut Window, &mut App) -> T) -> (T, Duration) {
        let started = Instant::now();
        let value = cx.update_window(self.window, |_, window, cx| f(window, cx)).expect("window is open");
        (value, started.elapsed())
    }

    fn video_id(&self, cx: &mut TestAppContext, title: &str) -> VideoId {
        cx.read(|cx| {
            self.state.read(cx).rows().iter().find(|row| row.video.title == title).expect("video exists").video.id
        })
    }

    fn has(&self, cx: &mut TestAppContext, id: &'static str) -> bool {
        cx.update_window(self.window, |_, window, _| window.try_find(id).is_some()).expect("window is open")
    }

    fn notifications(&self, cx: &mut TestAppContext) -> usize {
        cx.update_window(self.window, |_, window, cx| window.notifications(cx).len()).expect("window is open")
    }

    /// Collects the errors the app reports from now on.
    fn errors(&self, cx: &mut TestAppContext) -> Rc<RefCell<Vec<String>>> {
        let errors = Rc::new(RefCell::new(Vec::new()));
        let sink = errors.clone();
        cx.update(|cx| {
            cx.subscribe(&self.state, move |_, event: &AppEvent, _| match event {
                AppEvent::Error(message) => sink.borrow_mut().push(message.to_string()),
                AppEvent::Problem(problem) => {
                    sink.borrow_mut().push(format!("{}: {}", problem.title(), problem.message()))
                }
                _ => {}
            })
            .detach()
        });
        errors
    }

    fn page_video(&self, cx: &mut TestAppContext) -> Option<VideoId> {
        cx.read(|cx| self.workspace.read(cx).video_page().map(|page| page.read(cx).video_id()))
    }

    fn script_blocks(&self, cx: &mut TestAppContext) -> Vec<BlockId> {
        cx.read(|cx| {
            let page = self.workspace.read(cx).video_page().expect("video page").read(cx);
            let editor = page.script().expect("script tab").read(cx).editor().cloned().expect("script editor");
            editor.read(cx).document(cx).blocks.iter().map(|block| block.id).collect()
        })
    }
}

/// Another program holding the database's write lock, as a backup or sync
/// tool does: `BEGIN EXCLUSIVE` until released.
struct Locker(Option<Connection>);

impl Locker {
    fn hold(database: &Path) -> Self {
        let connection = Connection::open(database).expect("second connection");
        connection.execute_batch("BEGIN EXCLUSIVE").expect("lock the database");
        Self(Some(connection))
    }

    fn release(&mut self) {
        if let Some(connection) = self.0.take() {
            connection.execute_batch("COMMIT").expect("release the lock");
        }
    }
}

impl Drop for Locker {
    fn drop(&mut self) {
        self.release();
    }
}

/// A fresh connection's view of the database, as another program sees it.
fn stored<T>(database: &Path, read: impl FnOnce(&Store) -> T) -> T {
    read(&Store::open(database).expect("open the database"))
}

fn stored_property(database: &Path, video: VideoId, id: PropertyId) -> Option<PropertyValue> {
    stored(database, |store| store.get_video(video).expect("read video").expect("video").properties.get(&id).cloned())
}

fn stored_script(database: &Path, video: VideoId) -> String {
    stored(database, |store| {
        let documents = store.list_documents(video).expect("list documents");
        let script = documents.into_iter().find(|document| document.kind == DocumentKind::Script).expect("script");
        script.blocks[0].text.clone()
    })
}

fn priority_property() -> PropertyDef {
    let option = |id: &str, name: &str, color| SelectOption { id: id.into(), name: name.into(), color };
    PropertyDef {
        id: PropertyId::new(),
        name: "Priority".into(),
        kind: PropertyKind::Select {
            options: vec![option("high", "High", TagColor::Red), option("low", "Low", TagColor::Gray)],
        },
        position: 0,
    }
}

fn block_part(id: BlockId, name: &'static str) -> ElementId {
    ElementId::NamedChild(Arc::new(ElementId::Uuid(id.0)), name.into())
}

fn set_priority(state: &Entity<AppState>, video: VideoId, priority: PropertyId, option: &str, cx: &mut App) {
    state.update(cx, |state, cx| {
        let mut changed = state.video(video).cloned().expect("video");
        changed.properties.insert(priority, PropertyValue::Select(Some(option.into())));
        state.update_video(changed, cx);
    });
}

#[gpui_kit::test]
fn edits_during_another_programs_lock_wait_for_nothing_and_land_after_it(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, &["Desk setup tour", "Studio tour"]);
    let (desk, studio) = (app.video_id(cx, "Desk setup tour"), app.video_id(cx, "Studio tour"));
    let property = priority_property();
    let priority = property.id;
    let state = app.state.clone();
    app.step(cx, move |_, cx| {
        state.update(cx, |state, cx| state.insert_property(property, cx));
        set_priority(&state, desk, priority, "high", cx);
        state.update(cx, |state, cx| state.open_video(desk, VideoTab::Script, cx));
    });
    let first = app.script_blocks(cx)[0];
    app.click(cx, block_part(first, "text"));
    let errors = app.errors(cx);

    let mut locker = Locker::hold(&app.database);

    // Picking a priority shows at once.
    let state = app.state.clone();
    let ((), took) = app.timed(cx, move |_, cx| set_priority(&state, desk, priority, "low", cx));
    assert!(took < PROMPT, "a property change took {took:?}");
    let shown = cx.read(|cx| app.state.read(cx).video(desk).and_then(|video| video.properties.get(&priority).cloned()));
    assert_eq!(shown, Some(PropertyValue::Select(Some("low".into()))));

    // Typing goes to the queue after the save delay, without waiting either.
    let ((), took) = app.timed(cx, |window, cx| window.input("Locked in", cx));
    assert!(took < PROMPT, "typing took {took:?}");
    cx.executor().advance_clock(SAVE_DELAY);
    let page = cx.read(|cx| app.workspace.read(cx).video_page().cloned().expect("video page"));
    let ((), took) = app.timed(cx, move |_, cx| {
        let script = page.read(cx).script().cloned().expect("script");
        script.update(cx, |script, cx| script.flush(cx));
    });
    assert!(took < PROMPT, "handing the script to the queue took {took:?}");

    // Another page opens at once: reading doesn't wait for the lock.
    let state = app.state.clone();
    let ((), took) =
        app.timed(cx, move |_, cx| state.update(cx, |state, cx| state.open_video(studio, VideoTab::Notes, cx)));
    assert!(took < PROMPT, "opening a page took {took:?}");
    assert_eq!(app.page_video(cx), Some(studio));
    let notes = cx.read(|cx| {
        let page = app.workspace.read(cx).video_page().expect("video page").read(cx);
        page.notes().and_then(|notes| notes.read(cx).editor().cloned()).is_some()
    });
    assert!(notes, "the notes are shown");

    // A reload (as a finished pipeline step does) keeps the queued change.
    let state = app.state.clone();
    app.step(cx, move |_, cx| state.update(cx, |state, cx| state.reload(cx)));
    let shown = cx.read(|cx| app.state.read(cx).video(desk).and_then(|video| video.properties.get(&priority).cloned()));
    assert_eq!(shown, Some(PropertyValue::Select(Some("low".into()))), "the queued change still shows");

    // The background writer gave up on this attempt: the page says so, once.
    let state = app.state.clone();
    app.step(cx, move |_, cx| state.update(cx, |state, cx| state.open_video(desk, VideoTab::Script, cx)));
    assert!(cx.read(|cx| app.state.read(cx).is_save_failing(desk)));
    assert!(app.has(cx, "not-saved"), "the page shows its edits aren't saved");
    assert_eq!(app.notifications(cx), 1);
    assert_eq!(
        *errors.borrow(),
        ["Couldn’t save your changes: another program is using the database. VideoCRM keeps trying and saves them \
          when the database is free."]
    );
    assert_eq!(stored_property(&app.database, desk, priority), Some(PropertyValue::Select(Some("high".into()))));
    app.wait(cx, SAVE_RETRY_DELAY);
    assert_eq!(errors.borrow().len(), 1, "retries stay quiet");

    // The lock ends: the next retry writes both, in the background.
    locker.release();
    app.wait(cx, SAVE_RETRY_DELAY);
    assert_eq!(errors.borrow().len(), 1, "nothing else went wrong");
    assert!(!cx.read(|cx| app.state.read(cx).has_unsaved_changes()));
    assert!(!app.has(cx, "not-saved"));
    assert_eq!(stored_property(&app.database, desk, priority), Some(PropertyValue::Select(Some("low".into()))));
    assert_eq!(stored_script(&app.database, desk), "Locked in");
}

#[gpui_kit::test]
fn quitting_waits_for_a_short_lock_and_writes_everything(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, &["Desk setup tour"]);
    let desk = app.video_id(cx, "Desk setup tour");
    let state = app.state.clone();
    app.step(cx, move |_, cx| state.update(cx, |state, cx| state.open_video(desk, VideoTab::Script, cx)));
    let first = app.script_blocks(cx)[0];
    app.click(cx, block_part(first, "text"));

    let locker = Locker::hold(&app.database);
    let state = app.state.clone();
    app.step(cx, move |_, cx| {
        state.update(cx, |state, cx| {
            let mut video = state.video(desk).cloned().expect("video");
            video.hook = "Cheap gear, real sound".into();
            state.update_video(video, cx);
        })
    });
    // Typed right before quitting: still waiting for the save delay.
    app.step(cx, |window, cx| window.input("Last words", cx));

    // The lock ends a second into the quit.
    let release = std::thread::spawn(move || {
        let mut locker = locker;
        std::thread::sleep(Duration::from_secs(1));
        locker.release();
    });
    let started = Instant::now();
    cx.update(|cx| cx.shutdown());
    let took = started.elapsed();
    release.join().expect("released");
    assert!(took < QUIT_SAVE_TIMEOUT, "quitting took {took:?}");

    let hook = stored(&app.database, |store| store.get_video(desk).expect("read video").expect("video").hook);
    assert_eq!(hook, "Cheap gear, real sound");
    assert_eq!(stored_script(&app.database, desk), "Last words");
    assert!(!app.database.with_file_name("unsaved-changes.json").exists());
}

#[gpui_kit::test]
fn changes_that_cant_be_written_before_quitting_are_written_at_the_next_start(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, &["Desk setup tour"]);
    let desk = app.video_id(cx, "Desk setup tour");
    let property = priority_property();
    let priority = property.id;
    let state = app.state.clone();
    app.step(cx, move |_, cx| state.update(cx, |state, cx| state.insert_property(property, cx)));

    let mut locker = Locker::hold(&app.database);
    let state = app.state.clone();
    app.step(cx, move |_, cx| set_priority(&state, desk, priority, "low", cx));

    // The lock outlasts the quit's wait: the change is kept next to the database.
    let started = Instant::now();
    cx.update(|cx| cx.shutdown());
    let took = started.elapsed();
    assert!(took < QUIT_SAVE_TIMEOUT + Duration::from_secs(2), "quitting took {took:?}");
    let unsaved = app.database.with_file_name("unsaved-changes.json");
    assert!(unsaved.exists(), "the change is kept");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&unsaved).expect("unsaved file").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "private like the database");
    }
    locker.release();
    assert_eq!(stored_property(&app.database, desk, priority), None);

    // The next start writes it and says so.
    let mut next = AppState::new(Store::open(&app.database).expect("open again")).expect("load state");
    let problems = next.take_startup_problems();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(problems[0].title(), "Changes from last time restored");
    assert!(problems[0].message().contains("they’re saved now"), "{problems:?}");
    assert!(!unsaved.exists(), "written changes aren't replayed again");
    assert_eq!(stored_property(&app.database, desk, priority), Some(PropertyValue::Select(Some("low".into()))));
    let shown = next.video(desk).and_then(|video| video.properties.get(&priority).cloned());
    assert_eq!(shown, Some(PropertyValue::Select(Some("low".into()))));
}
