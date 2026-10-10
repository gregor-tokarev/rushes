//! A selectable card in a small set of choices.

use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce, Role,
    SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div,
};

use super::KeyboardFocus as _;

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// A selectable card in a small set of choices: leading visual, title and a
/// sentence of description. Behaves as a radio button.
///
/// The selected card carries the primary border, the accent fill and a check
/// on its trailing edge; hover only darkens a card's border, so it never looks
/// selected.
#[derive(IntoElement)]
pub struct ChoiceCard {
    id: ElementId,
    leading: AnyElement,
    title: SharedString,
    description: SharedString,
    selected: bool,
    on_click: Option<ClickHandler>,
}

impl ChoiceCard {
    pub fn new(
        id: impl Into<ElementId>,
        leading: impl IntoElement,
        title: impl Into<SharedString>,
        description: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            leading: leading.into_any_element(),
            title: title.into(),
            description: description.into(),
            selected: false,
            on_click: None,
        }
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for ChoiceCard {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let focus =
            window.use_keyed_state(self.id.clone(), cx, |_, cx| cx.focus_handle().tab_stop(true)).read(cx).clone();
        let theme = cx.theme();
        let hover_border = theme.muted_foreground.opacity(0.5);
        let check = Icon::new(IconName::CircleCheck).small().text_color(theme.primary);
        v_flex()
            .id(self.id)
            .test_support()
            .role(Role::RadioButton)
            .aria_label(self.title.clone())
            .aria_selected(self.selected)
            .keyboard_focus(&focus, window, cx)
            .gap_1()
            .p_3()
            .rounded(theme.radius_lg)
            .border_1()
            .map(|this| {
                if self.selected {
                    this.border_color(theme.primary).bg(theme.accent)
                } else {
                    this.border_color(theme.border).hover(move |this| this.border_color(hover_border))
                }
            })
            .child(
                h_flex()
                    .gap_2()
                    .child(self.leading)
                    .child(div().flex_1().min_w_0().text_sm().font_medium().child(self.title))
                    // The slot stays when unselected, so titles don't shift.
                    .child(div().flex_none().size_4().when(self.selected, |this| this.child(check))),
            )
            .child(div().text_xs().text_color(theme.muted_foreground).child(self.description))
            .when_some(self.on_click, |this, on_click| {
                this.on_click(move |event, window, cx| on_click(event, window, cx))
            })
    }
}
