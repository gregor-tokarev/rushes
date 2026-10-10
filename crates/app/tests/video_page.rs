//! UI integration tests for a video's page and the New video dialog: the real
//! `Workspace` over an in-memory store, driven through keys, clicks and typing,
//! with outcomes checked in `AppState` and the store.

use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, ElementId, Entity, Focusable as _, TestAppContext,
    VisualTestContext, Window, WindowBounds, WindowOptions, px, size,
};
use videocrm::ai_edit::PipelineService;
use videocrm::publish::YoutubeService;
use videocrm::route::{Route, VideoTab};
use videocrm::shell::{self, Workspace};
use videocrm::state::AppState;
use videocrm::video_page::{PathPurpose, VideoPage};
use videocrm_core::settings::AppSettings;
use videocrm_core::templates::TemplateKind;
use videocrm_core::{
    BlockId, Column, DocumentKind, NewVideo, PropertyDef, PropertyId, PropertyKind, PropertyValue, SelectOption, Store,
    TagColor, UploadId, UploadJob, UploadStatus, VideoFormat, VideoId, ViewKind,
};

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
        // An inactive window reports no focus changes, so fields would never blur.
        fixture.step(cx, |window, _| window.activate_window());
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
fn a_missing_title_is_explained_under_the_field_in_the_smallest_window(cx: &mut TestAppContext) {
    let app = Fixture::open_sized(cx, &[], 960., 600.);
    app.press(cx, "ctrl-n");
    app.type_text(cx, "   ");
    // The pointer takes the keyboard away from the title.
    app.click(cx, "template-tutorial");
    app.click(cx, "new-video-create");
    assert!(app.has_dialog(cx), "nothing is created");

    cx.update_window(app.window, |_, window, _| {
        let error = window.find("new-video-error");
        assert_eq!(error.label(), Some("Give the video a title."));
        assert!(error.visible());
        let (error, title, hook) =
            (error.bounds(), window.find("new-video-title").bounds(), window.find("new-video-hook").bounds());
        assert!(title.bottom() <= error.top() && error.bottom() <= hook.top(), "under the title: {error:?}");
        assert!(error.bottom() <= px(600.), "in view without scrolling: {error:?}");
        assert_eq!(window.find("new-video-title").focused(), Some(true), "the title takes the keyboard");
    })
    .expect("window is open");
    app.type_text(cx, "Desk tour");
    assert!(!app.has(cx, "new-video-error"));
}

#[gpui_kit::test]
fn the_format_is_chosen_with_the_keyboard(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, &[]);
    let format = |app: &Fixture, cx: &mut TestAppContext| {
        cx.update_window(app.window, |_, window, _| {
            let control = window.find("new-video-format");
            (control.focused(), control.label().map(str::to_string))
        })
        .expect("window is open")
    };
    app.press(cx, "ctrl-n");
    app.type_text(cx, "Quick tip");
    // Title, Hook, then Format.
    app.press(cx, "tab");
    app.press(cx, "tab");
    assert_eq!(format(&app, cx), (Some(true), Some("Format: Long-form".into())));

    app.press(cx, "right");
    app.press(cx, "right");
    assert_eq!(format(&app, cx).1.as_deref(), Some("Format: Livestream"));
    app.press(cx, "right");
    assert_eq!(format(&app, cx).1.as_deref(), Some("Format: Livestream"), "the last format stays");
    app.press(cx, "left");
    assert_eq!(format(&app, cx).1.as_deref(), Some("Format: Short"));

    // Tab goes on to Status; Enter creates the video.
    app.press(cx, "tab");
    assert_eq!(format(&app, cx).0, Some(false));
    app.press(cx, "shift-tab");
    app.press(cx, "enter");
    assert!(!app.has_dialog(cx));
    let id = app.video_id(cx, "Quick tip");
    assert_eq!(cx.read(|cx| app.state.read(cx).video(id).map(|video| video.format)), Some(VideoFormat::Short));
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

/// Adds a custom property through the app state, as the database does.
fn add_property(app: &Fixture, cx: &mut TestAppContext, name: &str, kind: PropertyKind) -> PropertyId {
    let property = PropertyDef { id: PropertyId::new(), name: name.into(), kind, position: 0 };
    let (state, id) = (app.state.clone(), property.id);
    app.step(cx, move |_, cx| state.update(cx, |state, cx| state.insert_property(property, cx)));
    id
}

fn stored_property(app: &Fixture, cx: &mut TestAppContext, video: VideoId, id: PropertyId) -> Option<PropertyValue> {
    cx.read(|cx| {
        let store = app.state.read(cx).store();
        let store = store.lock().expect("store lock");
        store.get_video(video).expect("read video").and_then(|video| video.properties.get(&id).cloned())
    })
}

fn property_input(id: PropertyId) -> ElementId {
    ElementId::Name(format!("property-input-{id}").into())
}

/// The text a field shows, as its accessibility value.
fn field_text(app: &Fixture, cx: &mut TestAppContext, id: impl Into<ElementId>) -> String {
    let id = id.into();
    cx.update_window(app.window, |_, window, _| window.find(id).value().unwrap_or_default().to_string())
        .expect("window is open")
}

