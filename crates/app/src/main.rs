use gpui_kit::*;
use videocrm::ai_edit::PipelineService;
use videocrm::publish::YoutubeService;
use videocrm::shell::Workspace;
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

    gpui_kit::application().with_assets(gpui_kit::assets::Assets).run(move |cx| {
        gpui_kit::init(cx);
        let state = cx.new(|_| state);
        AppState::set_global(state.clone(), cx);
        YoutubeService::init(state.clone(), cx);
        PipelineService::init(state.clone(), cx);

        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| Workspace::new(state, window, cx))
        })
        .expect("failed to open the main window");
    });
}
