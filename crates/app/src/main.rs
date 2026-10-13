// Log through `log_line!`: `eprintln!` panics once the launching terminal is closed.
#![warn(clippy::print_stderr, clippy::print_stdout)]
use gpui_kit::component::TitleBar;
use gpui_kit::*;
use rushes::ai_edit::PipelineService;
use rushes::publish::YoutubeService;
use rushes::shell::{self, InstanceLock, StartupProblem, Workspace};
use rushes::state::AppState;
use rushes::update::UpdateService;
use rushes_core::Store;
use rushes_core::media::legacy_default_media_root;
use rushes_core::settings::{data_dir, database_path, legacy_data_dir};

fn main() {
    // Before anything else: it sets PATH, which is only safe while no other thread runs.
    shell::extend_tool_path();
    if shell::adopt_rushes_app_name() {
        return;
    }
    let data_dir = data_dir();
    let started = start(&data_dir);

    // The page icon images and the complete Lucide catalog, which covers the
    // icons of every feature module (editor, database, publish, AI edit).
    gpui_kit::application().with_assets(rushes::ui::Assets).run(move |cx| {
        gpui_kit::init(cx);
        shell::register_fonts(cx);
        shell::init(cx);
        match started {
            Ok((lock, state, adopted)) => {
                cx.set_global(lock);
                open_workspace(state, adopted, cx);
            }
            Err(problem) => {
                shell::apply_theme(Default::default(), None, cx);
                shell::open_startup_notice(problem, data_dir, cx);
            }
        }
    });
}

/// Takes the data folder for this process and loads the workspace from it;
/// also says whether the data moved there from VideoCRM just now.
fn start(data_dir: &std::path::Path) -> Result<(InstanceLock, AppState, bool), StartupProblem> {
    let adopted = shell::adopt_legacy_data(legacy_data_dir().as_deref(), data_dir)?;
    // Before the database opens: a second copy of Rushes must not touch it at all.
    let lock = InstanceLock::acquire(data_dir)?;
    let store = Store::open(database_path()).map_err(StartupProblem::Database)?;
    let state = AppState::new(store).map_err(StartupProblem::Database)?;
    Ok((lock, state, adopted))
}

fn open_workspace(state: AppState, adopted_legacy_data: bool, cx: &mut App) {
    let theme = state.settings().theme;
    let state = cx.new(|_| state);
    // VideoCRM's videos point into its default media folder: new ones join them there.
    if let Some(legacy) = legacy_default_media_root().filter(|root| adopted_legacy_data && root.is_dir()) {
        state.update(cx, |state, cx| {
            state.update_settings(cx, |settings| {
                settings.media_root.get_or_insert(legacy);
            })
        });
    }
    AppState::set_global(state.clone(), cx);
    YoutubeService::init(state.clone(), cx);
    PipelineService::init(state.clone(), cx);
    UpdateService::init(state.clone(), cx);
    shell::apply_theme(theme, None, cx);
    // The workspace's quit hook writes pending edits, as for Ctrl+Q.
    shell::on_termination_signal(cx, |cx| cx.quit());

    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(1440.), px(900.)), cx))),
        window_min_size: Some(size(px(960.), px(600.))),
        titlebar: Some(TitlebarOptions { title: Some("Rushes".into()), ..TitleBar::title_bar_options() }),
        app_id: Some("rushes".into()),
        #[cfg(target_os = "linux")]
        window_decorations: Some(WindowDecorations::Client),
        ..TitleBar::window_options()
    };
    let opened = gpui_kit::open_window(options, cx, |window, cx| {
        window.set_window_title("Rushes");
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
        rushes::log_line!("rushes: cannot open the main window: {error}");
        cx.quit();
    }
}
