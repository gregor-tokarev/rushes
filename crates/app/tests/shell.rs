//! UI integration tests for the application shell: the real `Workspace` over an
//! in-memory store, driven through keys, clicks and typing, with outcomes
//! checked in `AppState` and the store.

use std::sync::Arc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, ElementId, Entity, Focusable as _, TestAppContext, Window,
    WindowBounds, WindowOptions, px, size,
};
use videocrm::ai_edit::PipelineService;
use videocrm::onboarding::{StartChoice, Step};
use videocrm::publish::YoutubeService;
use videocrm::route::{Route, VideoTab};
use videocrm::shell::{self, Page, Workspace};
use videocrm::state::AppState;
use videocrm::video_page::SAVE_DELAY;
use videocrm_core::settings::AppSettings;
use videocrm_core::templates::TemplateKind;
use videocrm_core::{BlockId, DocumentKind, NewVideo, Store, VideoId, VideoStatus};

struct Fixture {
    window: AnyWindowHandle,
    workspace: Entity<Workspace>,
    state: Entity<AppState>,
    _media: tempfile::TempDir,
}

impl Fixture {
    /// A workspace over a fresh in-memory store holding `titles` as videos.
    fn open(cx: &mut TestAppContext, onboarded: bool, titles: &[&str]) -> Self {
        cx.update(|cx| {
            gpui_kit::init(cx);
            shell::init(cx);
        });
        let media = tempfile::tempdir().expect("temp media folder");
        let mut store = Store::open_in_memory().expect("in-memory store");
        let settings = AppSettings {
            media_root: Some(media.path().to_path_buf()),
            onboarding_completed: onboarded,
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

    fn route(&self, cx: &mut TestAppContext) -> Route {
        cx.read(|cx| self.state.read(cx).route().clone())
    }

    fn page(&self, cx: &mut TestAppContext) -> Page {
        cx.read(|cx| self.workspace.read(cx).page(cx))
    }

    fn video_id(&self, cx: &mut TestAppContext, title: &str) -> VideoId {
        cx.read(|cx| {
            self.state.read(cx).rows().iter().find(|row| row.video.title == title).expect("video exists").video.id
        })
    }

    fn has(&self, cx: &mut TestAppContext, id: &'static str) -> bool {
        cx.update_window(self.window, |_, window, _| window.try_find(id).is_some()).expect("window is open")
    }

    fn has_dialog(&self, cx: &mut TestAppContext) -> bool {
        cx.update_window(self.window, |_, window, cx| window.has_active_dialog(cx)).expect("window is open")
    }

    /// The stored text of the first block of a video's document.
    fn stored_first_block(&self, cx: &mut TestAppContext, id: VideoId, kind: DocumentKind) -> String {
        cx.read(|cx| {
            let store = self.state.read(cx).store();
            let store = store.lock().expect("store lock");
            let documents = store.list_documents(id).expect("list documents");
            let document = documents.into_iter().find(|document| document.kind == kind).expect("document exists");
            document.blocks.first().map(|block| block.text.clone()).unwrap_or_default()
        })
    }

    fn script_first_block(&self, cx: &mut TestAppContext) -> BlockId {
        cx.read(|cx| {
            let page = self.workspace.read(cx).video_page().expect("video page").read(cx);
            let editor = page.script().expect("script tab").read(cx).editor().expect("script editor");
            editor.read(cx).document(cx).blocks[0].id
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

#[gpui_kit::test]
fn routes_show_their_pages(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, true, &["Desk setup tour", "Rust in 100 seconds"]);
    assert_eq!(app.page(cx), Page::Database);
    assert!(app.has(cx, "database-page"));

    app.navigate(cx, Route::Settings);
    assert_eq!(app.page(cx), Page::Settings);
    assert!(app.has(cx, "settings-page"));
    assert!(!app.has(cx, "database-page"));

    let desk = app.video_id(cx, "Desk setup tour");
    app.navigate(cx, Route::Video { id: desk, tab: VideoTab::Script });
    assert!(app.has(cx, "video-page"));
    let page = cx.read(|cx| app.workspace.read(cx).video_page().cloned().expect("video page"));
    assert_eq!(cx.read(|cx| page.read(cx).video_id()), desk);

    // Only the tab changes: the same page is reused.
    app.navigate(cx, Route::Video { id: desk, tab: VideoTab::Notes });
    let same = cx.read(|cx| app.workspace.read(cx).video_page().cloned().expect("video page"));
    assert_eq!(same.entity_id(), page.entity_id());
    assert_eq!(cx.read(|cx| same.read(cx).tab()), VideoTab::Notes);

    // Another video gets its own page.
    let rust = app.video_id(cx, "Rust in 100 seconds");
    app.navigate(cx, Route::Video { id: rust, tab: VideoTab::Script });
    let other = cx.read(|cx| app.workspace.read(cx).video_page().cloned().expect("video page"));
    assert_ne!(other.entity_id(), page.entity_id());
    assert_eq!(cx.read(|cx| other.read(cx).video_id()), rust);

    // A video that does not exist shows an empty state instead of a page.
    app.navigate(cx, Route::Video { id: VideoId::new(), tab: VideoTab::Script });
    assert!(app.has(cx, "video-missing"));

    app.click(cx, "back-to-videos");
    assert_eq!(app.page(cx), Page::Database);
}

#[gpui_kit::test]
fn back_and_forward_walk_the_history(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, true, &["Desk setup tour"]);
    let desk = app.video_id(cx, "Desk setup tour");
    app.navigate(cx, Route::Pipelines);
    app.navigate(cx, Route::Video { id: desk, tab: VideoTab::Script });

    app.press(cx, "alt-left");
    assert_eq!(app.route(cx), Route::Pipelines);
    app.press(cx, "alt-left");
    assert_eq!(app.route(cx), Route::default());
    // Nothing further back: the key does nothing.
    app.press(cx, "alt-left");
    assert_eq!(app.route(cx), Route::default());

    app.press(cx, "alt-right");
    assert_eq!(app.route(cx), Route::Pipelines);
    // The title bar's buttons walk the same history.
    app.click(cx, "go-forward");
    assert_eq!(app.route(cx), Route::Video { id: desk, tab: VideoTab::Script });
    app.click(cx, "go-back");
    assert_eq!(app.route(cx), Route::Pipelines);
}

#[gpui_kit::test]
fn the_wizard_creates_a_video_and_opens_its_script(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, true, &[]);
    app.press(cx, "ctrl-n");
    assert!(app.has_dialog(cx));

    // A title is required.
    app.click(cx, "new-video-create");
    assert!(app.has_dialog(cx));
    assert!(cx.read(|cx| app.state.read(cx).rows().is_empty()));

    app.click(cx, "new-video-title");
    app.type_text(cx, "Launch day vlog");
    app.click(cx, "template-vlog");
    app.click(cx, "new-video-create");

    assert!(!app.has_dialog(cx));
    let id = app.video_id(cx, "Launch day vlog");
    assert_eq!(app.route(cx), Route::Video { id, tab: VideoTab::Script });
    let video = cx.read(|cx| app.state.read(cx).video(id).cloned().expect("video"));
    assert_eq!(video.icon.as_deref(), TemplateKind::Vlog.icon());
    assert_eq!(video.status, VideoStatus::Idea);
    // The script opens with the caret in its first block.
    let first = app.script_first_block(cx);
    assert_eq!(app.script_focused_block(cx), Some(first));
}

#[gpui_kit::test]
fn a_board_column_preselects_the_status_and_enter_submits(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, true, &[]);
    let database = cx.read(|cx| app.workspace.read(cx).database().clone());
    app.step(cx, move |_, cx| database.update(cx, |panel, cx| panel.create_video(Some(VideoStatus::Filming), cx)));
    assert!(app.has_dialog(cx));

    app.type_text(cx, "B-roll day");
    app.press(cx, "enter");

    assert!(!app.has_dialog(cx));
    let id = app.video_id(cx, "B-roll day");
    let status = cx.read(|cx| app.state.read(cx).video(id).map(|video| video.status));
    assert_eq!(status, Some(VideoStatus::Filming));

    // Escape cancels without creating anything.
    app.press(cx, "ctrl-n");
    assert!(app.has_dialog(cx));
    app.type_text(cx, "Never mind");
    app.press(cx, "escape");
    assert!(!app.has_dialog(cx));
    assert_eq!(cx.read(|cx| app.state.read(cx).rows().len()), 1);
}

#[gpui_kit::test]
fn completing_onboarding_starts_the_workspace(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, false, &[]);
    assert_eq!(app.page(cx), Page::Onboarding);
    assert!(app.has(cx, "onboarding-card"));
    assert!(!app.has(cx, "sidebar-new-video"));

    // The workspace's commands wait until onboarding is done.
    app.press(cx, "ctrl-k");
    assert!(!app.has_dialog(cx));

    let onboarding = cx.read(|cx| app.workspace.read(cx).onboarding().cloned().expect("onboarding"));
    let step = |cx: &mut TestAppContext| cx.read(|cx| onboarding.read(cx).step());
    app.click(cx, "onboarding-next");
    assert_eq!(step(cx), Step::MediaFolder);
    app.click(cx, "onboarding-next");
    assert_eq!(step(cx), Step::Youtube);
    app.click(cx, "onboarding-skip");
    assert_eq!(step(cx), Step::AiEditing);
    app.click(cx, "onboarding-back");
    assert_eq!(step(cx), Step::Youtube);
    app.click(cx, "onboarding-skip");
    app.click(cx, "onboarding-skip");
    assert_eq!(step(cx), Step::GetStarted);
    app.click(cx, "start-empty");
    app.click(cx, "onboarding-next");

    assert!(cx.read(|cx| app.state.read(cx).settings().onboarding_completed));
    assert_eq!(app.page(cx), Page::Database);
    assert!(cx.read(|cx| app.state.read(cx).rows().is_empty()));
    assert!(app.has(cx, "database-page"));
}

#[gpui_kit::test]
fn onboarding_can_start_with_sample_videos(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, false, &[]);
    for _ in 0..4 {
        app.click(cx, "onboarding-next");
    }
    let onboarding = cx.read(|cx| app.workspace.read(cx).onboarding().cloned().expect("onboarding"));
    assert_eq!(cx.read(|cx| onboarding.read(cx).choice()), StartChoice::SampleVideos);
    app.click(cx, "onboarding-next");

