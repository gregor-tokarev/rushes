//! UI integration tests for the videos database: a real `DatabasePanel` in a
//! headless window, driven through clicks and typing, with the owner's view of
//! it observed through the `DatabaseEvent`s it emits.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use chrono::{TimeZone as _, Utc};
use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, ElementId, Entity, InputEvent as _, Modifiers, MouseButton,
    MouseDownEvent, MouseUpEvent, Pixels, Size, TestAppContext, VisualTestContext, WindowBounds, WindowOptions, point,
    px, size,
};
use videocrm::database::{DatabaseEvent, DatabasePanel};
use videocrm_core::{
    BuiltinField, Column, DatabaseView, FilterSpec, Privacy, PropertyKind, RunStatus, SortDirection, SortSpec,
    UploadStatus, Video, VideoFormat, VideoId, VideoRow, VideoStatus, ViewId, ViewKind, YoutubeLink,
};

struct Fixture {
    window: AnyWindowHandle,
    panel: Entity<DatabasePanel>,
    events: Rc<RefCell<Vec<DatabaseEvent>>>,
    tokyo: VideoId,
    ownership: VideoId,
    desk: VideoId,
    recap: VideoId,
    table: ViewId,
    board: ViewId,
}

impl Fixture {
    fn open(cx: &mut TestAppContext) -> Self {
        Self::open_sized(cx, size(px(1600.), px(900.)))
    }

    fn open_sized(cx: &mut TestAppContext, window_size: Size<Pixels>) -> Self {
        cx.update(gpui_kit::init);

        let video = |title: &str, status, format, tags: &[&str], position: f64| {
            let mut video = Video::new(title);
            video.status = status;
            video.format = format;
            video.tags = tags.iter().map(|tag| tag.to_string()).collect();
            video.position = position;
            video.created_at = Utc.with_ymd_and_hms(2026, 9, 1, 9, 0, 0).unwrap();
            video.updated_at = video.created_at;
            video
        };
        let mut desk = video("Desk setup 2026", VideoStatus::Filming, VideoFormat::LongForm, &[], 2.0);
        desk.hook = "Everything on my desk, and the cable management".into();
        let videos = vec![
            video("Tokyo tech stores tour", VideoStatus::Idea, VideoFormat::LongForm, &["Travel"], 0.0),
            video("Rust ownership in 60 seconds", VideoStatus::Published, VideoFormat::Short, &["Rust"], 1.0),
            desk,
            video("My 2025 channel recap", VideoStatus::Archived, VideoFormat::LongForm, &[], 3.0),
        ];
        let ids: Vec<VideoId> = videos.iter().map(|video| video.id).collect();
        let rows = videos.into_iter().map(row).collect();

        let table = DatabaseView {
            id: ViewId::new(),
            name: "All videos".into(),
            kind: ViewKind::Table,
            columns: Vec::new(),
            sorts: Vec::new(),
            filter: FilterSpec { hide_archived: true, ..FilterSpec::default() },
            position: 0,
        };
        let board = DatabaseView {
            id: ViewId::new(),
            name: "Pipeline".into(),
            kind: ViewKind::Board,
            position: 1,
            ..table.clone()
        };
        let views = vec![table.clone(), board.clone()];

        let (window, panel) = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(Default::default(), window_size))),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    cx.new(|cx| {
                        let mut panel = DatabasePanel::new(window, cx);
                        panel.set_data(rows, Vec::new(), views, window, cx);
                        panel
                    })
                },
            )
            .expect("open test window")
        });

        let events = Rc::new(RefCell::new(Vec::new()));
        cx.update(|cx| {
            let events = events.clone();
            cx.subscribe(&panel, move |_, event: &DatabaseEvent, _| {
                events.borrow_mut().push(event.clone());
            })
            .detach();
        });

        let fixture = Self {
            window,
            panel,
            events,
            tokyo: ids[0],
            ownership: ids[1],
            desk: ids[2],
            recap: ids[3],
            table: table.id,
            board: board.id,
        };
        fixture.render(cx);
        fixture
    }

    /// Runs pending effects, then completes two frames: the table measures
    /// itself in the first one and lays out its rows in the second.
    fn render(&self, cx: &mut TestAppContext) {
        cx.run_until_parked();
        self.with_window(cx, |window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
        });
    }

    fn with_window<R>(
        &self,
        cx: &mut TestAppContext,
        f: impl FnOnce(&mut gpui_kit::Window, &mut gpui_kit::App) -> R,
    ) -> R {
        cx.update_window(self.window, |_, window, cx| f(window, cx)).expect("window is open")
    }

    /// Performs a UI step, then lets its deferred effects run and redraws.
    fn step(&self, cx: &mut TestAppContext, f: impl FnOnce(&mut gpui_kit::Window, &mut gpui_kit::App)) {
        self.with_window(cx, f);
        self.render(cx);
    }

    /// Clicks the way the platform delivers a click: press and release in
    /// separate updates, so whatever the press opens has settled before the
    /// release reaches the row underneath. `TestWindowExt::click` sends both
    /// in one update, and a popup trigger inside a table row does not open
    /// then. The pointer is not moved onto the target first: after a
    /// synthetic hover the headless table drops the press (the running app,
    /// driven through X11, opens the menu either way).
    fn press_and_release(&self, cx: &mut TestAppContext, id: impl Into<ElementId>) {
        let id = id.into();
        let position = self.with_window(cx, |window, _| window.find(id).bounds().center());
        let press = MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        };
        let release =
            MouseUpEvent { button: MouseButton::Left, position, modifiers: Modifiers::default(), click_count: 1 };
        self.step(cx, |window, cx| {
            window.dispatch_event(press.to_platform_input(), cx);
        });
        self.step(cx, |window, cx| {
            window.dispatch_event(release.to_platform_input(), cx);
        });
    }

    fn shown_rows(&self, cx: &mut TestAppContext) -> Vec<VideoId> {
        let ids = [self.tokyo, self.ownership, self.desk, self.recap];
        self.with_window(cx, |window, _| {
            ids.into_iter().filter(|id| window.try_find(row_id(*id)).is_some_and(|row| row.visible())).collect()
        })
    }

    fn take_events(&self) -> Vec<DatabaseEvent> {
        self.events.borrow_mut().drain(..).collect()
    }

    /// Hands the panel new rows and views, as its owner does after a change.
    fn set_data(
        &self,
        cx: &mut TestAppContext,
        rows: impl FnOnce(Vec<VideoRow>) -> Vec<VideoRow>,
        views: impl FnOnce(Vec<DatabaseView>) -> Vec<DatabaseView>,
    ) {
        let panel = self.panel.clone();
        self.step(cx, |window, cx| {
            panel.update(cx, |panel, cx| {
                let ids = [self.tokyo, self.ownership, self.desk, self.recap];
                let current = ids.iter().filter_map(|id| panel.video(*id)).cloned().map(row).collect();
                let current_views = panel.views().to_vec();
                panel.set_data(rows(current), Vec::new(), views(current_views), window, cx);
            });
        });
    }

    /// Waits out the pause a title click leaves for a double-click.
    fn wait_for_click_delay(&self, cx: &mut TestAppContext) {
        cx.executor().advance_clock(Duration::from_millis(500));
        self.render(cx);
    }

    fn label(&self, cx: &mut TestAppContext, id: String) -> Option<String> {
        self.with_window(cx, |window, _| window.find(id).label().map(str::to_string))
    }
}

