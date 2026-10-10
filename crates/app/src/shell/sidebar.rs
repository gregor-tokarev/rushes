//! The workspace sidebar: create and search, the saved database views,
//! pipelines and uploads, recently opened videos, account and settings.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::sidebar::{
    Sidebar, SidebarCollapsible, SidebarGroup, SidebarItem, SidebarMenu, SidebarMenuItem,
};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Collapsible, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, Role, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div,
};
use videocrm_core::{RunStatus, UploadStatus, VideoId, ViewKind};

use super::elements::shortcut_hint;
use super::{NewVideo, OpenCommandPalette, Workspace};
use crate::route::{Route, SettingsSection, VideoTab};
use crate::state::AppState;
use crate::ui::PageIcon;

/// One block of the sidebar. The kit's sidebar takes a single item type, so
/// menus, labelled groups and the recent-pages list share this enum.
#[derive(Clone)]
enum Section {
    Menu(Box<SidebarMenu>),
    Group(SidebarGroup<SidebarMenu>),
    Pages(PageList),
}

impl Collapsible for Section {
    fn is_collapsed(&self) -> bool {
        match self {
            Section::Menu(menu) => menu.is_collapsed(),
            Section::Group(group) => group.is_collapsed(),
            Section::Pages(list) => list.collapsed,
        }
    }

    fn collapsed(self, collapsed: bool) -> Self {
        match self {
            Section::Menu(menu) => Section::Menu(Box::new(menu.collapsed(collapsed))),
            Section::Group(group) => Section::Group(group.collapsed(collapsed)),
            Section::Pages(list) => Section::Pages(PageList { collapsed, ..list }),
        }
    }
}

impl SidebarItem for Section {
    fn render(self, id: impl Into<ElementId>, window: &mut Window, cx: &mut App) -> impl IntoElement {
        match self {
            Section::Menu(menu) => menu.render(id, window, cx).into_any_element(),
            Section::Group(group) => group.render(id, window, cx).into_any_element(),
            Section::Pages(list) => list.render(cx),
        }
    }
}

/// A labelled list of video pages, each led by its page icon. Mirrors the
/// geometry of the kit's group label and menu rows, which only take icons.
#[derive(Clone)]
struct PageList {
    label: SharedString,
    pages: Vec<PageLink>,
    collapsed: bool,
}

#[derive(Clone)]
struct PageLink {
    id: VideoId,
    icon: Option<SharedString>,
    title: SharedString,
    active: bool,
}

impl PageList {
    fn render(self, cx: &mut App) -> AnyElement {
        let theme = cx.theme();
        let (hover_bg, hover_fg) = (theme.sidebar_accent.opacity(0.8), theme.sidebar_accent_foreground);
        let (active_bg, label_fg) = (theme.tokens.sidebar_accent, theme.sidebar_foreground.opacity(0.7));
        let radius = theme.radius;
        let state = AppState::global(cx);
        v_flex()
            .child(h_flex().flex_shrink_0().px_2().h_8().text_xs().text_color(label_fg).child(self.label))
            .child(v_flex().gap_2().children(self.pages.into_iter().map(|page| {
                let state = state.clone();
                let id = page.id;
                h_flex()
                    .id(ElementId::Name(format!("recent-{}", page.id).into()))
                    .test_support()
                    .role(Role::TreeItem)
                    .aria_label(page.title.clone())
                    .aria_selected(page.active)
                    .w_full()
                    .h_7()
                    .p_2()
                    .gap_x_2()
                    .overflow_x_hidden()
                    .rounded(radius)
                    .text_sm()
                    .when(!page.active, |this| this.hover(move |this| this.bg(hover_bg).text_color(hover_fg)))
                    .when(page.active, |this| this.font_medium().bg(active_bg).text_color(hover_fg))
                    .child(PageIcon::new(page.icon.as_deref()))
                    .child(div().flex_1().min_w_0().truncate().child(page.title))
                    .on_click(move |_, _, cx| {
                        state.update(cx, |state, cx| state.open_video(id, VideoTab::Script, cx));
                    })
                    .into_any_element()
            })))
            .into_any_element()
    }
}

/// A neutral count shown at the end of a sidebar row.
fn count_badge(count: usize, cx: &App) -> AnyElement {
    div()
        .px_1p5()
        .rounded_full()
        .text_xs()
        .bg(cx.theme().sidebar_accent)
        .text_color(cx.theme().sidebar_accent_foreground)
        .child(count.to_string())
        .into_any_element()
}

/// The Pipelines row's counts: edits waiting for the creator's approval stand
/// out from the ones still working on their own.
fn pipeline_badges(running: usize, awaiting: usize, cx: &App) -> AnyElement {
    let theme = cx.theme();
    h_flex()
        .gap_1()
        .when(awaiting > 0, |this| {
            let waiting: SharedString = format!("{awaiting} waiting for approval").into();
            this.child(
                div()
                    .id("pipelines-awaiting")
                    .test_support()
                    .aria_label(waiting.clone())
                    .px_1p5()
                    .rounded_full()
                    .text_xs()
                    .font_medium()
                    .bg(theme.warning)
                    .text_color(theme.warning_foreground)
                    .tooltip(move |window, cx| Tooltip::new(waiting.clone()).build(window, cx))
                    .child(awaiting.to_string()),
            )
        })
        .when(running > 0, |this| this.child(count_badge(running, cx)))
        .into_any_element()
}

/// The Pipelines row's name for assistive technology, with its counts.
fn pipelines_label(running: usize, awaiting: usize) -> SharedString {
    let mut parts = vec!["Pipelines".to_string()];
    if running > 0 {
        parts.push(format!("{running} running"));
    }
    if awaiting > 0 {
        parts.push(format!("{awaiting} waiting for approval"));
    }
    parts.join(", ").into()
}