    assert!(cx.read(|cx| app.state.read(cx).settings().onboarding_completed));
    assert!(cx.read(|cx| !app.state.read(cx).rows().is_empty()));
}

#[gpui_kit::test]
fn script_edits_are_saved_after_a_pause(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, true, &["Desk setup tour"]);
    let id = app.video_id(cx, "Desk setup tour");
    app.navigate(cx, Route::Video { id, tab: VideoTab::Script });

    let first = app.script_first_block(cx);
    app.click(cx, block_part(first, "text"));
    app.type_text(cx, "Everything on this desk earned its spot");
    assert_eq!(app.stored_first_block(cx, id, DocumentKind::Script), "");

    // Still typing: nothing is written before the delay has passed.
    cx.executor().advance_clock(SAVE_DELAY / 2);
    cx.run_until_parked();
    assert_eq!(app.stored_first_block(cx, id, DocumentKind::Script), "");

    cx.executor().advance_clock(SAVE_DELAY);
    cx.run_until_parked();
    assert_eq!(app.stored_first_block(cx, id, DocumentKind::Script), "Everything on this desk earned its spot");
    // The database row's derived script stats follow the saved document.
    let words = cx.read(|cx| app.state.read(cx).row(id).map(|row| row.script_words));
    assert_eq!(words, Some(7));

    // Leaving the page writes pending edits without waiting.
    app.type_text(cx, " and stayed");
    app.navigate(cx, Route::default());
    assert_eq!(
        app.stored_first_block(cx, id, DocumentKind::Script),
        "Everything on this desk earned its spot and stayed"
    );
}