fn row(video: Video) -> VideoRow {
    VideoRow {
        video,
        script_words: 0,
        script_seconds: 0,
        latest_run: None,
        has_finished_edit: false,
        latest_upload: None,
    }
}

fn row_id(video: VideoId) -> String {
    format!("video-row-{video}")
}

#[gpui_kit::test]
fn set_data_renders_a_row_per_visible_video(cx: &mut TestAppContext) {
    let db = Fixture::open(cx);

    // The archived recap is hidden by the view's filter.
    assert_eq!(db.shown_rows(cx), vec![db.tokyo, db.ownership, db.desk]);
    assert_eq!(cx.read(|cx| db.panel.read(cx).visible_videos()), vec![db.tokyo, db.ownership, db.desk]);
    assert_eq!(db.with_window(cx, |window, _| window.find(row_id(db.tokyo)).selected()), Some(false));

    // New data from the owner keeps the active view and shows the new row.
    let mut extra = Video::new("SQLite is all you need");
    extra.position = 0.5;
    let extra_id = extra.id;
    db.step(cx, |window, cx| {
        db.panel.update(cx, |panel, cx| {
            let mut rows: Vec<VideoRow> =
                panel.visible_videos().iter().filter_map(|id| panel.video(*id)).cloned().map(row).collect();
            rows.push(row(extra));
            let views = panel.views().to_vec();
            panel.set_data(rows, Vec::new(), views, window, cx);
        });
    });
    assert_eq!(cx.read(|cx| db.panel.read(cx).visible_videos()), vec![db.tokyo, extra_id, db.ownership, db.desk]);
    assert!(db.with_window(cx, |window, _| window.find(row_id(extra_id)).visible()));
    assert!(db.take_events().is_empty(), "set_data reports nothing back");
}

#[gpui_kit::test]
fn search_and_filters_narrow_the_rows(cx: &mut TestAppContext) {
    let db = Fixture::open(cx);

    // Search matches tags case-insensitively, and the hook too.
    db.step(cx, |window, cx| {
        window.click("database-search", cx);
        window.input("TRAVEL", cx);
    });
    assert_eq!(db.shown_rows(cx), vec![db.tokyo]);
    db.step(cx, |window, cx| {
        for _ in 0.."TRAVEL".len() {
            window.press("backspace", cx);
        }
        window.input("cable", cx);
    });
    assert_eq!(db.shown_rows(cx), vec![db.desk]);
    db.step(cx, |window, cx| {
        for _ in 0.."cable".len() {
            window.press("backspace", cx);
        }
    });
    assert_eq!(db.shown_rows(cx), vec![db.tokyo, db.ownership, db.desk]);
    // Searching is transient: it does not change the saved view.
    assert!(db.take_events().is_empty());

    // The Filter popover narrows by format and saves the view.
    db.step(cx, |window, cx| window.click("filter", cx));
    db.step(cx, |window, cx| window.click("filter-format-Short", cx));
    assert_eq!(db.shown_rows(cx), vec![db.ownership]);
    let saved = db.take_events();
    let [DatabaseEvent::SaveView(view)] = saved.as_slice() else {
        panic!("expected one SaveView, got {saved:?}");
    };
    assert_eq!(view.id, db.table);
    assert_eq!(view.filter.formats, vec![VideoFormat::Short]);
    assert!(view.filter.hide_archived);

    // Letting archived videos through brings the recap back for long-form.
    db.step(cx, |window, cx| window.click("filter-format-Short", cx));
    db.step(cx, |window, cx| window.click("filter-hide-archived", cx));
    assert_eq!(db.shown_rows(cx), vec![db.tokyo, db.ownership, db.desk, db.recap]);
}