impl Workspace {
    pub(super) fn render_sidebar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let route = state.route().clone();
        let active_view = match &route {
            Route::Database { view } => self.shown_view(*view, cx),
            _ => None,
        };
        let active_video = match &route {
            Route::Video { id, .. } => Some(*id),
            _ => None,
        };
        let active_runs = state.rows().iter().filter(|row| row.latest_run.is_some_and(|run| run.is_active())).count();
        let awaiting_runs =
            state.rows().iter().filter(|row| row.latest_run == Some(RunStatus::AwaitingApproval)).count();
        let active_uploads = state
            .rows()
            .iter()
            .filter(|row| {
                matches!(
                    row.latest_upload,
                    Some(UploadStatus::Pending | UploadStatus::Uploading { .. } | UploadStatus::Processing { .. })
                )
            })
            .count();

        let mut views = state.views().to_vec();
        views.sort_by_key(|view| view.position);
        let view_items = views.into_iter().map(|view| {
            let id = view.id;
            let handle = self.state.clone();
            SidebarMenuItem::new(view.name.clone())
                .icon(Icon::new(match view.kind {
                    ViewKind::Table => IconName::Sheet,
                    ViewKind::Board => IconName::SquareKanban,
                }))
                .active(active_view == Some(id))
                .on_click(move |_, _, cx| {
                    handle.update(cx, |state, cx| state.navigate(Route::Database { view: Some(id) }, cx));
                })
        });

        let recent: Vec<PageLink> = state
            .recent_videos()
            .into_iter()
            .map(|video| PageLink {
                id: video.id,
                icon: video.icon.clone().map(Into::into),
                title: display_title(&video.title).into(),
                active: active_video == Some(video.id),
            })
            .collect();

        let workspace = cx.weak_entity();
        let navigate = |route: Route| {
            let state = self.state.clone();
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                state.update(cx, |state, cx| state.navigate(route.clone(), cx));
            }
        };

        let top = SidebarMenu::new()
            .child(
                SidebarMenuItem::new("Search")
                    .icon(Icon::new(IconName::Search))
                    .suffix(|window, cx| {
                        shortcut_hint(&OpenCommandPalette, window, cx).unwrap_or_else(|| div().into_any_element())
                    })
                    .on_click(move |_, window, cx| {
                        _ = workspace.update(cx, |workspace, cx| workspace.open_command_palette(window, cx));
                    }),
            )
            .child(
                SidebarMenuItem::new("Pipelines")
                    .icon(Icon::new(IconName::WandSparkles))
                    .accessibility_label(pipelines_label(active_runs, awaiting_runs))
                    .active(route == Route::Pipelines)
                    .when(active_runs + awaiting_runs > 0, |this| {
                        this.suffix(move |_, cx| pipeline_badges(active_runs, awaiting_runs, cx))
                    })
                    .on_click(navigate(Route::Pipelines)),
            )
            .child(
                SidebarMenuItem::new("Uploads")
                    .icon(Icon::new(IconName::CloudUpload))
                    .active(route == Route::Uploads)
                    .when(active_uploads > 0, |this| this.suffix(move |_, cx| count_badge(active_uploads, cx)))
                    .on_click(navigate(Route::Uploads)),
            );

        let mut sidebar = Sidebar::new("sidebar")
            .collapsible(SidebarCollapsible::Offcanvas)
            .collapsed(self.sidebar_collapsed)
            .header(self.render_sidebar_header(window, cx))
            .child(Section::Menu(Box::new(top)))
            .child(Section::Group(SidebarGroup::new("Videos").child(SidebarMenu::new().children(view_items))));
        if !recent.is_empty() {
            sidebar =
                sidebar.child(Section::Pages(PageList { label: "Recent".into(), pages: recent, collapsed: false }));
        }
        sidebar.footer(self.render_sidebar_footer(matches!(route, Route::Settings { .. }), window, cx))
    }

    fn render_sidebar_header(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let shortcut = shortcut_hint(&NewVideo, window, cx);
        v_flex()
            .w_full()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .px_1()
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .size_6()
                            .items_center()
                            .justify_center()
                            .rounded(cx.theme().radius)
                            .bg(cx.theme().primary)
                            .text_color(cx.theme().primary_foreground)
                            .child(Icon::new(IconName::Clapperboard).xsmall()),
                    )
                    .child(div().text_sm().font_semibold().child("VideoCRM")),
            )
            .child(
                Button::new("sidebar-new-video")
                    .w_full()
                    .outline()
                    .icon(IconName::Plus)
                    .label("New video…")
                    .when_some(shortcut, |this, hint| this.child(div().flex_1()).child(hint))
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(NewVideo), cx)),
            )
    }

    fn render_sidebar_footer(&self, settings_active: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let state = self.state.clone();
        let settings = SidebarMenu::new().child(
            SidebarMenuItem::new("Settings").icon(Icon::new(IconName::Settings)).active(settings_active).on_click(
                move |_, _, cx| {
                    state.update(cx, |state, cx| state.open_settings(SettingsSection::General, cx));
                },
            ),
        );
        v_flex()
            .w_full()
            .gap_2()
            .child(settings.render("sidebar-footer", window, cx))
            .child(div().pt_2().border_t_1().border_color(cx.theme().sidebar_border).child(self.account.clone()))
            .into_any_element()
    }
}

/// A video title for navigation, with a placeholder for untitled videos.
pub(crate) fn display_title(title: &str) -> String {
    let title = title.trim();
    if title.is_empty() { "Untitled".to_string() } else { title.to_string() }
}