#[gpui_kit::test]
fn switching_tabs_saves_the_notes(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, true, &["Desk setup tour"]);
    let id = app.video_id(cx, "Desk setup tour");
    app.navigate(cx, Route::Video { id, tab: VideoTab::Notes });
    let first = cx.read(|cx| {
        let page = app.workspace.read(cx).video_page().expect("video page").read(cx);
        let editor = page.notes().expect("notes tab").read(cx).editor().expect("notes editor");
        editor.read(cx).document(cx).blocks[0].id
    });
    app.click(cx, block_part(first, "text"));
    app.type_text(cx, "Shoot at golden hour");

    app.navigate(cx, Route::Video { id, tab: VideoTab::Script });
    assert_eq!(app.stored_first_block(cx, id, DocumentKind::Notes), "Shoot at golden hour");
}

#[gpui_kit::test]
fn database_events_reach_the_store(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, true, &["Desk setup tour"]);
    let id = app.video_id(cx, "Desk setup tour");
    let database = cx.read(|cx| app.workspace.read(cx).database().clone());

    let panel = database.clone();
    app.step(cx, move |_, cx| panel.update(cx, |panel, cx| panel.set_status(id, VideoStatus::Editing, cx)));
    let stored = cx.read(|cx| {
        let store = app.state.read(cx).store();
        let store = store.lock().expect("store lock");
        store.get_video(id).expect("read video").map(|video| video.status)
    });
    assert_eq!(stored, Some(VideoStatus::Editing));
    assert_eq!(cx.read(|cx| app.state.read(cx).video(id).map(|video| video.status)), Some(VideoStatus::Editing));

    let panel = database.clone();
    app.step(cx, move |_, cx| panel.update(cx, |panel, cx| panel.open_video(id, cx)));
    assert_eq!(app.route(cx), Route::Video { id, tab: VideoTab::Script });
}

