use gpui_kit::component::TitleBar;
use gpui_kit::*;
use videocrm::ai_edit::PipelineService;
use videocrm::publish::YoutubeService;
use videocrm::shell::{self, Workspace};
use videocrm::state::AppState;
use videocrm_core::Store;
use videocrm_core::settings::database_path;

fn main() {
    let store = match Store::open(database_path()) {
        Ok(store) => store,
        Err(error) => {
            eprintln!("videocrm: cannot open the database at {}: {error}", database_path().display());
            std::process::exit(1);
        }
    };
    let state = match AppState::new(store) {
        Ok(state) => state,
        Err(error) => {
            eprintln!("videocrm: cannot load the workspace: {error}");
            std::process::exit(1);
        }
    };

    // The complete Lucide catalog: covers the icons of every feature module
    // (editor, database, publish, AI edit), which are Lucide icons too.
    gpui_kit::application().with_assets(gpui_kit::assets::AllAssets).run(move |cx| {
        gpui_kit::init(cx);
        shell::register_fonts(cx);
        shell::init(cx);
        let theme = state.settings().theme;
        let state = cx.new(|_| state);
        AppState::set_global(state.clone(), cx);
        YoutubeService::init(state.clone(), cx);
        PipelineService::init(state.clone(), cx);
        shell::apply_theme(theme, None, cx);

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(1440.), px(900.)), cx))),
            window_min_size: Some(size(px(960.), px(600.))),
            titlebar: Some(TitlebarOptions { title: Some("VideoCRM".into()), ..TitleBar::title_bar_options() }),
            app_id: Some("videocrm".into()),
            #[cfg(target_os = "linux")]
            window_decorations: Some(WindowDecorations::Client),
            ..TitleBar::window_options()
        };
        let opened = gpui_kit::open_window(options, cx, |window, cx| {
            window.set_window_title("VideoCRM");
            // System mode follows this window's appearance.
            shell::apply_theme(theme, Some(window), cx);
            let workspace = cx.new(|cx| Workspace::new(state, window, cx));
            window.on_window_should_close(cx, {
                let workspace = workspace.downgrade();
                move |_, cx| {
                    _ = workspace.update(cx, |workspace, cx| workspace.flush_pending_saves(cx));
                    cx.quit();
                    true
                }
            });
            let focus = workspace.focus_handle(cx);
            window.defer(cx, move |window, cx| {
                if window.focused(cx).is_none() {
                    focus.focus(window, cx);
                }
            });
            workspace
        });
        if let Err(error) = opened {
            eprintln!("videocrm: cannot open the main window: {error}");
            cx.quit();
        }
    });
}
