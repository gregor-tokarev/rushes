//! A full-width command row, and the heading over a list of them.

use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ClickEvent, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, Rems, RenderOnce, Role,
    SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div,
};

use super::KeyboardFocus as _;

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// A small muted heading over a group of rows, e.g. in an inspector rail.
pub fn section_label(label: impl Into<SharedString>, cx: &App) -> impl IntoElement {
    div().text_xs().font_medium().text_color(cx.theme().muted_foreground).child(label.into())
}

/// A full-width, leading-aligned command row: an optional icon and a label
/// that truncates. Focusable and activated by Enter or Space like a button.
/// Its hover surface pads the content by `px_2`; a list of rows bleeds out by
/// as much (`mx_neg_2`) so its text lines up with the heading above it.
#[derive(IntoElement)]
pub struct RowButton {
    id: ElementId,
    icon: Option<IconName>,
    label: SharedString,
    indent: Rems,
    muted: bool,
    on_click: Option<ClickHandler>,
}

impl RowButton {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self { id: id.into(), icon: None, label: label.into(), indent: Rems(0.), muted: false, on_click: None }
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Leading inset, for nested rows such as lower outline levels.
    pub fn indent(mut self, indent: Rems) -> Self {
        self.indent = indent;
        self
    }

    /// Draws the label in the muted foreground, for secondary rows.
    pub fn muted(mut self, muted: bool) -> Self {
        self.muted = muted;
        self
    }

    pub fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for RowButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let focus =
            window.use_keyed_state(self.id.clone(), cx, |_, cx| cx.focus_handle().tab_stop(true)).read(cx).clone();
        let hover = cx.theme().secondary_hover;
        let active = cx.theme().secondary_active;
        h_flex()
            .id(self.id)
            .test_support()
            .role(Role::Button)
            .aria_label(self.label.clone())
            .keyboard_focus(&focus, window, cx)
            .w_full()
            .min_w_0()
            .h_7()
            .px_2()
            .when(self.indent.0 > 0., |this| this.pl(Rems(0.5 + self.indent.0)))
            .gap_2()
            .rounded(cx.theme().radius)
            .text_sm()
            .when(self.muted, |this| this.text_color(cx.theme().muted_foreground))
            .hover(move |this| this.bg(hover))
            .active(move |this| this.bg(active))
            .when_some(self.icon, |this, icon| {
                this.child(Icon::new(icon).small().text_color(cx.theme().muted_foreground))
            })
            .child(div().flex_1().min_w_0().truncate().child(self.label))
            .when_some(self.on_click, |this, on_click| {
                this.on_click(move |event, window, cx| on_click(event, window, cx))
            })
    }
}
