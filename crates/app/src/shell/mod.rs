//! Window root: title bar, sidebar and the routed main area.
//!
//! [`Workspace`] owns one entity per page and keeps them in step with
//! [`AppState`]: the database panel is persistent, a [`VideoPage`] lives per
//! opened video, and the other pages are created the first time they are shown.

pub(crate) mod elements;
mod palette;
mod sidebar;
mod signals;
mod startup;
mod theme;
mod title_bar;

use chrono::{Local, NaiveDate};
use gpui_kit::component::button::Button;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, Focusable, InteractiveElement as _, IntoElement,
    KeyBinding, ParentElement as _, Render, Styled as _, Subscription, TestSupportExt as _, Window, actions, div,
};
use videocrm_core::settings::ThemeMode;
use videocrm_core::{VideoRow, VideoStatus, ViewId};

use crate::ai_edit::PipelinesPage;
use crate::database::{DatabaseEvent, DatabasePanel};
use crate::onboarding::Onboarding;
use crate::publish::{AccountBadge, UploadsPage};
use crate::route::{Route, SettingsSection, VideoTab};
use crate::settings_page::SettingsPage;
use crate::state::{AppEvent, AppState, Problem, Remedy};
use crate::video_page::VideoPage;
use crate::wizard;

use palette::CommandPalette;
pub use signals::on_termination_signal;
pub use startup::{InstanceLock, StartupNotice, StartupProblem, open_startup_notice};
pub use theme::{apply_theme, register_fonts};

actions!(
    videocrm,
    [
        /// Open the New video dialog.
        NewVideo,
        /// Open the command palette.
        OpenCommandPalette,
        /// Go to the previous page.
        GoBack,
        /// Go to the next page after going back.
        GoForward,
        /// Show or hide the sidebar.
        ToggleSidebar,
        /// Switch between the light and dark theme.
        ToggleTheme,
        /// Open the settings page.
        OpenSettings,
        /// Save pending edits and quit.
        Quit,
    ]
);

const KEY_CONTEXT: &str = "Workspace";

