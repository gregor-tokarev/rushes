//! The publish views on a sample workspace, for trying them by hand and for
//! screenshots: a minimal stand-in for the app shell with the Publish tab, the
//! Uploads page, the YouTube settings and the sidebar account badge.
//!
//! Data (database, credentials, media) lives in `$VIDEOCRM_DATA_DIR`, which must be set.
//!
//! `VIDEOCRM_DATA_DIR=/tmp/vcrm cargo run -p videocrm --example publish_demo [-- --dark] [--video <title>]`

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Selectable as _, Theme, ThemeMode, h_flex, v_flex};
use gpui_kit::{
    App, AppContext as _, Bounds, Context, Entity, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Subscription, Window, WindowBounds, WindowOptions, div, point, px, rems, size,
};
use videocrm::database::WithDatabaseIcons;
use videocrm::publish::{AccountBadge, PublishPanel, UploadsPage, WithPublishIcons, YoutubeService, YoutubeSettings};
use videocrm::route::{Route, VideoTab};
use videocrm::state::AppState;
use videocrm_core::Store;
use videocrm_core::settings::{DATA_DIR_ENV, data_dir, database_path};

struct Demo {
    state: Entity<AppState>,
    panel: Entity<PublishPanel>,
    uploads: Entity<UploadsPage>,
    settings: Entity<YoutubeSettings>,
    badge: Entity<AccountBadge>,
    _subscription: Subscription,
}

impl Demo {
    fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let Route::Video { id, .. } = state.read(cx).route().clone() else {
            panic!("the demo starts on a video");
        };
        Self {
            panel: cx.new(|cx| PublishPanel::new(id, window, cx)),
            uploads: cx.new(|cx| UploadsPage::new(window, cx)),
            settings: cx.new(|cx| YoutubeSettings::new(window, cx)),
            badge: cx.new(|cx| AccountBadge::new(window, cx)),
            _subscription: cx.observe(&state, |_, _, cx| cx.notify()),
            state,
        }
    }

    fn nav_button(&self, id: &'static str, label: &'static str, icon: IconName, route: Route, cx: &App) -> Button {
        let state = self.state.clone();
        let selected = match (&route, self.state.read(cx).route()) {
            (Route::Video { .. }, Route::Video { .. }) => true,
            (route, current) => route == current,
        };
        Button::new(id).label(label).icon(icon).ghost().w_full().justify_start().selected(selected).on_click(
            move |_, _, cx| {
                let route = route.clone();
                state.update(cx, |state, cx| state.navigate(route, cx));
            },
        )
    }
}

impl Render for Demo {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let route = self.state.read(cx).route().clone();
        let video_route = Route::Video { id: self.panel.read(cx).video_id(), tab: VideoTab::Publish };
        let title: SharedString = self
            .state
            .read(cx)
            .video(self.panel.read(cx).video_id())
            .map(|video| format!("{} {}", video.icon.clone().unwrap_or_default(), video.title).into())
            .unwrap_or_default();
        let main = match route {
            Route::Uploads => self.uploads.clone().into_any_element(),
            Route::Settings => div()
                .size_full()
                .px_8()
                .py_6()
                .child(
                    v_flex()
                        .max_w(rems(42.))
                        .gap_6()
                        .child(div().text_2xl().font_weight(gpui_kit::FontWeight::BOLD).child("YouTube"))
                        .child(self.settings.clone()),
                )
                .into_any_element(),
            _ => v_flex()
                .size_full()
                .child(
                    v_flex()
                        .px_8()
                        .pt_8()
                        .gap_1()
                        .child(div().text_3xl().font_weight(gpui_kit::FontWeight::BOLD).child(title))
                        .child(div().text_sm().text_color(cx.theme().muted_foreground).child("Publish")),
                )
                .child(div().flex_1().min_h_0().child(self.panel.clone()))
                .into_any_element(),
        };
        h_flex()
            .items_stretch()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                v_flex()
                    .w(rems(15.))
                    .flex_shrink_0()
                    .justify_between()
                    .p_2()
                    .bg(cx.theme().sidebar)
                    .border_r_1()
                    .border_color(cx.theme().sidebar_border)
                    .child(
                        v_flex()
                            .gap_0p5()
                            .child(self.nav_button("nav-publish", "Publish", IconName::Upload, video_route, cx))
                            .child(self.nav_button("nav-uploads", "Uploads", IconName::CloudUpload, Route::Uploads, cx))
                            .child(self.nav_button("nav-settings", "Settings", IconName::Settings, Route::Settings, cx)),
                    )
                    .child(self.badge.clone()),
            )
            .child(div().flex_1().min_w_0().child(main))
    }
}

fn main() {
    if std::env::var_os(DATA_DIR_ENV).is_none() {
        eprintln!("publish_demo: set {DATA_DIR_ENV} to a scratch directory");
        std::process::exit(2);
    }
    let args: Vec<String> = std::env::args().collect();
    let dark = args.iter().any(|arg| arg == "--dark");
    let wanted = args.iter().position(|arg| arg == "--video").and_then(|ix| args.get(ix + 1)).cloned();

    let mut store = match Store::open(database_path()) {
        Ok(store) => store,
        Err(error) => {
            eprintln!("publish_demo: {error}");
            std::process::exit(1);
        }
    };
    let prepared = (|| {
        let mut settings = store.load_settings()?;
        settings.onboarding_completed = true;
        settings.media_root.get_or_insert_with(|| data_dir().join("media"));
        store.save_settings(&settings)?;
        if store.list_videos()?.is_empty() {
            store.seed_sample_workspace()?;
        }
        AppState::new(store)
    })();
    let state = match prepared {
        Ok(state) => state,
        Err(error) => {
            eprintln!("publish_demo: {error}");
            std::process::exit(1);
        }
    };

    gpui_kit::application().with_assets(WithPublishIcons(WithDatabaseIcons(gpui_kit::assets::Assets))).run(move |cx| {
        gpui_kit::init(cx);
        Theme::update(cx, |theme| theme.font_family = "Inter".into());
        if dark {
            Theme::change(ThemeMode::Dark, None, cx);
        }
        let state = cx.new(|_| state);
        AppState::set_global(state.clone(), cx);
        YoutubeService::init(state.clone(), cx);
        state.update(cx, |state, cx| {
            let video = state
                .rows()
                .iter()
                .find(|row| wanted.as_ref().is_none_or(|wanted| row.video.title.contains(wanted.as_str())))
                .map(|row| row.video.id);
            if let Some(id) = video {
                state.ensure_video_folder(id, cx);
                state.open_video(id, VideoTab::Publish, cx);
            }
        });
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(point(px(0.), px(0.)), size(px(1440.), px(900.))))),
            ..WindowOptions::default()
        };
        gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| Demo::new(state, window, cx)))
            .expect("failed to open the demo window");
    });
}