// Properties

#[gpui_kit::test]
fn a_typed_property_value_is_saved_when_the_app_quits(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, &["Desk setup tour"]);
    let id = app.video_id(cx, "Desk setup tour");
    let sponsor = add_property(&app, cx, "Sponsor", PropertyKind::Text);
    app.open_video(cx, id, VideoTab::Script);

    app.click(cx, property_input(sponsor));
    app.type_text(cx, "Beta Sponsor");
    // No Enter, no blur: the app quits with the field still focused.
    cx.update(|cx| cx.shutdown());
    assert_eq!(stored_property(&app, cx, id, sponsor), Some(PropertyValue::Text("Beta Sponsor".into())));
}

#[gpui_kit::test]
fn a_typed_property_value_is_saved_when_leaving_by_keyboard(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, &["Desk setup tour"]);
    let id = app.video_id(cx, "Desk setup tour");
    let sponsor = add_property(&app, cx, "Sponsor", PropertyKind::Text);
    app.open_video(cx, id, VideoTab::Script);

    app.click(cx, property_input(sponsor));
    app.type_text(cx, "Acme Corp");
    app.press(cx, "alt-left");
    assert_eq!(app.route(cx), Route::default());
    assert_eq!(stored_property(&app, cx, id, sponsor), Some(PropertyValue::Text("Acme Corp".into())));
}

#[gpui_kit::test]
fn numbers_read_like_the_database_and_edit_raw(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, &["Desk setup tour"]);
    let id = app.video_id(cx, "Desk setup tour");
    let views = add_property(&app, cx, "Views", PropertyKind::Number);
    let precise = add_property(&app, cx, "Ratio", PropertyKind::Number);
    let state = app.state.clone();
    app.step(cx, move |_, cx| {
        state.update(cx, |state, cx| {
            let mut video = state.video(id).cloned().expect("video");
            video.properties.insert(precise, PropertyValue::Number(1.234));
            state.update_video(video, cx);
        })
    });
    app.open_video(cx, id, VideoTab::Script);

    app.click(cx, property_input(views));
    app.type_text(cx, "12000");
    app.press(cx, "enter");
    assert_eq!(stored_property(&app, cx, id, views), Some(PropertyValue::Number(12000.)));
    app.click(cx, "page-hook");
    assert_eq!(field_text(&app, cx, property_input(views)), "12,000");

    // Focusing shows the number as stored; leaving it untouched keeps its precision.
    assert_eq!(field_text(&app, cx, property_input(precise)), "1.23");
    app.click(cx, property_input(precise));
    assert_eq!(field_text(&app, cx, property_input(precise)), "1.234");
    app.click(cx, "page-hook");
    assert_eq!(stored_property(&app, cx, id, precise), Some(PropertyValue::Number(1.234)));
    assert_eq!(field_text(&app, cx, property_input(precise)), "1.23");
}

/// Sets a number property of a video through the app state.
fn set_number(app: &Fixture, cx: &mut TestAppContext, video: VideoId, id: PropertyId, number: f64) {
    let state = app.state.clone();
    app.step(cx, move |_, cx| {
        state.update(cx, |state, cx| {
            let mut stored = state.video(video).cloned().expect("video");
            stored.properties.insert(id, PropertyValue::Number(number));
            state.update_video(stored, cx);
        })
    });
}

#[gpui_kit::test]
fn a_decimal_comma_is_the_decimal_point(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, &["Desk setup tour"]);
    let id = app.video_id(cx, "Desk setup tour");
    let budget = add_property(&app, cx, "Budget", PropertyKind::Number);
    app.open_video(cx, id, VideoTab::Script);

    for (typed, number) in [("3,5", 3.5), ("1,25", 1.25), ("1,500", 1500.)] {
        app.click(cx, property_input(budget));
        app.press(cx, "ctrl-a");
        app.type_text(cx, typed);
        app.press(cx, "enter");
        assert_eq!(stored_property(&app, cx, id, budget), Some(PropertyValue::Number(number)), "{typed}");
    }
    assert_eq!(app.notifications(cx), 0);
}

#[gpui_kit::test]
fn a_refused_number_shows_the_stored_one_and_is_reported_once(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, &["Desk setup tour"]);
    let id = app.video_id(cx, "Desk setup tour");
    let views = add_property(&app, cx, "Views", PropertyKind::Number);
    set_number(&app, cx, id, views, 1500.);
    app.open_video(cx, id, VideoTab::Script);

    // Refused on Enter: the field shows the stored number again.
    app.click(cx, property_input(views));
    app.press(cx, "ctrl-a");
    app.type_text(cx, "abc");
    app.press(cx, "enter");
    assert_eq!(app.notifications(cx), 1);
    assert_eq!(field_text(&app, cx, property_input(views)), "1500");
    // Saving the page's edits on tab switches and navigation reports nothing new.
    app.open_video(cx, id, VideoTab::Notes);
    app.open_video(cx, id, VideoTab::Script);
    assert_eq!(app.notifications(cx), 1);

    // Refused when a tab switch saves the field: reported once, too.
    app.click(cx, property_input(views));
    app.press(cx, "ctrl-a");
    app.type_text(cx, "1,2,3");
    app.open_video(cx, id, VideoTab::Notes);
    assert_eq!(field_text(&app, cx, property_input(views)), "1500");
    app.open_video(cx, id, VideoTab::Script);
    app.navigate(cx, Route::default());
    assert_eq!(app.notifications(cx), 2);
    assert_eq!(stored_property(&app, cx, id, views), Some(PropertyValue::Number(1500.)));
}

