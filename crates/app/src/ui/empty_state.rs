//! The one empty state.

use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, StyledExt as _, v_flex};
use gpui_kit::{
    AnyElement, App, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce, SharedString,
    Styled as _, TestSupportExt as _, Window, div,
};

/// What an empty region would hold and how to fill it: a muted icon, a title,
/// one sentence and the next action, centred in the region.
///
/// Actions are small outline buttons, so the empty state never competes with
/// the page's own primary action.
#[derive(IntoElement)]
pub struct EmptyState {
    id: ElementId,
    icon: IconName,
    title: SharedString,
    description: SharedString,
    action: Option<AnyElement>,
}

impl EmptyState {
    pub fn new(
        id: impl Into<ElementId>,
        icon: IconName,
        title: impl Into<SharedString>,
        description: impl Into<SharedString>,
    ) -> Self {
        Self { id: id.into(), icon, title: title.into(), description: description.into(), action: None }
    }

    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.action = Some(action.into_any_element());
        self
    }
}

impl RenderOnce for EmptyState {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        v_flex()
            .id(self.id)
            .test_support()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .px_6()
            .py_8()
            .child(Icon::new(self.icon).size_8().text_color(muted.opacity(0.7)))
            .child(div().text_base().font_semibold().text_center().child(self.title))
            .child(div().max_w_80().text_sm().text_center().text_color(muted).child(self.description))
            .children(self.action.map(|action| div().pt_2().child(action)))
    }
}