/// Binds the application's keys and the app-wide Quit. Call once at startup,
/// after `gpui_kit::init` and before opening windows.
///
/// These bindings have no context, and GPUI ranks a context-free binding above
/// every context-scoped one, so none of them may reuse a key that a component
/// binds for itself (the editor's Ctrl+B, the table's Enter).
pub fn init(cx: &mut App) {
    // Pending saves are written by each workspace's quit hook.
    cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
    cx.bind_keys([
        KeyBinding::new("secondary-n", NewVideo, None),
        KeyBinding::new("secondary-k", OpenCommandPalette, None),
        KeyBinding::new("secondary-\\", ToggleSidebar, None),
        KeyBinding::new("secondary-,", OpenSettings, None),
        KeyBinding::new("secondary-q", Quit, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-[", GoBack, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-]", GoForward, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("alt-left", GoBack, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("alt-right", GoForward, None),
    ]);
}

/// The page the main area shows for the current route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Database,
    Video,
    Pipelines,
    Uploads,
    Settings,
    Onboarding,
}

impl Page {
    fn of(route: &Route) -> Self {
        match route {
            Route::Database { .. } => Page::Database,
            Route::Video { .. } => Page::Video,
            Route::Pipelines => Page::Pipelines,
            Route::Uploads => Page::Uploads,
            Route::Settings { .. } => Page::Settings,
            Route::Onboarding => Page::Onboarding,
        }
    }
}

pub struct Workspace {
    state: Entity<AppState>,
    database: Entity<DatabasePanel>,
    video_page: Option<Entity<VideoPage>>,
    pipelines: Option<Entity<PipelinesPage>>,
    uploads: Option<Entity<UploadsPage>>,
    settings: Option<Entity<SettingsPage>>,
    onboarding: Option<Entity<Onboarding>>,
    account: Entity<AccountBadge>,
    palette: Entity<CommandPalette>,
    sidebar_collapsed: bool,
    /// The theme mode last applied, so unrelated settings changes leave the theme alone.
    theme_mode: ThemeMode,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let database = cx.new(|cx| DatabasePanel::new(window, cx));
        let account = cx.new(|cx| AccountBadge::new(window, cx));
        let palette = cx.new(|cx| CommandPalette::new(window, cx));
        let theme_mode = state.read(cx).settings().theme;

        let subscriptions = vec![
            cx.observe(&state, |_, _, cx| cx.notify()),
            cx.subscribe_in(&state, window, Self::on_app_event),
            cx.subscribe_in(&database, window, Self::on_database_event),
            cx.observe_window_appearance(window, |this, window, cx| {
                if this.theme_mode == ThemeMode::System {
                    apply_theme(ThemeMode::System, Some(window), cx);
                }
            }),
            cx.on_app_quit(|this, cx| {
                this.flush_pending_saves(cx);
                async {}
            }),
            // When the focused element goes away (a page left, a dialog closed
            // over it), the workspace's keys keep working.
            cx.on_focus_lost(window, |this, window, cx| {
                let target = window.focus_lost_restore_target(cx).unwrap_or_else(|| this.focus_handle.clone());
                target.focus(window, cx);
            }),
        ];

        let mut workspace = Self {
            state,
            database,
            video_page: None,
            pipelines: None,
            uploads: None,
            settings: None,
            onboarding: None,
            account,
            palette,
            sidebar_collapsed: false,
            theme_mode,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        workspace.sync_data(window, cx);
        workspace.sync_route(window, cx);
        let problems = workspace.state.update(cx, |state, _| state.take_startup_problems());
        if !problems.is_empty() {
            // The window's notification layer exists once the window does.
            window.defer(cx, move |window, cx| {
                for problem in &problems {
                    window.push_notification(problem_notification(problem), cx);
                }
            });
        }
        workspace
    }

    pub fn state(&self) -> &Entity<AppState> {
        &self.state
    }

    pub fn database(&self) -> &Entity<DatabasePanel> {
        &self.database
    }

    /// The page of the video opened last, kept while other pages are shown.
    pub fn video_page(&self) -> Option<&Entity<VideoPage>> {
        self.video_page.as_ref()
    }

    pub fn onboarding(&self) -> Option<&Entity<Onboarding>> {
        self.onboarding.as_ref()
    }

    /// The page the main area currently shows.
    pub fn page(&self, cx: &App) -> Page {
        Page::of(self.state.read(cx).route())
    }

    pub fn is_sidebar_collapsed(&self) -> bool {
        self.sidebar_collapsed
    }

    pub fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
        cx.notify();
    }

    /// Writes edits that are waiting for their save delay.
    pub fn flush_pending_saves(&mut self, cx: &mut App) {
        if let Some(page) = &self.video_page {
            page.update(cx, |page, cx| page.flush(cx));
        }
    }

    pub fn open_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = cx.weak_entity();
        self.palette.update(cx, |palette, cx| palette.open(workspace, window, cx));
    }

    /// Switches between light and dark, leaving System for an explicit choice in Settings.
    pub fn toggle_theme(&mut self, cx: &mut Context<Self>) {
        let mode = if cx.theme().is_dark() { ThemeMode::Light } else { ThemeMode::Dark };
        self.state.update(cx, |state, cx| state.update_settings(cx, |settings| settings.theme = mode));
    }

    fn on_app_event(&mut self, _: &Entity<AppState>, event: &AppEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            AppEvent::DataChanged => self.sync_data(window, cx),
            AppEvent::RouteChanged => self.sync_route(window, cx),
            AppEvent::SettingsChanged => {
                let mode = self.state.read(cx).settings().theme;
                if mode != self.theme_mode {
                    self.theme_mode = mode;
                    apply_theme(mode, Some(window), cx);
                }
            }
            AppEvent::Error(message) => {
                window.push_notification(Notification::error(message.clone()), cx);
            }
            AppEvent::Problem(problem) => window.push_notification(problem_notification(problem), cx),
        }
    }

    fn sync_data(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let (rows, properties, views) = (state.rows().to_vec(), state.properties().to_vec(), state.views().to_vec());
        self.database.update(cx, |panel, cx| panel.set_data(rows, properties, views, window, cx));
    }

    /// Points the main area at the current route, creating the page it needs.
    fn sync_route(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let route = self.state.read(cx).route().clone();
        if !matches!(route, Route::Video { .. }) {
            self.flush_pending_saves(cx);
        }
        match route {
            Route::Database { view } => {
                if let Some(view) = self.shown_view(view, cx) {
                    self.database.update(cx, |panel, cx| panel.set_active_view(view, window, cx));
                }
            }
            Route::Video { id, tab } => match &self.video_page {
                Some(page) if page.read(cx).video_id() == id => {
                    page.update(cx, |page, cx| page.set_tab(tab, window, cx));
                }
                _ => {
                    self.flush_pending_saves(cx);
                    self.video_page = Some(cx.new(|cx| VideoPage::new(id, tab, window, cx)));
                }
            },
            Route::Pipelines => {
                if self.pipelines.is_none() {
                    self.pipelines = Some(cx.new(|cx| PipelinesPage::new(window, cx)));
                }
            }
            Route::Uploads => {
                if self.uploads.is_none() {
                    self.uploads = Some(cx.new(|cx| UploadsPage::new(window, cx)));
                }
            }
            Route::Settings { section } => match &self.settings {
                Some(page) => page.update(cx, |page, cx| page.open_section(section, cx)),
                None => self.settings = Some(cx.new(|cx| SettingsPage::new(section, window, cx))),
            },
            Route::Onboarding => {
                if self.onboarding.is_none() {
                    self.onboarding = Some(cx.new(|cx| Onboarding::new(window, cx)));
                }
            }
        }
        cx.notify();
    }

    /// The saved view the database page shows for a route's `view`: that view
    /// while it exists (history can outlive it), otherwise the first one.
    fn shown_view(&self, view: Option<ViewId>, cx: &App) -> Option<ViewId> {
        let views = self.state.read(cx).views();
        view.filter(|id| views.iter().any(|known| known.id == *id))
            .or_else(|| views.iter().min_by_key(|known| known.position).map(|known| known.id))
    }

    fn on_database_event(
        &mut self,
        _: &Entity<DatabasePanel>,
        event: &DatabaseEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            DatabaseEvent::CreateVideo { status } => wizard::open_new_video_dialog(*status, window, cx),
            event => {
                let event = event.clone();
                self.state.update(cx, |state, cx| apply_database_event(state, event, cx));
            }
        }
    }

    fn new_video(&mut self, _: &NewVideo, window: &mut Window, cx: &mut Context<Self>) {
        wizard::open_new_video_dialog(None::<VideoStatus>, window, cx);
    }

    fn on_open_command_palette(&mut self, _: &OpenCommandPalette, window: &mut Window, cx: &mut Context<Self>) {
        self.open_command_palette(window, cx);
    }

    fn go_back(&mut self, _: &GoBack, _: &mut Window, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| state.go_back(cx));
    }

    fn go_forward(&mut self, _: &GoForward, _: &mut Window, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| state.go_forward(cx));
    }

    fn on_toggle_sidebar(&mut self, _: &ToggleSidebar, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_sidebar(cx);
    }

    fn on_toggle_theme(&mut self, _: &ToggleTheme, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_theme(cx);
    }

    fn open_settings(&mut self, _: &OpenSettings, _: &mut Window, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| state.open_settings(SettingsSection::General, cx));
    }

    fn render_main(&self, cx: &mut Context<Self>) -> AnyElement {
        let route = self.state.read(cx).route().clone();
        let page: Option<AnyElement> = match route {
            Route::Database { .. } => Some(self.render_database_page(cx)),
            Route::Video { .. } => self.video_page.clone().map(IntoElement::into_any_element),
            Route::Pipelines => self.pipelines.clone().map(IntoElement::into_any_element),
            Route::Uploads => self.uploads.clone().map(IntoElement::into_any_element),
            Route::Settings { .. } => self.settings.clone().map(IntoElement::into_any_element),
            Route::Onboarding => self.onboarding.clone().map(IntoElement::into_any_element),
        };
        let page_id = match Page::of(&route) {
            Page::Database => "database-page",
            Page::Video => "video-page",
            Page::Pipelines => "pipelines-page",
            Page::Uploads => "uploads-page",
            Page::Settings => "settings-page",
            Page::Onboarding => "onboarding-page",
        };
        v_flex()
            .id(page_id)
            .test_support()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .bg(cx.theme().background)
            .children(page)
            .into_any_element()
    }

    fn render_database_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let summary = production_summary(self.state.read(cx).rows(), Local::now().date_naive());
        v_flex()
            .size_full()
            .min_h_0()
            .child(
                v_flex()
                    .flex_shrink_0()
                    .px_4()
                    .pt_6()
                    .pb_2()
                    .gap_1()
                    .child(div().text_2xl().font_semibold().child("Videos"))
                    .child(div().text_sm().text_color(cx.theme().muted_foreground).child(summary)),
            )
            .child(div().flex_1().min_h_0().child(self.database.clone()))
            .into_any_element()
    }
}