#[gpui_kit::test]
fn link_properties_open_web_addresses_and_refuse_other_schemes(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, &["Desk setup tour"]);
    let id = app.video_id(cx, "Desk setup tour");
    let link = add_property(&app, cx, "Ref link", PropertyKind::Url);
    app.open_video(cx, id, VideoTab::Script);
    let open = ElementId::Name(format!("open-url-{link}").into());

    app.click(cx, property_input(link));
    app.type_text(cx, "youtube.com/@mychannel");
    app.press(cx, "enter");
    app.click(cx, open.clone());
    assert_eq!(cx.opened_url().as_deref(), Some("https://youtube.com/@mychannel"));

    app.click(cx, property_input(link));
    app.press(cx, "ctrl-a");
    app.type_text(cx, "file:///home/creator/.bashrc");
    app.press(cx, "enter");
    let before = app.notifications(cx);
    app.click(cx, open);
    assert_eq!(cx.opened_url().as_deref(), Some("https://youtube.com/@mychannel"), "nothing else was opened");
    assert_eq!(app.notifications(cx), before + 1, "the refusal is explained");
}

#[gpui_kit::test]
fn a_property_added_on_the_page_shows_in_the_tables(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, &["Desk setup tour"]);
    let id = app.video_id(cx, "Desk setup tour");
    app.open_video(cx, id, VideoTab::Script);

    app.click(cx, "add-property");
    assert!(app.has_dialog(cx));
    // The database's dialog: the same form adds a property from either place.
    app.type_text(cx, "Sponsor");
    app.click(cx, "property-commit");
    assert!(!app.has_dialog(cx));

    let (property, tables) = cx.read(|cx| {
        let state = app.state.read(cx);
        let property = state.properties().iter().find(|property| property.name == "Sponsor").expect("created").id;
        let tables: Vec<_> = state.views().iter().filter(|view| view.kind == ViewKind::Table).cloned().collect();
        (property, tables)
    });
    assert!(!tables.is_empty());
    for view in tables {
        let layout = view.columns.iter().find(|layout| layout.column == Column::Custom(property));
        assert!(layout.is_some_and(|layout| layout.visible), "shown in “{}”", view.name);
    }
}

#[gpui_kit::test]
fn a_multi_select_on_the_page_stays_open_while_options_are_ticked(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, &["Desk setup tour"]);
    let id = app.video_id(cx, "Desk setup tour");
    let option = |id: &str, name: &str| SelectOption { id: id.into(), name: name.into(), color: TagColor::Blue };
    let platforms = add_property(
        &app,
        cx,
        "Platforms",
        PropertyKind::MultiSelect { options: vec![option("yt", "YouTube"), option("tt", "TikTok")] },
    );
    app.open_video(cx, id, VideoTab::Script);

    // As in the table's cell: one opening, several options.
    app.click(cx, ElementId::Name(format!("property-multi-{platforms}").into()));
    app.click(cx, "option-yt");
    app.click(cx, "option-tt");
    assert_eq!(
        stored_property(&app, cx, id, platforms),
        Some(PropertyValue::MultiSelect(vec!["yt".into(), "tt".into()]))
    );
    // The open list shows both ticks.
    let checked = |app: &Fixture, cx: &mut TestAppContext, option: &'static str| {
        cx.update_window(app.window, |_, window, _| window.find(option).checked()).expect("window is open")
    };
    assert_eq!(checked(&app, cx, "option-yt"), Some(true));
    assert_eq!(checked(&app, cx, "option-tt"), Some(true));
}

#[gpui_kit::test]
fn backspace_in_the_empty_tag_field_removes_the_last_tag(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, &["Desk setup tour"]);
    let id = app.video_id(cx, "Desk setup tour");
    app.open_video(cx, id, VideoTab::Script);
    let tags = |app: &Fixture, cx: &mut TestAppContext| {
        cx.read(|cx| app.state.read(cx).video(id).expect("video").tags.clone())
    };

    app.click(cx, "property-tags-input");
    app.type_text(cx, "rust");
    app.press(cx, "enter");
    app.type_text(cx, "tutorial");
    app.press(cx, "enter");
    assert_eq!(tags(&app, cx), ["rust", "tutorial"]);

    // Typed text is edited first; the empty field removes tags.
    app.type_text(cx, "x");
    app.press(cx, "backspace");
    assert_eq!(tags(&app, cx), ["rust", "tutorial"]);
    app.press(cx, "backspace");
    assert_eq!(tags(&app, cx), ["rust"]);

    // A tag typed but not entered is kept when the page saves its edits,
    // as on a tab switch, and when the app quits.
    app.type_text(cx, "desk");
    app.open_video(cx, id, VideoTab::Notes);
    assert_eq!(tags(&app, cx), ["rust", "desk"]);
    assert_eq!(field_text(&app, cx, "property-tags-input"), "", "the added tag leaves the field");
    app.click(cx, "property-tags-input");
    app.type_text(cx, "studio");
    cx.update(|cx| cx.shutdown());
    let stored = cx.read(|cx| {
        let store = app.state.read(cx).store();
        let store = store.lock().expect("store lock");
        store.get_video(id).expect("read video").expect("video").tags
    });
    assert_eq!(stored, ["rust", "desk", "studio"]);
}