#[gpui_kit::test]
fn changing_a_status_emits_update_video(cx: &mut TestAppContext) {
    let db = Fixture::open(cx);

    // Through the panel API.
    db.step(cx, |_, cx| {
        db.panel.update(cx, |panel, cx| panel.set_status(db.tokyo, VideoStatus::Scripting, cx));
    });
    let events = db.take_events();
    let [DatabaseEvent::UpdateVideo(video)] = events.as_slice() else {
        panic!("expected one UpdateVideo, got {events:?}");
    };
    assert_eq!((video.id, video.status), (db.tokyo, VideoStatus::Scripting));
    assert!(video.updated_at > video.created_at, "edits stamp the update time");

    // Setting the same status again is not a change.
    db.step(cx, |_, cx| {
        db.panel.update(cx, |panel, cx| panel.set_status(db.tokyo, VideoStatus::Scripting, cx));
    });
    assert!(db.take_events().is_empty());

    // Through the status cell's menu: the fifth status is Ready.
    db.press_and_release(cx, format!("status-{}", db.desk));
    db.step(cx, |window, cx| window.within("popup-menu").click(4usize, cx));
    let events = db.take_events();
    let [DatabaseEvent::UpdateVideo(video)] = events.as_slice() else {
        panic!("expected one UpdateVideo, got {events:?}");
    };
    assert_eq!((video.id, video.status), (db.desk, VideoStatus::Ready));
    assert_eq!(cx.read(|cx| db.panel.read(cx).video(db.desk).map(|video| video.status)), Some(VideoStatus::Ready));
}

#[gpui_kit::test]
fn switching_views_emits_active_view_changed(cx: &mut TestAppContext) {
    let db = Fixture::open(cx);
    assert!(db.with_window(cx, |window, _| { window.try_find("board-column-idea").is_none() }));

    db.step(cx, |window, cx| window.within("database-views").click(1usize, cx));
    assert_eq!(db.take_events(), vec![DatabaseEvent::ActiveViewChanged(db.board)]);
    db.with_window(cx, |window, _| {
        assert!(window.find("board-column-idea").visible());
        assert!(window.find(format!("card-{}", db.tokyo)).visible());
        // Archived only gets a column when the filter lets it through.
        assert!(window.try_find("board-column-archived").is_none());
        assert!(window.try_find(row_id(db.tokyo)).is_none());
    });

    // Clicking the active tab again changes nothing.
    db.step(cx, |window, cx| window.within("database-views").click(1usize, cx));
    assert!(db.take_events().is_empty());

    // The owner switching views is not reported back to it.
    db.step(cx, |window, cx| db.panel.update(cx, |panel, cx| panel.set_active_view(db.table, window, cx)));
    assert!(db.take_events().is_empty());
    assert!(db.with_window(cx, |window, _| window.find(row_id(db.tokyo)).visible()));
}

#[gpui_kit::test]
fn adding_a_property_emits_create_property(cx: &mut TestAppContext) {
    let db = Fixture::open(cx);

    db.step(cx, |window, cx| window.click("properties", cx));
    db.step(cx, |window, cx| window.click("new-property", cx));
    // The dialog focuses the name field; an empty name is rejected.
    db.step(cx, |window, cx| window.click("property-commit", cx));
    assert!(db.take_events().is_empty());
    db.step(cx, |window, cx| {
        window.click("property-name", cx);
        window.input("Sponsor", cx);
    });
    db.step(cx, |window, cx| window.click("property-commit", cx));

    let events = db.take_events();
    let [DatabaseEvent::CreateProperty(property), DatabaseEvent::SaveView(view)] = events.as_slice() else {
        panic!("expected CreateProperty then SaveView, got {events:?}");
    };
    assert_eq!(property.name, "Sponsor");
    assert_eq!(property.kind, PropertyKind::Text);
    // The new property is shown in the view it was added from.
    assert_eq!(view.id, db.table);
    let layout = view.columns.iter().find(|layout| layout.column == Column::Custom(property.id));
    assert!(layout.is_some_and(|layout| layout.visible));
    assert_eq!(cx.read(|cx| db.panel.read(cx).properties().to_vec()), vec![property.clone()]);
    db.with_window(cx, |window, _| {
        assert!(window.try_find("property-name").is_none(), "the dialog closes");
        assert!(window.find(format!("column-{}", property.id)).visible());
    });
}

