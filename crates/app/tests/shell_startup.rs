//! UI integration tests for starting up: the notice shown when VideoCRM can't
//! open its data, and a workspace whose settings were written by another version.

use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext as _, Bounds, TestAppContext, WindowBounds, WindowOptions, px, size};
use videocrm::ai_edit::PipelineService;
use videocrm::publish::YoutubeService;
use videocrm::route::Route;
use videocrm::shell::{self, StartupNotice, StartupProblem, Workspace};
use videocrm::state::AppState;
use videocrm_core::{Store, StoreError};

fn options() -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(Default::default(), size(px(520.), px(240.))))),
        ..Default::default()
    }
}

#[gpui_kit::test]
fn a_second_copy_says_videocrm_is_already_open(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let dir = tempfile::tempdir().expect("temp dir");
    let data_dir = dir.path().to_path_buf();
    let (window, notice) = cx.update(|cx| {
        gpui_kit::open_window(options(), cx, |_, cx| {
            cx.new(|cx| StartupNotice::new(StartupProblem::AlreadyRunning, data_dir, cx))
        })
        .expect("open window")
    });
    let title = cx.read(|cx| notice.read(cx).title().clone());
    assert_eq!(title, "VideoCRM is already open");
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        // Nothing in the data folder to act on: only the acknowledgement.
        assert!(window.try_find("startup-open-data-folder").is_none());
        assert_eq!(window.find("startup-quit").label(), Some("OK"));
    })
    .expect("window is open");
}

#[gpui_kit::test]
fn a_damaged_database_offers_its_folder(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("videocrm.db");
    std::fs::write(&file, vec![7u8; 8192]).expect("write garbage");
    let error = Store::open(&file).expect_err("garbage is not a database");
    assert!(matches!(error, StoreError::Sqlite(_)));

    let data_dir = dir.path().to_path_buf();
    let (window, notice) = cx.update(|cx| {
        gpui_kit::open_window(options(), cx, |_, cx| {
            cx.new(|cx| StartupNotice::new(StartupProblem::Database(error), data_dir, cx))
        })
        .expect("open window")
    });
    let (title, message) = cx.read(|cx| (notice.read(cx).title().clone(), notice.read(cx).message().clone()));
    assert_eq!(title, "VideoCRM can’t read its database");
    assert!(message.contains("is damaged or isn’t a VideoCRM database"), "{message}");
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("startup-open-data-folder").is_some());
        assert_eq!(window.find("startup-quit").label(), Some("Quit"));
    })
    .expect("window is open");
}

#[gpui_kit::test]
fn settings_from_another_version_still_open_the_videos(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        shell::init(cx);
    });
    let mut store = Store::open_in_memory().expect("in-memory store");
    let settings = serde_json::json!({ "onboarding_completed": true, "theme": "high_contrast" });
    store.set_setting("app", &settings).expect("write settings");
    let state = AppState::new(store).expect("unreadable settings still load");

    let (window, state) = cx.update(|cx| {
        let state = cx.new(|_| state);
        AppState::set_global(state.clone(), cx);
        YoutubeService::init(state.clone(), cx);
        PipelineService::init(state.clone(), cx);
        let handle = state.clone();
        let (window, _) =
            gpui_kit::open_window(options(), cx, |window, cx| cx.new(|cx| Workspace::new(handle, window, cx)))
                .expect("open window");
        (window, state)
    });
    cx.run_until_parked();

    // The readable settings apply: onboarding is done, so the videos show.
    assert_eq!(cx.read(|cx| state.read(cx).route().clone()), Route::default());
    let notifications = cx
        .update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            window.notifications(cx).len()
        })
        .expect("window is open");
    assert_eq!(notifications, 1);
}