#[gpui_kit::test]
fn a_tag_remove_button_sits_after_the_tag_name(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, &["Desk setup tour"]);
    let id = app.video_id(cx, "Desk setup tour");
    app.open_video(cx, id, VideoTab::Script);
    app.click(cx, "property-tags-input");
    for tag in ["ai", "programming"] {
        app.type_text(cx, tag);
        app.press(cx, "enter");
    }

    for tag in ["ai", "programming"] {
        let chip = ElementId::Name(format!("tag-{tag}").into());
        let at_rest = bounds(&app, cx, chip.clone());
        let hover = chip.clone();
        app.step(cx, move |window, cx| window.hover(hover, cx));
        let hovered = bounds(&app, cx, chip);
        let remove = bounds(&app, cx, ElementId::Name(format!("remove-tag-{tag}").into()));
        assert_eq!(hovered, at_rest, "{tag}: the chip keeps room for the button, so nothing moves");
        assert!(remove.right() <= hovered.right() && remove.left() >= hovered.left(), "{tag}: inside the chip");
        // The name and the chip's leading padding come before the button.
        assert!(
            remove.left() - hovered.left() >= remove.size.width,
            "{tag}: {remove:?} covers the name in {hovered:?}"
        );
    }

    app.step(cx, |window, cx| window.hover("tag-ai", cx));
    app.click(cx, "remove-tag-ai");
    assert_eq!(cx.read(|cx| app.state.read(cx).video(id).expect("video").tags.clone()), ["programming"]);
}

#[gpui_kit::test]
fn the_publish_date_opens_a_calendar_and_clears_from_it(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, &["Desk setup tour"]);
    let id = app.video_id(cx, "Desk setup tour");
    let date = chrono::NaiveDate::from_ymd_opt(2026, 10, 14).expect("date");
    let state = app.state.clone();
    app.step(cx, move |_, cx| {
        state.update(cx, |state, cx| {
            let mut video = state.video(id).cloned().expect("video");
            video.publish_date = Some(date);
            state.update_video(video, cx);
        })
    });
    app.open_video(cx, id, VideoTab::Script);
    assert_eq!(
        cx.update_window(app.window, |_, window, _| window.find("property-date-publish").label().map(str::to_string))
            .expect("window is open")
            .as_deref(),
        Some("Oct 14, 2026")
    );

    app.click(cx, "property-date-publish");
    app.click(cx, "clear-date");
    assert_eq!(cx.read(|cx| app.state.read(cx).video(id).and_then(|video| video.publish_date)), None);
    assert!(!app.has(cx, "clear-date"), "the calendar closes");
}

#[gpui_kit::test]
fn a_video_gone_from_youtube_says_so(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, &["Desk setup tour"]);
    let id = app.video_id(cx, "Desk setup tour");
    app.open_video(cx, id, VideoTab::Script);
    let shown = |app: &Fixture, cx: &mut TestAppContext| {
        cx.update_window(app.window, |_, window, _| window.find("property-youtube-value").label().map(str::to_string))
            .expect("window is open")
    };
    assert_eq!(shown(&app, cx).as_deref(), Some("Not uploaded"));

    // The upload reached YouTube; the link was dropped once YouTube no longer had it.
    let now = chrono::Utc::now();
    let job = UploadJob {
        id: UploadId::new(),
        video_id: id,
        file: app._media.path().join("final.mp4"),
        file_stamp: None,
        status: UploadStatus::Done { video_id: "yt-gone".into() },
        session_url: None,
        created_at: now,
        updated_at: now,
    };
    let state = app.state.clone();
    app.step(cx, move |_, cx| {
        state.read(cx).store().lock().expect("store lock").insert_upload(&job).expect("insert upload");
        state.update(cx, |state, cx| state.reload(cx));
    });
    assert_eq!(shown(&app, cx).as_deref(), Some("Not on YouTube anymore"), "as the Publish tab says it");
}

// Title

#[gpui_kit::test]
fn an_emptied_title_keeps_the_stored_one(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, &["Desk setup tour"]);
    let id = app.video_id(cx, "Desk setup tour");
    app.open_video(cx, id, VideoTab::Script);

    app.click(cx, "page-title");
    app.press(cx, "ctrl-a");
    app.press(cx, "backspace");
    app.click(cx, "page-hook");
    assert_eq!(
        cx.read(|cx| app.state.read(cx).video(id).map(|video| video.title.clone())).as_deref(),
        Some("Desk setup tour")
    );
    let page = app.page(cx);
    assert_eq!(cx.read(|cx| page.read(cx).title_field().read(cx).value().to_string()), "Desk setup tour");
}