#[gpui_kit::test]
fn deleting_from_the_row_menu_asks_first(cx: &mut TestAppContext) {
    let db = Fixture::open(cx);

    db.step(cx, |window, cx| window.right_click(row_id(db.ownership), cx));
    // Open, Rename, Duplicate, separator, Delete…
    db.step(cx, |window, cx| window.within("popup-menu").click(4usize, cx));
    assert!(db.take_events().is_empty(), "nothing is deleted before confirming");
    assert_eq!(db.shown_rows(cx), vec![db.tokyo, db.ownership, db.desk]);

    db.step(cx, |window, cx| window.click("ok", cx));
    assert_eq!(db.take_events(), vec![DatabaseEvent::DeleteVideos(vec![db.ownership])]);
    assert_eq!(db.shown_rows(cx), vec![db.tokyo, db.desk]);
}

#[gpui_kit::test]
fn dragging_a_card_moves_it_between_columns(cx: &mut TestAppContext) {
    let db = Fixture::open(cx);
    db.step(cx, |window, cx| db.panel.update(cx, |panel, cx| panel.set_active_view(db.board, window, cx)));

    // Onto the Filming column's only card: Filming, ordered before it.
    db.step(cx, |window, cx| window.drag_to(format!("card-{}", db.tokyo), format!("card-slot-{}", db.desk), cx));
    let events = db.take_events();
    let [DatabaseEvent::UpdateVideo(video)] = events.as_slice() else {
        panic!("expected one UpdateVideo, got {events:?}");
    };
    assert_eq!(video.id, db.tokyo);
    assert_eq!(video.status, VideoStatus::Filming);
    assert!(video.position < 2.0, "placed before the desk video, got {}", video.position);
    db.with_window(cx, |window, _| {
        let moved = window.find(format!("card-{}", db.tokyo)).bounds();
        let column = window.find("board-column-filming").bounds();
        assert!(column.contains(&moved.center()), "the card is drawn in the Filming column");
        assert!(moved.top() < window.find(format!("card-{}", db.desk)).bounds().top());
        assert!(window.try_find("board-column-idea").is_some_and(|column| column.visible()));
    });
}

#[gpui_kit::test]
fn titles_open_the_video_and_new_video_reports_its_status(cx: &mut TestAppContext) {
    let db = Fixture::open(cx);

    db.step(cx, |window, cx| window.click(format!("title-text-{}", db.desk), cx));
    assert!(db.take_events().is_empty(), "a double-click may still follow");
    db.wait_for_click_delay(cx);
    assert_eq!(db.take_events(), vec![DatabaseEvent::OpenVideo(db.desk)]);

    db.step(cx, |window, cx| window.click("new-video", cx));
    assert_eq!(db.take_events(), vec![DatabaseEvent::CreateVideo { status: None }]);

    db.step(cx, |window, cx| db.panel.update(cx, |panel, cx| panel.set_active_view(db.board, window, cx)));
    db.step(cx, |window, cx| window.click("board-add-filming", cx));
    assert_eq!(db.take_events(), vec![DatabaseEvent::CreateVideo { status: Some(VideoStatus::Filming) }]);
}

#[gpui_kit::test]
fn board_cards_are_reached_with_tab_and_open_with_enter(cx: &mut TestAppContext) {
    let db = Fixture::open(cx);
    db.step(cx, |window, cx| db.panel.update(cx, |panel, cx| panel.set_active_view(db.board, window, cx)));
    db.with_window(cx, |window, _| window.activate_window());
    db.step(cx, |window, cx| window.click("database-search", cx));

    let card = format!("card-{}", db.desk);
    for _ in 0..60 {
        if db.with_window(cx, |window, _| window.find(card.clone()).focused()) == Some(true) {
            break;
        }
        db.step(cx, |window, cx| window.press("tab", cx));
    }
    assert_eq!(db.with_window(cx, |window, _| window.find(card.clone()).focused()), Some(true));
    db.step(cx, |window, cx| window.press("enter", cx));
    assert_eq!(db.take_events(), vec![DatabaseEvent::OpenScript(db.desk)]);
}

#[gpui_kit::test]
fn sorting_from_a_column_header_saves_the_view(cx: &mut TestAppContext) {
    let db = Fixture::open(cx);

    db.step(cx, |window, cx| window.click("column-title", cx));
    // Sort ascending, Sort descending, …
    db.step(cx, |window, cx| window.within("popup-menu").click(0usize, cx));
    let events = db.take_events();
    let [DatabaseEvent::SaveView(view)] = events.as_slice() else {
        panic!("expected one SaveView, got {events:?}");
    };
    assert_eq!(
        view.sorts,
        vec![SortSpec { column: Column::Builtin(BuiltinField::Title), direction: SortDirection::Ascending }]
    );
    let sorted = vec![db.desk, db.ownership, db.tokyo];
    assert_eq!(cx.read(|cx| db.panel.read(cx).visible_videos()), sorted);
    db.with_window(cx, |window, _| {
        let tops: Vec<_> = sorted.iter().map(|id| window.find(row_id(*id)).bounds().top()).collect();
        assert!(tops.windows(2).all(|pair| pair[0] < pair[1]), "rows are drawn in sorted order");
    });
}

