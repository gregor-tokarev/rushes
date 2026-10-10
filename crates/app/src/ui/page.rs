//! The frame of a top-level page: Videos, Pipelines and Uploads.

use gpui_kit::component::{ActiveTheme as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Div, IntoElement, ParentElement as _, Rems, RenderOnce, SharedString, Styled as _, Window, div,
};

/// Leading and trailing inset of a top-level page: its title, toolbar, table
/// and empty state all start on this spine.
pub const PAGE_INSET: Rems = Rems(2.5);

/// A page's title, a one-line subtitle under it (the page's count or
/// summary) and actions on the title's trailing side.
#[derive(IntoElement)]
pub struct PageHeader {
    title: SharedString,
    subtitle: Option<SharedString>,
    actions: Vec<AnyElement>,
}

impl PageHeader {
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self { title: title.into(), subtitle: None, actions: Vec::new() }
    }

    pub fn subtitle(mut self, subtitle: impl Into<SharedString>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.actions.push(action.into_any_element());
        self
    }
}

impl RenderOnce for PageHeader {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        v_flex()
            .flex_shrink_0()
            .px(PAGE_INSET)
            .pt_8()
            .pb_4()
            .gap_1()
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().min_w_0().truncate().text_2xl().font_semibold().child(self.title))
                    .children(self.actions),
            )
            .when_some(self.subtitle, |this, subtitle| {
                this.child(div().text_sm().text_color(cx.theme().muted_foreground).truncate().child(subtitle))
            })
    }
}

/// A page column: the header on top, `content` filling the rest. The content
/// owns its scrolling and puts its own [`PAGE_INSET`] inside the scroll area,
/// so the scrollbar stays on the window's edge.
pub fn page_frame(header: PageHeader, content: impl IntoElement) -> Div {
    v_flex().size_full().min_h_0().child(header).child(div().flex_1().min_h_0().flex().flex_col().child(content))
}