#[gpui_kit::test]
fn the_command_palette_jumps_to_a_video(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, true, &["Desk setup tour", "Rust ownership explained", "Tokyo tech stores"]);
    app.press(cx, "ctrl-k");
    assert!(app.has_dialog(cx));

    app.type_text(cx, "rst own");
    app.press(cx, "enter");

    assert!(!app.has_dialog(cx));
    let rust = app.video_id(cx, "Rust ownership explained");
    assert_eq!(app.route(cx), Route::Video { id: rust, tab: VideoTab::Script });
}

#[gpui_kit::test]
fn palette_actions_and_keys_toggle_the_sidebar(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, true, &[]);
    assert!(!cx.read(|cx| app.workspace.read(cx).is_sidebar_collapsed()));

    app.press(cx, "ctrl-b");
    assert!(cx.read(|cx| app.workspace.read(cx).is_sidebar_collapsed()));

    app.press(cx, "ctrl-k");
    app.type_text(cx, "show sidebar");
    app.press(cx, "enter");
    assert!(!app.has_dialog(cx));
    assert!(!cx.read(|cx| app.workspace.read(cx).is_sidebar_collapsed()));
}

#[gpui_kit::test]
fn keys_keep_working_after_the_focused_page_goes_away(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, true, &["Desk setup tour"]);
    let id = app.video_id(cx, "Desk setup tour");
    app.navigate(cx, Route::Video { id, tab: VideoTab::Script });
    app.click(cx, "page-hook");

    app.navigate(cx, Route::Settings);
    app.press(cx, "ctrl-b");
    assert!(cx.read(|cx| app.workspace.read(cx).is_sidebar_collapsed()));
}

#[gpui_kit::test]
fn the_title_and_hook_save_on_enter(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, true, &["Desk setup tour"]);
    let id = app.video_id(cx, "Desk setup tour");
    app.navigate(cx, Route::Video { id, tab: VideoTab::Script });

    app.click(cx, "page-hook");
    app.type_text(cx, "Everything that survived");
    app.press(cx, "enter");
    let hook = cx.read(|cx| app.state.read(cx).video(id).map(|video| video.hook.clone()));
    assert_eq!(hook.as_deref(), Some("Everything that survived"));
}