#[gpui_kit::test]
fn keyboard_renames_and_opens_the_selected_row(cx: &mut TestAppContext) {
    let db = Fixture::open(cx);

    // Between the title text and the Open button, which both open the video.
    db.step(cx, |window, cx| window.click_at(row_id(db.desk), point(px(190.), px(16.)), cx));
    assert_eq!(cx.read(|cx| db.panel.read(cx).selected_video()), Some(db.desk));
    db.step(cx, |window, cx| window.press("f2", cx));
    db.with_window(cx, |window, _| {
        assert_eq!(window.find("cell-input").value(), Some("Desk setup 2026"));
        assert_eq!(window.find("cell-input").focused(), Some(true));
    });
    db.step(cx, |window, cx| {
        window.input(" tour", cx);
        window.press("enter", cx);
    });
    let events = db.take_events();
    let [DatabaseEvent::UpdateVideo(video)] = events.as_slice() else {
        panic!("Enter commits the title and does not open the video, got {events:?}");
    };
    assert_eq!(video.title, "Desk setup 2026 tour");
    assert!(db.with_window(cx, |window, _| window.try_find("cell-input").is_none()));

    // Opened from the keyboard, to write in its script.
    db.step(cx, |window, cx| window.press("enter", cx));
    assert_eq!(db.take_events(), vec![DatabaseEvent::OpenScript(db.desk)]);
}

#[gpui_kit::test]
fn an_emptied_title_is_not_saved_from_the_table(cx: &mut TestAppContext) {
    let db = Fixture::open(cx);
    let title = |db: &Fixture, cx: &mut TestAppContext, id: VideoId| {
        cx.read(|cx| db.panel.read(cx).video(id).map(|video| video.title.clone()))
    };

    // Rename from the row menu: Open, Rename, …
    db.step(cx, |window, cx| window.right_click(row_id(db.ownership), cx));
    db.step(cx, |window, cx| window.within("popup-menu").click(1usize, cx));
    db.with_window(cx, |window, _| {
        assert_eq!(window.find("cell-input").focused(), Some(true), "the title takes the keyboard");
    });
    db.step(cx, |window, cx| {
        window.press("ctrl-a", cx);
        window.press("backspace", cx);
        window.press("enter", cx);
    });
    assert!(db.take_events().is_empty(), "an empty title is not saved");
    assert_eq!(title(&db, cx, db.ownership).as_deref(), Some("Rust ownership in 60 seconds"));
    assert!(db.with_window(cx, |window, _| window.try_find("cell-input").is_none()));

    // F2 on the selected row, with only spaces left. Between the title text
    // and the Open button, which both open the video.
    db.step(cx, |window, cx| window.click_at(row_id(db.desk), point(px(190.), px(16.)), cx));
    db.step(cx, |window, cx| window.press("f2", cx));
    db.step(cx, |window, cx| {
        window.press("ctrl-a", cx);
        window.input("   ", cx);
        window.press("enter", cx);
    });
    assert!(db.take_events().is_empty(), "a blank title is not saved");
    assert_eq!(title(&db, cx, db.desk).as_deref(), Some("Desk setup 2026"));

    // Runs of spaces collapse, as on the video page.
    db.step(cx, |window, cx| window.press("f2", cx));
    db.step(cx, |window, cx| {
        window.press("ctrl-a", cx);
        window.input("  Desk   setup  ", cx);
        window.press("enter", cx);
    });
    assert_eq!(title(&db, cx, db.desk).as_deref(), Some("Desk setup"));
}

#[gpui_kit::test]
fn escape_cancels_an_inline_edit_at_once(cx: &mut TestAppContext) {
    let db = Fixture::open(cx);

    db.step(cx, |window, cx| window.click_at(row_id(db.desk), point(px(190.), px(16.)), cx));
    db.step(cx, |window, cx| window.press("f2", cx));
    db.step(cx, |window, cx| window.input(" tour", cx));
    assert!(db.with_window(cx, |window, _| window.try_find("cell-input").is_some()));

    // The row is still selected, so the table would take Escape to clear the
    // selection; the editor gets it first.
    db.step(cx, |window, cx| window.press("escape", cx));
    assert!(db.with_window(cx, |window, _| window.try_find("cell-input").is_none()), "one Escape closes the editor");
    assert!(db.take_events().is_empty(), "nothing is saved");
    assert_eq!(
        cx.read(|cx| db.panel.read(cx).video(db.desk).map(|video| video.title.clone())),
        Some("Desk setup 2026".to_string())
    );
    assert_eq!(cx.read(|cx| db.panel.read(cx).selected_video()), Some(db.desk), "the row stays selected");

    // Keyboard focus is back on the table: Enter opens the row.
    db.step(cx, |window, cx| window.press("enter", cx));
    assert_eq!(db.take_events(), vec![DatabaseEvent::OpenScript(db.desk)]);
}

#[gpui_kit::test]
fn row_keys_do_not_reach_through_an_open_cell_menu(cx: &mut TestAppContext) {
    let db = Fixture::open(cx);

    db.step(cx, |window, cx| window.click_at(row_id(db.desk), point(px(190.), px(16.)), cx));
    db.press_and_release(cx, format!("status-{}", db.desk));
    assert!(db.with_window(cx, |window, _| window.try_find("popup-menu").is_some()));

    // Backspace and Delete in the status menu do not ask to delete the row.
    db.step(cx, |window, cx| window.press("backspace", cx));
    db.step(cx, |window, cx| window.press("delete", cx));
    assert!(db.with_window(cx, |window, _| window.try_find("ok").is_none()), "no delete confirmation");
    assert!(db.take_events().is_empty());
}