#[gpui_kit::test]
fn a_value_picked_from_a_menu_keeps_what_was_typed(cx: &mut TestAppContext) {
    // Without an active window (no window manager, another app in front)
    // fields are never told they lost focus, so nothing is saved on blur.
    for active in [true, false] {
        let app = Fixture::open(cx, &["Desk setup tour"]);
        if !active {
            VisualTestContext::from_window(app.window, cx).deactivate_window();
            app.step(cx, |_, _| {});
        }
        let id = app.video_id(cx, "Desk setup tour");
        let option = |id: &str, name: &str| SelectOption { id: id.into(), name: name.into(), color: TagColor::Blue };
        let options = vec![option("low", "Low"), option("high", "High")];
        let priority = add_property(&app, cx, "Priority", PropertyKind::Select { options });
        let sponsor = add_property(&app, cx, "Sponsor", PropertyKind::Text);
        app.open_video(cx, id, VideoTab::Script);
        let video = |app: &Fixture, cx: &mut TestAppContext| {
            cx.read(|cx| app.state.read(cx).video(id).cloned().expect("video"))
        };
        let pick = |app: &Fixture, cx: &mut TestAppContext, item: usize| {
            app.click(cx, ElementId::Name(format!("property-select-{priority}").into()));
            app.step(cx, move |window, cx| window.within("popup-menu").click(item, cx));
        };

        // Typed into a field, then a menu pick before the field is left.
        app.click(cx, "page-hook");
        app.type_text(cx, "Cheap gear, real sound");
        pick(&app, cx, 0);
        assert_eq!(field_text(&app, cx, "page-hook"), "Cheap gear, real sound", "active: {active}");
        assert_eq!(video(&app, cx).hook, "Cheap gear, real sound", "active: {active}");
        assert_eq!(video(&app, cx).properties.get(&priority), Some(&PropertyValue::Select(Some("low".into()))));

        app.click(cx, "page-title");
        app.press(cx, "ctrl-a");
        app.type_text(cx, "Desk setup tour 2026");
        pick(&app, cx, 1);
        let page = app.page(cx);
        let title = cx.read(|cx| page.read(cx).title_field().read(cx).value().to_string());
        assert_eq!(title, "Desk setup tour 2026", "active: {active}");
        assert_eq!(video(&app, cx).title, "Desk setup tour 2026", "active: {active}");
        assert_eq!(video(&app, cx).properties.get(&priority), Some(&PropertyValue::Select(Some("high".into()))));

        app.click(cx, property_input(sponsor));
        app.type_text(cx, "Acme Corp");
        app.click(cx, "property-status-value");
        app.step(cx, |window, cx| window.within("popup-menu").click(2usize, cx));
        assert_eq!(field_text(&app, cx, property_input(sponsor)), "Acme Corp", "active: {active}");
        let stored = video(&app, cx).properties.get(&sponsor).cloned();
        assert_eq!(stored, Some(PropertyValue::Text("Acme Corp".into())), "active: {active}");

        // A field being typed into keeps its text when the video changes
        // elsewhere, e.g. in the table; the text is saved when it is left.
        app.click(cx, "page-hook");
        app.press(cx, "ctrl-a");
        app.type_text(cx, "Typed while it changed");
        let state = app.state.clone();
        app.step(cx, move |_, cx| {
            state.update(cx, |state, cx| {
                let mut video = state.video(id).cloned().expect("video");
                video.tags.push("studio".into());
                state.update_video(video, cx);
            })
        });
        assert_eq!(field_text(&app, cx, "page-hook"), "Typed while it changed", "active: {active}");
        app.navigate(cx, Route::default());
        assert_eq!(video(&app, cx).hook, "Typed while it changed", "active: {active}");
        assert_eq!(video(&app, cx).tags, vec!["studio".to_string()]);
    }
}

// Layout and scrolling

/// Creates a video from `template` through the app state, as the wizard does.
fn create_video(app: &Fixture, cx: &mut TestAppContext, title: &str, template: TemplateKind) -> VideoId {
    let state = app.state.clone();
    let new = NewVideo { title: title.to_string(), template, ..NewVideo::default() };
    cx.update(|cx| state.update(cx, |state, cx| state.create_video(new, cx))).expect("video created")
}

/// Replaces a video's script with `markdown`, saved.
fn set_script(app: &Fixture, cx: &mut TestAppContext, id: VideoId, markdown: &str) {
    let blocks = videocrm_core::document::from_markdown(markdown);
    let state = app.state.clone();
    cx.update(|cx| {
        let store = state.read(cx).store();
        let mut store = store.lock().expect("store lock");
        let mut script = store
            .list_documents(id)
            .expect("list documents")
            .into_iter()
            .find(|document| document.kind == DocumentKind::Script)
            .expect("script");
        script.blocks = blocks;
        store.save_document(&mut script).expect("save script");
    });
}

fn long_script(sections: usize) -> String {
    (1..=sections)
        .map(|n| {
            format!("## Section {n}\n\nEverything on this desk earned its spot, one paragraph per section {n}.\n\n")
        })
        .collect()
}

