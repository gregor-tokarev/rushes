//! UI integration tests for a video's page and the New video dialog: the real
//! `Workspace` over an in-memory store, driven through keys, clicks and typing,
//! with outcomes checked in `AppState` and the store.

use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, ElementId, Entity, Focusable as _, TestAppContext, Window,
    WindowBounds, WindowOptions, px, size,
};
use videocrm::ai_edit::PipelineService;
use videocrm::publish::YoutubeService;
use videocrm::route::{Route, VideoTab};
use videocrm::shell::{self, Workspace};
use videocrm::state::AppState;
use videocrm::video_page::VideoPage;
use videocrm_core::settings::AppSettings;
use videocrm_core::templates::TemplateKind;
use videocrm_core::{BlockId, DocumentKind, NewVideo, Store, VideoId};

struct Fixture {
    window: AnyWindowHandle,
    workspace: Entity<Workspace>,
    state: Entity<AppState>,
    _media: tempfile::TempDir,
}

impl Fixture {
    /// A workspace in a 1440×900 window over a fresh in-memory store holding
    /// `titles` as videos.
    fn open(cx: &mut TestAppContext, titles: &[&str]) -> Self {
        Self::open_sized(cx, titles, 1440., 900.)
    }

    fn open_sized(cx: &mut TestAppContext, titles: &[&str], width: f32, height: f32) -> Self {
        cx.update(|cx| {
            gpui_kit::init(cx);
            shell::init(cx);
            // Overlays slide in on wall-clock time; without motion a click lands
            // on a dialog's button where the last frame drew it.
            cx.set_reduce_motion(true);
        });
        let media = tempfile::tempdir().expect("temp media folder");
        let mut store = Store::open_in_memory().expect("in-memory store");
        let settings = AppSettings {
            media_root: Some(media.path().to_path_buf()),
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
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    Default::default(),
                    size(px(width), px(height)),
                ))),
                ..Default::default()
            };
            let handle = state.clone();
            let (window, workspace) =
                gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| Workspace::new(handle, window, cx)))
                    .expect("open test window");
            (window, workspace, state)
        });
        let fixture = Self { window, workspace, state, _media: media };
        let workspace = fixture.workspace.clone();
        fixture.step(cx, move |window, cx| workspace.read(cx).focus_handle(cx).focus(window, cx));
        fixture
    }

    /// Runs one interaction, then lets GPUI deliver its effects and redraw.
    fn step(&self, cx: &mut TestAppContext, interact: impl FnOnce(&mut Window, &mut App)) {
        cx.update_window(self.window, |_, window, cx| interact(window, cx)).expect("window is open");
        cx.run_until_parked();
        cx.update_window(self.window, |_, window, cx| window.render_frame(cx)).expect("window is open");
    }

    /// Lets timers run, e.g. a dialog's close animation or a save delay.
    fn wait(&self, cx: &mut TestAppContext, duration: Duration) {
        cx.executor().advance_clock(duration);
        self.step(cx, |_, _| {});
    }

    fn press(&self, cx: &mut TestAppContext, key: &str) {
        let key = key.to_string();
        self.step(cx, move |window, cx| window.press(&key, cx));
    }

    fn type_text(&self, cx: &mut TestAppContext, text: &str) {
        let text = text.to_string();
        self.step(cx, move |window, cx| window.input(&text, cx));
    }

    fn click(&self, cx: &mut TestAppContext, id: impl Into<ElementId>) {
        let id = id.into();
        self.step(cx, move |window, cx| window.click(id, cx));
    }

    fn navigate(&self, cx: &mut TestAppContext, route: Route) {
        let state = self.state.clone();
        self.step(cx, move |_, cx| state.update(cx, |state, cx| state.navigate(route, cx)));
    }

    fn open_video(&self, cx: &mut TestAppContext, id: VideoId, tab: VideoTab) {
        self.navigate(cx, Route::Video { id, tab });
    }

    fn route(&self, cx: &mut TestAppContext) -> Route {
        cx.read(|cx| self.state.read(cx).route().clone())
    }

    fn video_id(&self, cx: &mut TestAppContext, title: &str) -> VideoId {
        cx.read(|cx| {
            self.state.read(cx).rows().iter().find(|row| row.video.title == title).expect("video exists").video.id
        })
    }

    fn has(&self, cx: &mut TestAppContext, id: impl Into<ElementId>) -> bool {
        let id = id.into();
        cx.update_window(self.window, |_, window, _| window.try_find(id).is_some()).expect("window is open")
    }

    fn has_dialog(&self, cx: &mut TestAppContext) -> bool {
        cx.update_window(self.window, |_, window, cx| window.has_active_dialog(cx)).expect("window is open")
    }

    fn notifications(&self, cx: &mut TestAppContext) -> usize {
        cx.update_window(self.window, |_, window, cx| window.notifications(cx).len()).expect("window is open")
    }

    fn page(&self, cx: &mut TestAppContext) -> Entity<VideoPage> {
        cx.read(|cx| self.workspace.read(cx).video_page().cloned().expect("video page"))
    }

    /// The stored blocks' text of a video's document.
    fn stored_blocks(&self, cx: &mut TestAppContext, id: VideoId, kind: DocumentKind) -> Vec<String> {
        cx.read(|cx| {
            let store = self.state.read(cx).store();
            let store = store.lock().expect("store lock");
            let documents = store.list_documents(id).expect("list documents");
            let document = documents.into_iter().find(|document| document.kind == kind).expect("document exists");
            document.blocks.iter().map(|block| block.text.clone()).collect()
        })
    }

    fn script_blocks(&self, cx: &mut TestAppContext) -> Vec<BlockId> {
        let page = self.page(cx);
        cx.read(|cx| {
            let editor = page.read(cx).script().expect("script tab").read(cx).editor().cloned().expect("script editor");
            editor.read(cx).document(cx).blocks.iter().map(|block| block.id).collect()
        })
    }

    fn script_focused_block(&self, cx: &mut TestAppContext) -> Option<BlockId> {
        cx.read(|cx| {
            let page = self.workspace.read(cx).video_page()?.read(cx);
            let editor = page.script()?.read(cx).editor()?;
            editor.read(cx).focused_block()
        })
    }
}