#[gpui_kit::test]
fn clearing_a_search_without_matches_keeps_the_view(cx: &mut TestAppContext) {
    let db = Fixture::open(cx);

    db.step(cx, |window, cx| {
        window.click("database-search", cx);
        window.input("zzz", cx);
    });
    assert!(db.shown_rows(cx).is_empty());
    assert!(
        db.with_window(cx, |window, _| window.try_find("empty-clear-filters").is_none()),
        "a search with no matches offers to clear the search, not the view's filters"
    );

    db.step(cx, |window, cx| window.click("empty-clear-search", cx));
    assert_eq!(db.shown_rows(cx), vec![db.tokyo, db.ownership, db.desk]);
    assert!(db.take_events().is_empty(), "the saved view is untouched");
    assert!(cx.read(|cx| { db.panel.read(cx).active_view().is_some_and(|view| view.filter.hide_archived) }));
}

#[gpui_kit::test]
fn a_typed_option_is_kept_without_pressing_enter(cx: &mut TestAppContext) {
    let db = Fixture::open(cx);

    db.step(cx, |window, cx| window.click("properties", cx));
    db.step(cx, |window, cx| window.click("new-property", cx));
    db.step(cx, |window, cx| {
        window.click("property-name", cx);
        window.input("Sponsor", cx);
    });
    // Text, Number, Select, …
    db.press_and_release(cx, "property-kind");
    db.step(cx, |window, cx| window.within("popup-menu").click(2usize, cx));
    db.step(cx, |window, cx| {
        window.click("new-option", cx);
        window.input("NordVPN", cx);
        window.press("enter", cx);
    });
    db.step(cx, |window, cx| window.input("Brilliant", cx));
    db.step(cx, |window, cx| window.click("property-commit", cx));

    let events = db.take_events();
    let Some(DatabaseEvent::CreateProperty(property)) = events.first() else {
        panic!("expected CreateProperty first, got {events:?}");
    };
    let names: Vec<&str> = property.kind.options().iter().map(|option| option.name.as_str()).collect();
    assert_eq!(names, ["NordVPN", "Brilliant"]);
}

#[gpui_kit::test]
fn number_cells_read_a_decimal_comma_and_say_what_they_refuse(cx: &mut TestAppContext) {
    // Wide enough that the new column is not under the table's edge.
    let db = Fixture::open_sized(cx, size(px(2600.), px(900.)));
    db.step(cx, |window, cx| window.click("properties", cx));
    db.step(cx, |window, cx| window.click("new-property", cx));
    db.step(cx, |window, cx| {
        window.click("property-name", cx);
        window.input("Budget", cx);
    });
    // Text, Number, …
    db.press_and_release(cx, "property-kind");
    db.step(cx, |window, cx| window.within("popup-menu").click(1usize, cx));
    db.step(cx, |window, cx| window.click("property-commit", cx));
    let events = db.take_events();
    let Some(DatabaseEvent::CreateProperty(budget)) = events.first() else {
        panic!("expected CreateProperty first, got {events:?}");
    };
    assert_eq!(budget.kind, PropertyKind::Number);
    let cell = format!("text-{}-{}", budget.id, db.desk);

    let mut edit = |typed: &str| {
        db.press_and_release(cx, cell.clone());
        db.step(cx, |window, cx| {
            window.input(typed, cx);
            window.press("enter", cx);
        });
        db.take_events()
    };
    let events = edit("3,5");
    let [DatabaseEvent::UpdateVideo(video)] = events.as_slice() else {
        panic!("expected the new value, got {events:?}");
    };
    assert_eq!(video.properties.get(&budget.id), Some(&videocrm_core::PropertyValue::Number(3.5)));

    assert!(edit("1,2,3").is_empty(), "nothing is saved");
    let notifications = db.with_window(cx, |window, cx| window.notifications(cx).len());
    assert_eq!(notifications, 1, "the refusal is said");
}

#[gpui_kit::test]
fn the_property_dialog_commits_when_nothing_in_it_has_focus(cx: &mut TestAppContext) {
    let db = Fixture::open(cx);

    db.step(cx, |window, cx| window.click("properties", cx));
    db.step(cx, |window, cx| window.click("new-property", cx));
    db.step(cx, |window, cx| {
        window.click("property-name", cx);
        window.input("Sponsor", cx);
    });
    // The focused field can leave the dialog (its option row removed, say),
    // and footer buttons don't take focus when clicked: Create still reaches
    // the dialog.
    db.step(cx, |window, cx| window.blur(cx));
    db.step(cx, |window, cx| window.click("property-commit", cx));

    let events = db.take_events();
    let Some(DatabaseEvent::CreateProperty(property)) = events.first() else {
        panic!("expected CreateProperty, got {events:?}");
    };
    assert_eq!(property.name, "Sponsor");
    assert!(db.with_window(cx, |window, _| window.try_find("property-commit").is_none()), "the dialog closed");
}