/// An error notification for `problem`, with a button for its remedy. The
/// same problem raised again replaces the one shown instead of stacking.
fn problem_notification(problem: &Problem) -> Notification {
    let notification = Notification::error(problem.message().clone())
        .title(problem.title().clone())
        .id1::<Problem>(problem.title().clone());
    let Some(remedy) = problem.remedy() else { return notification };
    notification.action(move |_, _, cx| {
        Button::new("problem-remedy").small().label(remedy.label()).on_click(cx.listener(
            move |notification, _, window, cx| {
                notification.dismiss(window, cx);
                let Remedy::OpenSettings(section) = remedy;
                AppState::global(cx).update(cx, |state, cx| state.open_settings(section, cx));
            },
        ))
    })
}

/// One line under the database title: what is in production and what ships next.
fn production_summary(rows: &[VideoRow], today: NaiveDate) -> String {
    let in_production = rows
        .iter()
        .filter(|row| matches!(row.video.status, VideoStatus::Scripting | VideoStatus::Filming | VideoStatus::Editing))
        .count();
    let next_publish = rows
        .iter()
        .filter(|row| !matches!(row.video.status, VideoStatus::Published | VideoStatus::Archived))
        .filter_map(|row| row.video.publish_date)
        .filter(|date| *date >= today)
        .min();
    if rows.is_empty() {
        return "Plan every video from idea to upload.".to_string();
    }
    let mut summary = match in_production {
        0 => "Nothing in production".to_string(),
        count => format!("{count} in production"),
    };
    if let Some(date) = next_publish {
        summary.push_str(&format!(" · next publish {}", date.format("%b %-d")));
    }
    summary
}