fn bounds(app: &Fixture, cx: &mut TestAppContext, id: impl Into<ElementId>) -> Bounds<gpui_kit::Pixels> {
    let id = id.into();
    cx.update_window(app.window, |_, window, _| window.find(id).bounds()).expect("window is open")
}

fn scroll_top(app: &Fixture, cx: &mut TestAppContext) -> gpui_kit::Pixels {
    let page = app.page(cx);
    cx.read(|cx| page.read(cx).scroll_top())
}

fn set_scroll_top(app: &Fixture, cx: &mut TestAppContext, distance: f32) {
    let page = app.page(cx);
    app.step(cx, move |_, cx| page.update(cx, |page, cx| page.set_scroll_top(px(distance), cx)));
}

#[gpui_kit::test]
fn switching_tabs_keeps_the_header_and_tabs_in_place(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, &["Desk setup tour"]);
    let id = app.video_id(cx, "Desk setup tour");
    app.open_video(cx, id, VideoTab::Script);
    assert!(app.has(cx, "script-rail"), "a wide window shows the rail");
    let on_script = (bounds(&app, cx, "page-menu"), bounds(&app, cx, "page-hook"));

    for tab in [VideoTab::Notes, VideoTab::Footage, VideoTab::Publish, VideoTab::Script] {
        app.open_video(cx, id, tab);
        let here = (bounds(&app, cx, "page-menu"), bounds(&app, cx, "page-hook"));
        assert_eq!(here.0.left(), on_script.0.left(), "{tab:?}: the tab strip stays put");
        assert_eq!(here.1.left(), on_script.1.left(), "{tab:?}: the header stays put");
    }
}

#[gpui_kit::test]
fn tabs_other_than_the_script_use_the_rails_width(cx: &mut TestAppContext) {
    // Wide enough for the script rail, too narrow for the column beside it.
    let app = Fixture::open_sized(cx, &["Desk setup tour"], 1160., 800.);
    let id = app.video_id(cx, "Desk setup tour");
    app.open_video(cx, id, VideoTab::Script);
    assert!(app.has(cx, "script-rail"));
    let rail = bounds(&app, cx, "script-rail");
    let script = (bounds(&app, cx, "page-hook"), bounds(&app, cx, "page-menu"));
    assert!(script.1.right() <= rail.left(), "the script's column ends before the rail");

    app.open_video(cx, id, VideoTab::Publish);
    // A frame for the panel to measure itself.
    app.step(cx, |_, _| {});
    let publish = (bounds(&app, cx, "page-hook"), bounds(&app, cx, "page-menu"));
    assert_eq!(publish.0.left(), script.0.left(), "the header starts where it did");
    assert!(publish.1.right() > script.1.right(), "the column takes the rail's room");
    // 39rem: the form and the side column next to each other.
    let panel = bounds(&app, cx, "publish-panel");
    assert!(panel.size.width >= px(624.), "Publish has room for two columns: {panel:?}");
}

#[gpui_kit::test]
fn each_tab_keeps_its_scroll_position(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, &["Desk setup tour"]);
    let id = app.video_id(cx, "Desk setup tour");
    set_script(&app, cx, id, &long_script(30));
    app.open_video(cx, id, VideoTab::Script);
    set_scroll_top(&app, cx, 1500.);
    assert_eq!(scroll_top(&app, cx), px(1500.));

    app.open_video(cx, id, VideoTab::Notes);
    assert!(scroll_top(&app, cx) < px(1500.), "the short notes tab starts higher");
    app.open_video(cx, id, VideoTab::Script);
    assert_eq!(scroll_top(&app, cx), px(1500.), "the script comes back where it was left");

    // History walks the same way.
    app.press(cx, "alt-left");
    app.press(cx, "alt-right");
    assert_eq!(app.route(cx), Route::Video { id, tab: VideoTab::Script });
    assert_eq!(scroll_top(&app, cx), px(1500.));
}

#[gpui_kit::test]
fn a_tab_opened_at_its_top_shows_its_first_section(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, &["Desk setup tour"]);
    let id = app.video_id(cx, "Desk setup tour");
    set_script(&app, cx, id, &long_script(30));
    app.open_video(cx, id, VideoTab::Script);
    set_scroll_top(&app, cx, 1500.);

    // "Go to Publish" from deep in another tab.
    app.step(cx, move |_, cx| videocrm::video_page::open_tab_at_top(id, VideoTab::Publish, cx));
    assert_eq!(app.route(cx), Route::Video { id, tab: VideoTab::Publish });
    let page = app.page(cx);
    let strip = cx.read(|cx| page.read(cx).tab_strip_top());
    assert!(strip > px(0.));
    assert_eq!(scroll_top(&app, cx), strip, "the tab strip is at the top of the page");
}