/// Element id of one part of an editor block, as the editor assigns it.
fn block_part(id: BlockId, name: &'static str) -> ElementId {
    ElementId::NamedChild(Arc::new(ElementId::Uuid(id.0)), name.into())
}

// New video dialog

#[gpui_kit::test]
fn enter_on_an_empty_title_says_what_is_missing(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, &[]);
    app.press(cx, "ctrl-n");
    assert!(app.has_dialog(cx));

    app.press(cx, "enter");
    assert!(app.has_dialog(cx));
    assert!(app.has(cx, "new-video-error"), "Enter shows the same error as Create");

    // Typing a title clears it.
    app.type_text(cx, "D");
    assert!(!app.has(cx, "new-video-error"));
}

#[gpui_kit::test]
fn a_created_video_keeps_the_caret_in_its_script(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, &["Desk setup tour"]);
    for (title, template) in [("Launch day vlog", "template-blank"), ("Build a home studio", "template-tutorial")] {
        app.press(cx, "ctrl-n");
        app.type_text(cx, title);
        app.click(cx, template);
        app.press(cx, "enter");
        assert!(!app.has_dialog(cx));
        let id = app.video_id(cx, title);
        assert_eq!(app.route(cx), Route::Video { id, tab: VideoTab::Script });

        // The dialog's close animation ends after the page took focus.
        app.wait(cx, Duration::from_secs(1));
        let first = app.script_blocks(cx)[0];
        assert_eq!(app.script_focused_block(cx), Some(first), "{template}: caret in the first block");

        app.type_text(cx, "Typed right away ");
        app.navigate(cx, Route::default());
        let stored = app.stored_blocks(cx, id, DocumentKind::Script);
        assert!(stored[0].starts_with("Typed right away "), "{template}: typing lands in the script: {stored:?}");
    }
}