/// Persists a change the database panel already made to its own copy.
fn apply_database_event(state: &mut AppState, event: DatabaseEvent, cx: &mut Context<AppState>) {
    match event {
        DatabaseEvent::OpenVideo(id) => state.open_video(id, VideoTab::Script, cx),
        DatabaseEvent::UpdateVideo(video) => state.update_video(video, cx),
        DatabaseEvent::DeleteVideos(ids) => state.delete_videos(&ids, cx),
        DatabaseEvent::DuplicateVideo(id) => {
            state.duplicate_video(id, cx);
        }
        DatabaseEvent::CreateProperty(property) => state.insert_property(property, cx),
        DatabaseEvent::UpdateProperty(property) => state.update_property(property, cx),
        DatabaseEvent::DeleteProperty(id) => state.delete_property(id, cx),
        DatabaseEvent::SaveView(view) | DatabaseEvent::CreateView(view) => state.save_view(view, cx),
        DatabaseEvent::DeleteView(id) => state.delete_view(id, cx),
        DatabaseEvent::ActiveViewChanged(view) => state.navigate(Route::Database { view: Some(view) }, cx),
        DatabaseEvent::CreateVideo { .. } => {}
    }
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let onboarding = self.page(cx) == Page::Onboarding;
        v_flex()
            .id("workspace")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            // Onboarding owns the window until it completes.
            .when(!onboarding, |this| {
                this.on_action(cx.listener(Self::new_video))
                    .on_action(cx.listener(Self::on_open_command_palette))
                    .on_action(cx.listener(Self::go_back))
                    .on_action(cx.listener(Self::go_forward))
                    .on_action(cx.listener(Self::on_toggle_sidebar))
                    .on_action(cx.listener(Self::on_toggle_theme))
                    .on_action(cx.listener(Self::open_settings))
            })
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .font_family(cx.theme().font_family.clone())
            .child(self.render_title_bar(onboarding, window, cx))
            .child(
                h_flex()
                    .items_stretch()
                    .flex_1()
                    .min_h_0()
                    .when(!onboarding, |this| this.child(self.render_sidebar(window, cx)))
                    .child(self.render_main(cx)),
            )
    }
}
