//! The title bar: sidebar toggle, history, breadcrumb, search and theme.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::sidebar::SidebarToggleButton;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, TitleBar, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, Context, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Role, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div,
};

use super::elements::shortcut_hint;
use super::sidebar::display_title;
use super::{GoBack, GoForward, OpenCommandPalette, ToggleSidebar, ToggleTheme, Workspace};
use crate::route::{Route, VideoTab};
use crate::state::AppState;
use crate::ui::PageIcon;

impl Workspace {
    pub(super) fn render_title_bar(
        &self,
        onboarding: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        if onboarding {
            return TitleBar::new()
                .child(div().text_sm().font_medium().text_color(cx.theme().muted_foreground).child("VideoCRM"))
                .into_any_element();
        }

        let state = self.state.read(cx);
        let (can_go_back, can_go_forward) = (state.can_go_back(), state.can_go_forward());
        let collapsed = self.sidebar_collapsed;
        let workspace = cx.weak_entity();
        let search_shortcut = shortcut_hint(&OpenCommandPalette, window, cx);
        let is_dark = cx.theme().is_dark();

        TitleBar::new()
            .child(
                h_flex()
                    .id("title-bar-navigation")
                    .gap_1()
                    .min_w_0()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        SidebarToggleButton::new()
                            .collapsed(collapsed)
                            .accessibility_label(if collapsed { "Show sidebar" } else { "Hide sidebar" })
                            .on_click(|_, window, cx| window.dispatch_action(Box::new(ToggleSidebar), cx)),
                    )
                    .child(
                        Button::new("go-back")
                            .ghost()
                            .small()
                            .icon(IconName::ArrowLeft)
                            .accessibility_label("Back")
                            .tooltip_with_action("Back", &GoBack, None)
                            .disabled(!can_go_back)
                            .on_click(|_, window, cx| window.dispatch_action(Box::new(GoBack), cx)),
                    )
                    .child(
                        Button::new("go-forward")
                            .ghost()
                            .small()
                            .icon(IconName::ArrowRight)
                            .accessibility_label("Forward")
                            .tooltip_with_action("Forward", &GoForward, None)
                            .disabled(!can_go_forward)
                            .on_click(|_, window, cx| window.dispatch_action(Box::new(GoForward), cx)),
                    )
                    .child(div().ml_2().min_w_0().text_sm().child(breadcrumb(&self.state, cx))),
            )
            .child(
                h_flex()
                    .id("title-bar-actions")
                    .gap_1()
                    .pr_2()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        Button::new("open-search")
                            .ghost()
                            .small()
                            .icon(IconName::Search)
                            .label("Search")
                            .when_some(search_shortcut, |this, hint| this.child(hint))
                            .on_click(move |_, window, cx| {
                                _ = workspace.update(cx, |workspace, cx| workspace.open_command_palette(window, cx));
                            }),
                    )
                    .child(
                        Button::new("toggle-theme")
                            .ghost()
                            .small()
                            .icon(if is_dark { IconName::Sun } else { IconName::Moon })
                            .accessibility_label(if is_dark { "Use light theme" } else { "Use dark theme" })
                            .tooltip(if is_dark { "Use light theme" } else { "Use dark theme" })
                            .on_click(|_, window, cx| window.dispatch_action(Box::new(ToggleTheme), cx)),
                    ),
            )
            .into_any_element()
    }
}

/// Longest video title the breadcrumb shows before eliding the rest.
const BREADCRUMB_TITLE_CHARS: usize = 48;

/// `title`, elided with `…` past [`BREADCRUMB_TITLE_CHARS`] so the title bar keeps room for its actions.
fn shorten(title: &str) -> String {
    match title.char_indices().nth(BREADCRUMB_TITLE_CHARS) {
        Some((end, _)) => format!("{}…", title[..end].trim_end()),
        None => title.to_string(),
    }
}

/// One step of the breadcrumb: a label, the page icon of a video, and where
/// a click on it goes.
struct Crumb {
    label: SharedString,
    page_icon: Option<Option<SharedString>>,
    route: Option<Route>,
}

impl Crumb {
    fn new(label: impl Into<SharedString>) -> Self {
        Self { label: label.into(), page_icon: None, route: None }
    }

    fn to(mut self, route: Route) -> Self {
        self.route = Some(route);
        self
    }

    /// Leads the label with a video's page icon (`None`: it has none).
    fn page_icon(mut self, icon: Option<&str>) -> Self {
        self.page_icon = Some(icon.map(|icon| SharedString::from(icon.to_owned())));
        self
    }
}

/// Where the main area is: `Videos › Board`, `Videos › [icon] Title › Script`.
///
/// The kit's breadcrumb takes text only; this one keeps its geometry (gap,
/// chevron separators, muted steps and a foreground last step) and leads a
/// video's step with its page icon.
fn breadcrumb(state: &gpui_kit::Entity<AppState>, cx: &App) -> impl IntoElement {
    let app = state.read(cx);
    let videos = || Crumb::new("Videos").to(Route::default());
    let crumbs = match app.route().clone() {
        Route::Database { view } => {
            let view = view
                .and_then(|id| app.views().iter().find(|known| known.id == id))
                .or_else(|| app.views().iter().min_by_key(|view| view.position));
            let mut crumbs = vec![videos()];
            crumbs.extend(view.map(|view| Crumb::new(view.name.clone())));
            crumbs
        }
        Route::Video { id, tab } => match app.video(id) {
            Some(video) => vec![
                videos(),
                Crumb::new(shorten(&display_title(&video.title)))
                    .page_icon(video.icon.as_deref())
                    .to(Route::Video { id, tab: VideoTab::Script }),
                Crumb::new(tab.label()),
            ],
            None => vec![videos(), Crumb::new("Not found")],
        },
        Route::Pipelines => vec![Crumb::new("Pipelines")],
        Route::Uploads => vec![Crumb::new("Uploads")],
        Route::Settings { section } => vec![Crumb::new("Settings"), Crumb::new(section.label())],
        Route::Onboarding => vec![Crumb::new("Welcome")],
    };
    let theme = cx.theme();
    let (muted, foreground) = (theme.muted_foreground, theme.foreground);
    let last = crumbs.len().saturating_sub(1);
    h_flex().min_w_0().gap_1p5().text_sm().text_color(muted).children(crumbs.into_iter().enumerate().flat_map(
        |(ix, crumb)| {
            let separator = (ix > 0)
                .then(|| Icon::new(IconName::ChevronRight).size_3p5().text_color(muted).flex_none().into_any_element());
            let state = state.clone();
            let step = h_flex()
                .id(("breadcrumb", ix))
                .min_w_0()
                .gap_1()
                .when(ix == last, |this| this.text_color(foreground))
                .when_some(crumb.page_icon, |this, icon| this.child(PageIcon::new(icon.as_deref())))
                .child(div().min_w_0().truncate().child(crumb.label))
                .when_some(crumb.route, |this, route| {
                    this.role(Role::Link).cursor_pointer().on_click(move |_, _, cx| {
                        state.update(cx, |state, cx| state.navigate(route.clone(), cx));
                    })
                })
                .into_any_element();
            separator.into_iter().chain([step])
        },
    ))
}