#[gpui_kit::test]
fn a_narrow_window_keeps_the_script_tools_in_a_menu(cx: &mut TestAppContext) {
    let app = Fixture::open_sized(cx, &[], 960., 600.);
    let id = create_video(&app, cx, "Build a home studio", TemplateKind::Tutorial);
    app.open_video(cx, id, VideoTab::Script);
    assert!(!app.has(cx, "script-rail"));
    assert!(app.has(cx, "script-menu"), "stats and actions are next to the tabs");

    app.click(cx, "script-menu");
    let page = app.page(cx);
    let script = cx.read(|cx| page.read(cx).script().cloned().expect("script"));
    let summary = cx.read(|cx| script.read(cx).stats().summary());
    let label = cx
        .update_window(app.window, |_, window, _| window.find("script-menu").label().map(str::to_string))
        .expect("window is open");
    assert_eq!(label.as_deref(), Some(summary.as_str()));
    app.press(cx, "escape");

    // Other tabs show no script menu.
    app.open_video(cx, id, VideoTab::Notes);
    assert!(!app.has(cx, "script-menu"));
}

#[gpui_kit::test]
fn the_outline_puts_a_later_heading_at_the_top(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, &["Desk setup tour"]);
    let id = app.video_id(cx, "Desk setup tour");
    set_script(&app, cx, id, &long_script(30));
    app.open_video(cx, id, VideoTab::Script);

    let page = app.page(cx);
    let outline = cx.read(|cx| page.read(cx).script().expect("script").read(cx).stats().outline.clone());
    let heading = outline[12].block_id;
    app.click(cx, ElementId::Name(format!("outline-{heading}").into()));
    assert_eq!(app.script_focused_block(cx), Some(heading));
    // A frame to lay out the reveal.
    app.step(cx, |_, _| {});
    let row = bounds(&app, cx, block_part(heading, "row"));
    let viewport = bounds(&app, cx, "video-page-scroll");
    assert!(row.top() >= viewport.top(), "the heading is visible");
    assert!(
        row.top() < viewport.top() + px(80.),
        "the heading is at the top, not the bottom edge: {row:?} in {viewport:?}"
    );
}

#[gpui_kit::test]
fn a_click_below_the_document_puts_the_caret_after_it(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, &["Desk setup tour"]);
    let id = app.video_id(cx, "Desk setup tour");
    set_script(&app, cx, id, "## Cold open\n\nEverything on this desk earned its spot.\n");
    app.open_video(cx, id, VideoTab::Script);

    let end = bounds(&app, cx, "page-end");
    assert!(end.size.height > px(120.), "the space below a short script reacts: {end:?}");
    // Far below the last block, near the bottom of the window.
    app.step(cx, |window, cx| window.click_at("page-end", gpui_kit::point(px(200.), end.size.height - px(20.)), cx));
    app.type_text(cx, "DEADZONE");
    app.navigate(cx, Route::default());
    assert_eq!(app.stored_blocks(cx, id, DocumentKind::Script).last().map(String::as_str), Some("DEADZONE"));
}

// Keyboard path into the document

#[gpui_kit::test]
fn enter_and_down_in_the_hook_continue_in_the_script(cx: &mut TestAppContext) {
    for key in ["enter", "down"] {
        let app = Fixture::open(cx, &[]);
        let id = create_video(&app, cx, "Build a home studio", TemplateKind::Tutorial);
        app.open_video(cx, id, VideoTab::Script);
        app.click(cx, "page-hook");
        app.type_text(cx, "Cheap gear, real sound");
        app.press(cx, key);
        let first = app.script_blocks(cx)[0];
        assert_eq!(app.script_focused_block(cx), Some(first), "{key} moves into the script");
        assert_eq!(
            cx.read(|cx| app.state.read(cx).video(id).map(|video| video.hook.clone())).as_deref(),
            Some("Cheap gear, real sound")
        );
    }
}

#[gpui_kit::test]
fn enter_on_a_table_row_opens_the_script_to_write(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, &[]);
    let id = create_video(&app, cx, "Build a home studio", TemplateKind::Tutorial);
    let row = ElementId::Name(format!("video-row-{id}").into());
    for typed in ["K5 ", "K6 "] {
        app.navigate(cx, Route::default());
        // Selects the row: beside the title, before its Open button.
        let select = row.clone();
        app.step(cx, move |window, cx| window.click_at(select, gpui_kit::point(px(240.), px(16.)), cx));
        app.press(cx, "enter");
        assert_eq!(app.route(cx), Route::Video { id, tab: VideoTab::Script });
        let first = app.script_blocks(cx)[0];
        assert_eq!(app.script_focused_block(cx), Some(first), "the caret is in the script");
        app.type_text(cx, typed);
    }
    app.navigate(cx, Route::default());
    let stored = app.stored_blocks(cx, id, DocumentKind::Script);
    assert!(stored[0].starts_with("K6 K5 "), "both openings typed into the script: {stored:?}");
}

#[gpui_kit::test]
fn tab_from_the_page_controls_reaches_the_script(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, &["Desk setup tour"]);
    let id = app.video_id(cx, "Desk setup tour");
    app.open_video(cx, id, VideoTab::Script);
    app.click(cx, "page-hook");
    for _ in 0..40 {
        if app.script_focused_block(cx).is_some() {
            break;
        }
        app.press(cx, "tab");
    }
    let first = app.script_blocks(cx)[0];
    assert_eq!(app.script_focused_block(cx), Some(first));
    app.type_text(cx, "K3");
    app.navigate(cx, Route::default());
    assert_eq!(app.stored_blocks(cx, id, DocumentKind::Script)[0], "K3");
}