#[gpui_kit::test]
fn resizing_a_column_saves_its_width(cx: &mut TestAppContext) {
    let db = Fixture::open(cx);

    // The resize handle straddles the right border of the Status header
    // cell, the table's second column.
    let (from, to, before) = db.with_window(cx, |window, _| {
        let status = window.find(("col-header", 1usize)).bounds();
        let y = status.center().y;
        (point(status.right() - px(2.), y), point(status.right() + px(58.), y), status.size.width)
    });
    db.step(cx, |window, cx| window.drag(from, to, cx));

    let events = db.take_events();
    let [DatabaseEvent::SaveView(view)] = events.as_slice() else {
        panic!("expected one SaveView, got {events:?}");
    };
    let status = view
        .columns
        .iter()
        .find(|layout| layout.column == Column::Builtin(BuiltinField::Status))
        .expect("the view lists the status column");
    // Dragged 60px from just inside the border: about that much wider than
    // the default 140.
    assert!((190.0..=202.0).contains(&status.width), "expected about 200, got {}", status.width);
    let after = db.with_window(cx, |window, _| window.find(("col-header", 1usize)).bounds().size.width);
    assert!(after > before, "the header is drawn wider");
}

#[gpui_kit::test]
fn double_clicking_a_title_renames_instead_of_opening(cx: &mut TestAppContext) {
    let db = Fixture::open(cx);

    db.step(cx, |window, cx| window.double_click(format!("title-text-{}", db.desk), cx));
    db.with_window(cx, |window, _| {
        assert_eq!(window.find("cell-input").value(), Some("Desk setup 2026"));
        assert_eq!(window.find("cell-input").focused(), Some(true));
    });
    db.wait_for_click_delay(cx);
    assert!(db.take_events().is_empty(), "the first click of the double-click does not open the video");
    db.step(cx, |window, cx| {
        window.input(" tour", cx);
        window.press("enter", cx);
    });
    let events = db.take_events();
    let [DatabaseEvent::UpdateVideo(video)] = events.as_slice() else {
        panic!("expected the rename, got {events:?}");
    };
    assert_eq!(video.title, "Desk setup 2026 tour");

    // The hover Open button opens at once.
    db.step(cx, |window, cx| window.hover(format!("title-text-{}", db.tokyo), cx));
    db.step(cx, |window, cx| window.click(format!("open-{}", db.tokyo), cx));
    assert_eq!(db.take_events(), vec![DatabaseEvent::OpenVideo(db.tokyo)]);
}

#[gpui_kit::test]
fn a_cell_edit_is_saved_when_another_cell_is_edited(cx: &mut TestAppContext) {
    let db = Fixture::open(cx);
    // In a window that isn't active the editor is never told it lost focus.
    VisualTestContext::from_window(db.window, cx).deactivate_window();
    db.render(cx);

    db.step(cx, |window, cx| window.double_click(format!("title-text-{}", db.desk), cx));
    db.step(cx, |window, cx| window.input(" tour", cx));
    db.wait_for_click_delay(cx);
    db.step(cx, |window, cx| window.double_click(format!("title-text-{}", db.tokyo), cx));
    db.with_window(cx, |window, _| {
        assert_eq!(window.find("cell-input").value(), Some("Tokyo tech stores tour"));
    });
    let events = db.take_events();
    let [DatabaseEvent::UpdateVideo(video)] = events.as_slice() else {
        panic!("the first cell's edit is saved, got {events:?}");
    };
    assert_eq!(video.title, "Desk setup 2026 tour");

    // Switching views saves the edit too, rather than dropping it.
    db.step(cx, |window, cx| window.input(" 2", cx));
    let (panel, board) = (db.panel.clone(), db.board);
    db.step(cx, move |window, cx| panel.update(cx, |panel, cx| panel.set_active_view(board, window, cx)));
    let events = db.take_events();
    let [DatabaseEvent::UpdateVideo(video)] = events.as_slice() else {
        panic!("the edit is saved when the view changes, got {events:?}");
    };
    assert_eq!(video.title, "Tokyo tech stores tour 2");
}

#[gpui_kit::test]
fn a_live_video_and_a_finished_edit_win_over_newer_attempts(cx: &mut TestAppContext) {
    let db = Fixture::open(cx);
    let url = "https://www.youtube.com/watch?v=mP4c-0n30Ds";
    let (ownership, tokyo) = (db.ownership, db.tokyo);
    db.set_data(
        cx,
        |rows| {
            rows.into_iter()
                .map(|mut row| {
                    if row.video.id == ownership {
                        row.video.youtube = Some(YoutubeLink {
                            video_id: "mP4c-0n30Ds".into(),
                            url: url.into(),
                            channel_id: "UC1".into(),
                            privacy: Privacy::Public,
                            uploaded_at: Utc::now(),
                        });
                        row.latest_upload = Some(UploadStatus::Cancelled);
                        row.latest_run = Some(RunStatus::Failed);
                        row.has_finished_edit = true;
                    } else if row.video.id == tokyo {
                        row.latest_upload = Some(UploadStatus::Failed { error: "quota".into() });
                        row.latest_run = Some(RunStatus::Failed);
                    }
                    row
                })
                .collect()
        },
        |views| views,
    );

    // On YouTube: its visibility, a link, and the cancelled retry as a hint.
    assert_eq!(db.label(cx, format!("youtube-{ownership}")).as_deref(), Some("Public"));
    assert_eq!(db.label(cx, format!("youtube-attempt-{ownership}")).as_deref(), Some("Latest upload: Cancelled"));
    assert_eq!(db.label(cx, format!("edit-{ownership}")).as_deref(), Some("Done"));
    assert_eq!(db.label(cx, format!("edit-attempt-{ownership}")).as_deref(), Some("Latest edit: Failed"));
    db.step(cx, |window, cx| window.click(format!("open-youtube-{ownership}"), cx));
    assert_eq!(cx.opened_url().as_deref(), Some(url));

    // Never on YouTube and never edited: the attempts are all there is.
    assert_eq!(db.label(cx, format!("youtube-{tokyo}")).as_deref(), Some("Failed"));
    assert_eq!(db.label(cx, format!("edit-{tokyo}")).as_deref(), Some("Failed"));
    db.with_window(cx, |window, _| {
        assert!(window.try_find(format!("youtube-attempt-{tokyo}")).is_none());
        assert!(window.try_find(format!("edit-attempt-{tokyo}")).is_none());
        assert!(window.try_find(format!("open-youtube-{tokyo}")).is_none());
    });
}

