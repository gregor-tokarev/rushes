//! The title bar: sidebar toggle, history, breadcrumb, search and theme.

use gpui_kit::assets::IconName;
use gpui_kit::component::breadcrumb::{Breadcrumb, BreadcrumbItem};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::sidebar::SidebarToggleButton;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, TitleBar, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, Context, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, SharedString, Styled as _,
    Window, div,
};

use super::elements::shortcut_hint;
use super::sidebar::{display_title, video_emoji};
use super::{GoBack, GoForward, OpenCommandPalette, ToggleSidebar, ToggleTheme, Workspace};
use crate::route::{Route, VideoTab};
use crate::state::AppState;

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

/// Where the main area is: `Videos › Board`, `Videos › 🎬 Title › Script`.
fn breadcrumb(state: &gpui_kit::Entity<AppState>, cx: &App) -> Breadcrumb {
    let navigate = |route: Route| {
        let state = state.clone();
        move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
            state.update(cx, |state, cx| state.navigate(route.clone(), cx));
        }
    };
    let app = state.read(cx);
    let videos = || BreadcrumbItem::new("Videos").on_click(navigate(Route::default()));
    match app.route().clone() {
        Route::Database { view } => {
            let view = view
                .and_then(|id| app.views().iter().find(|known| known.id == id))
                .or_else(|| app.views().iter().min_by_key(|view| view.position));
            Breadcrumb::new().child(videos()).when_some(view, |this, view| this.child(view.name.clone()))
        }
        Route::Video { id, tab } => match app.video(id) {
            Some(video) => {
                let page: SharedString =
                    format!("{} {}", video_emoji(video.icon.as_deref()), shorten(&display_title(&video.title))).into();
                Breadcrumb::new()
                    .child(videos())
                    .child(BreadcrumbItem::new(page).on_click(navigate(Route::Video { id, tab: VideoTab::Script })))
                    .child(tab.label())
            }
            None => Breadcrumb::new().child(videos()).child("Not found"),
        },
        Route::Pipelines => Breadcrumb::new().child("Pipelines"),
        Route::Uploads => Breadcrumb::new().child("Uploads"),
        Route::Settings => Breadcrumb::new().child("Settings"),
        Route::Onboarding => Breadcrumb::new().child("Welcome"),
    }
}