// Saving

#[gpui_kit::test]
fn a_failed_script_save_is_retried_and_written_on_quit(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, &["Desk setup tour"]);
    let id = app.video_id(cx, "Desk setup tour");
    app.open_video(cx, id, VideoTab::Script);
    let first = app.script_blocks(cx)[0];
    app.click(cx, block_part(first, "text"));

    // The database refuses the write: the video's row is gone from the store
    // while the page still shows it.
    let video = cx.read(|cx| app.state.read(cx).video(id).cloned().expect("video"));
    let store = cx.read(|cx| app.state.read(cx).store());
    store.lock().expect("store lock").delete_video(id).expect("delete video");

    app.type_text(cx, "Drone shot over the city");
    app.wait(cx, videocrm::video_page::SAVE_DELAY);
    assert!(app.has(cx, "not-saved"), "the page shows the edit isn't saved");
    assert_eq!(app.notifications(cx), 1, "the failure is reported once");
    app.wait(cx, videocrm::video_page::SAVE_RETRY_DELAY);
    assert_eq!(app.notifications(cx), 1, "retries stay quiet");

    // The database accepts writes again: the next retry saves the edit.
    store.lock().expect("store lock").insert_video(&video).expect("restore video");
    app.wait(cx, videocrm::video_page::SAVE_RETRY_DELAY);
    assert!(!app.has(cx, "not-saved"));
    assert_eq!(app.stored_blocks(cx, id, DocumentKind::Script)[0], "Drone shot over the city");

    // A failure right before quitting is written by the quit itself.
    store.lock().expect("store lock").delete_video(id).expect("delete video");
    app.type_text(cx, " at dusk");
    app.wait(cx, videocrm::video_page::SAVE_DELAY);
    assert!(app.has(cx, "not-saved"));
    store.lock().expect("store lock").insert_video(&video).expect("restore video");
    cx.update(|cx| cx.shutdown());
    let documents = store.lock().expect("store lock").list_documents(id).expect("list documents");
    let script = documents.iter().find(|document| document.kind == DocumentKind::Script).expect("script");
    assert_eq!(script.blocks[0].text, "Drone shot over the city at dusk");
}

// Markdown

#[gpui_kit::test]
fn without_a_file_chooser_markdown_exports_to_a_typed_path(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, &[]);
    let id = create_video(&app, cx, "Build a home studio", TemplateKind::Tutorial);
    app.open_video(cx, id, VideoTab::Script);
    let page = app.page(cx);
    let script = cx.read(|cx| page.read(cx).script().cloned().expect("script"));

    let tab = script.clone();
    app.step(cx, move |window, cx| tab.update(cx, |tab, cx| tab.prompt_for_path(PathPurpose::Export, window, cx)));
    assert!(app.has_dialog(cx));
    let suggested = field_text(&app, cx, "path-prompt-input");
    assert!(suggested.ends_with("build-a-home-studio.md"), "a file in the media folder: {suggested}");

    // A relative path is explained, not written.
    app.press(cx, "ctrl-a");
    app.type_text(cx, "script.md");
    app.press(cx, "enter");
    assert!(app.has_dialog(cx));
    assert!(app.has(cx, "path-prompt-error"));

    let target = app._media.path().join("exported.md");
    app.press(cx, "ctrl-a");
    app.type_text(cx, &target.display().to_string());
    app.press(cx, "enter");
    assert!(!app.has_dialog(cx));
    let written = std::fs::read_to_string(&target).expect("exported file");
    assert!(written.contains("## "), "the script as Markdown: {written}");
    assert_eq!(app.notifications(cx), 1, "says where it was written");

    // Import reads a typed path and asks how to add it.
    let source = app._media.path().join("import.md");
    std::fs::write(&source, "## Cold open\n\nEverything on this desk earned its spot.\n").expect("write source");
    let tab = script.clone();
    app.step(cx, move |window, cx| tab.update(cx, |tab, cx| tab.prompt_for_path(PathPurpose::Import, window, cx)));
    app.type_text(cx, &source.display().to_string());
    app.press(cx, "enter");
    assert!(app.has(cx, "import-append"), "the import dialog asks Append or Replace");
    app.press(cx, "enter");
    assert!(!app.has_dialog(cx));
    let stored = app.stored_blocks(cx, id, DocumentKind::Script);
    assert_eq!(stored.last().map(String::as_str), Some("Everything on this desk earned its spot."));
}

#[gpui_kit::test]
fn export_writes_where_the_file_chooser_says(cx: &mut TestAppContext) {
    let app = Fixture::open(cx, &[]);
    let id = create_video(&app, cx, "Build a home studio", TemplateKind::Tutorial);
    app.open_video(cx, id, VideoTab::Script);
    app.click(cx, "export-markdown");
    let target = app._media.path().join("chosen.md");
    let chosen = target.clone();
    cx.simulate_new_path_selection(move |_| Some(chosen));
    app.step(cx, |_, _| {});
    assert!(std::fs::read_to_string(&target).expect("exported file").contains("## "));
}