#[gpui_kit::test]
fn deleting_a_video_with_unsaved_edits_leaves_quietly(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, true, &["Desk setup tour"]);
    let id = app.video_id(cx, "Desk setup tour");
    app.navigate(cx, Route::Video { id, tab: VideoTab::Script });
    let first = app.script_first_block(cx);
    app.click(cx, block_part(first, "text"));
    app.type_text(cx, "Unsaved words");

    let state = app.state.clone();
    app.step(cx, move |_, cx| state.update(cx, |state, cx| state.delete_videos(&[id], cx)));
    assert_eq!(app.route(cx), Route::default());
    assert!(cx.read(|cx| app.state.read(cx).rows().is_empty()));
    let notifications =
        cx.update_window(app.window, |_, window, cx| window.notifications(cx).len()).expect("window is open");
    assert_eq!(notifications, 0);
}

#[gpui_kit::test]
fn errors_become_notifications(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, true, &[]);
    let state = app.state.clone();
    app.step(cx, move |_, cx| state.update(cx, |state, cx| state.report_error("Couldn’t reach YouTube.", cx)));
    let count = cx.update_window(app.window, |_, window, cx| window.notifications(cx).len()).expect("window is open");
    assert_eq!(count, 1);
}

#[gpui_kit::test]
fn shutting_down_writes_pending_edits(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, true, &["Desk setup tour"]);
    let id = app.video_id(cx, "Desk setup tour");
    app.navigate(cx, Route::Video { id, tab: VideoTab::Script });
    let first = app.script_first_block(cx);
    app.click(cx, block_part(first, "text"));
    app.type_text(cx, "Last words");

    let store = cx.read(|cx| app.state.read(cx).store());
    // What the platform runs once Quit has stopped the run loop.
    cx.update(|cx| cx.shutdown());
    let store = store.lock().expect("store lock");
    let documents = store.list_documents(id).expect("list documents");
    let script = documents.iter().find(|document| document.kind == DocumentKind::Script).expect("script");
    assert_eq!(script.blocks[0].text, "Last words");
}

/// Creates a video from `template` through the app state, as the wizard does.
fn create_video(app: &Fixture, cx: &mut TestAppContext, title: &str, template: TemplateKind) -> VideoId {
    let state = app.state.clone();
    let new = NewVideo { title: title.to_string(), template, ..NewVideo::default() };
    cx.update(|cx| state.update(cx, |state, cx| state.create_video(new, cx))).expect("video created")
}

#[gpui_kit::test]
fn the_outline_jumps_to_a_heading(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, true, &[]);
    let id = create_video(&app, cx, "Build a home studio", TemplateKind::Tutorial);
    app.navigate(cx, Route::Video { id, tab: VideoTab::Script });

    let (outline, words) = cx.read(|cx| {
        let page = app.workspace.read(cx).video_page().expect("video page").read(cx);
        let stats = page.script().expect("script tab").read(cx).stats().clone();
        (stats.outline, stats.words)
    });
    assert!(outline.len() > 2, "the tutorial template has sections");
    assert_eq!(cx.read(|cx| app.state.read(cx).row(id).map(|row| row.script_words)), Some(words));

    let heading = outline[2].block_id;
    app.click(cx, ElementId::Name(format!("outline-{heading}").into()));
    assert_eq!(app.script_focused_block(cx), Some(heading));
}

#[gpui_kit::test]
fn importing_markdown_can_replace_the_script(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, true, &["Desk setup tour"]);
    let id = app.video_id(cx, "Desk setup tour");
    app.navigate(cx, Route::Video { id, tab: VideoTab::Script });
    let script = cx.read(|cx| {
        app.workspace.read(cx).video_page().expect("video page").read(cx).script().cloned().expect("script tab")
    });

    let blocks = videocrm_core::document::from_markdown("## Cold open\n\nEverything on this desk earned its spot.\n");
    let tab = script.clone();
    app.step(cx, move |window, cx| {
        tab.update(cx, |tab, cx| tab.confirm_import(std::path::Path::new("desk.md"), blocks, window, cx))
    });
    assert!(app.has_dialog(cx));
    app.click(cx, "import-replace");

    assert!(!app.has_dialog(cx));
    assert_eq!(app.stored_first_block(cx, id, DocumentKind::Script), "Cold open");
    let words = cx.read(|cx| script.read(cx).stats().words);
    assert_eq!(words, 7);
}