#[gpui_kit::test]
fn picking_a_sort_column_keeps_the_sort_popover_open(cx: &mut TestAppContext) {
    let db = Fixture::open(cx);

    db.step(cx, |window, cx| window.click("sort", cx));
    db.step(cx, |window, cx| window.click("sort-add", cx));
    db.step(cx, |window, cx| window.click("sort-field-publishdate", cx));
    let events = db.take_events();
    let [DatabaseEvent::SaveView(view)] = events.as_slice() else {
        panic!("expected one SaveView, got {events:?}");
    };
    let publish_date = Column::Builtin(BuiltinField::PublishDate);
    assert_eq!(view.sorts, vec![SortSpec { column: publish_date, direction: SortDirection::Ascending }]);

    // Still open on the rules: the direction is one click away.
    db.step(cx, |window, cx| window.click("sort-direction-publishdate", cx));
    let events = db.take_events();
    let [DatabaseEvent::SaveView(view)] = events.as_slice() else {
        panic!("expected one SaveView, got {events:?}");
    };
    assert_eq!(view.sorts[0].direction, SortDirection::Descending);

    // Changing a rule's column works the same way.
    db.step(cx, |window, cx| window.click("sort-column-publishdate", cx));
    db.step(cx, |window, cx| window.click("sort-field-title", cx));
    let events = db.take_events();
    let [DatabaseEvent::SaveView(view)] = events.as_slice() else {
        panic!("expected one SaveView, got {events:?}");
    };
    assert_eq!(view.sorts[0].column, Column::Builtin(BuiltinField::Title));
    assert!(db.with_window(cx, |window, _| window.find("sort-direction-title").visible()));
}

#[gpui_kit::test]
fn new_videos_start_in_a_status_the_view_shows(cx: &mut TestAppContext) {
    let db = Fixture::open(cx);
    db.set_data(
        cx,
        |rows| rows,
        |views| {
            views
                .into_iter()
                .map(|mut view| {
                    view.filter.statuses = vec![VideoStatus::Filming, VideoStatus::Scripting];
                    view
                })
                .collect()
        },
    );

    db.step(cx, |window, cx| window.click("new-video", cx));
    db.step(cx, |window, cx| window.click("table-new-video", cx));
    let scripting = DatabaseEvent::CreateVideo { status: Some(VideoStatus::Scripting) };
    assert_eq!(db.take_events(), vec![scripting.clone(), scripting]);
}

#[gpui_kit::test]
fn a_narrow_toolbar_keeps_every_view_reachable(cx: &mut TestAppContext) {
    // The content width of the smallest window with the sidebar open.
    let db = Fixture::open_sized(cx, size(px(704.), px(600.)));

    db.with_window(cx, |window, _| {
        let tabs = window.within("database-views");
        for ix in 0..2usize {
            let tab = tabs.find(ix);
            assert!(tab.visible() && tab.bounds().size.width > px(40.), "tab {ix} shows");
        }
        let tab_line = tabs.find(0usize).bounds();
        let new_video = window.find("new-video").bounds();
        assert!(new_video.top() >= tab_line.bottom(), "the actions wrap below the tabs");
        assert!(new_video.right() <= px(704.));
    });

    // More views than the strip holds: a menu lists every one of them.
    let extra: Vec<DatabaseView> = (0..10)
        .map(|n| DatabaseView {
            id: ViewId::new(),
            name: format!("Production view {n}"),
            kind: ViewKind::Table,
            columns: Vec::new(),
            sorts: Vec::new(),
            filter: FilterSpec::default(),
            position: 2 + n,
        })
        .collect();
    let last = extra[9].id;
    db.set_data(
        cx,
        |rows| rows,
        |mut views| {
            views.extend(extra);
            views
        },
    );
    db.render(cx);
    db.press_and_release(cx, "more");
    db.step(cx, |window, cx| window.within("popup-menu").click(11usize, cx));
    assert_eq!(db.take_events(), vec![DatabaseEvent::ActiveViewChanged(last)]);
}
