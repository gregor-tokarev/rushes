//! Small presentation pieces the shell's pages share.

use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::empty::{Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyMediaVariant, EmptyTitle};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, ThemeStyled as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Action, AnyElement, App, ClickEvent, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, Rems,
    RenderOnce, Role, SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div,
};

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// An empty state: icon, title and one sentence on what to do next.
pub(crate) fn empty_state(
    icon: IconName,
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
) -> Empty {
    Empty::new().header(
        EmptyHeader::new()
            .media(EmptyMedia::new().with_variant(EmptyMediaVariant::Icon).child(Icon::new(icon)))
            .title(EmptyTitle::new().child(title.into()))
            .description(EmptyDescription::new().child(description.into())),
    )
}

/// A small muted heading over a group of rows, e.g. in an inspector rail.
pub(crate) fn section_label(label: impl Into<SharedString>, cx: &App) -> impl IntoElement {
    div().text_xs().font_medium().text_color(cx.theme().muted_foreground).child(label.into())
}

/// A full-width, leading-aligned command row: an optional icon and a label
/// that truncates. Focusable and activated by Enter or Space like a button.
#[derive(IntoElement)]
pub(crate) struct RowButton {
    id: ElementId,
    icon: Option<IconName>,
    label: SharedString,
    indent: Rems,
    muted: bool,
    on_click: Option<ClickHandler>,
}

impl RowButton {
    pub(crate) fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self { id: id.into(), icon: None, label: label.into(), indent: Rems(0.), muted: false, on_click: None }
    }

    pub(crate) fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Leading inset, for nested rows such as lower outline levels.
    pub(crate) fn indent(mut self, indent: Rems) -> Self {
        self.indent = indent;
        self
    }

    /// Draws the label in the muted foreground, for secondary rows.
    pub(crate) fn muted(mut self, muted: bool) -> Self {
        self.muted = muted;
        self
    }

    pub(crate) fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for RowButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let focus =
            window.use_keyed_state(self.id.clone(), cx, |_, cx| cx.focus_handle().tab_stop(true)).read(cx).clone();
        let focused = focus.is_focused(window);
        let hover = cx.theme().secondary_hover;
        let active = cx.theme().secondary_active;
        h_flex()
            .id(self.id)
            .test_support()
            .role(Role::Button)
            .aria_label(self.label.clone())
            .track_focus(&focus)
            .w_full()
            .min_w_0()
            .h_7()
            .px_2()
            .gap_2()
            .rounded(cx.theme().radius)
            .text_sm()
            .when(self.muted, |this| this.text_color(cx.theme().muted_foreground))
            .hover(move |this| this.bg(hover))
            .active(move |this| this.bg(active))
            .when(focused, |this| this.focus_ring_style(window, cx))
            .child(div().flex_none().w(self.indent))
            .when_some(self.icon, |this, icon| {
                this.child(Icon::new(icon).small().text_color(cx.theme().muted_foreground))
            })
            .child(div().flex_1().min_w_0().truncate().child(self.label))
            .when_some(self.on_click, |this, on_click| {
                this.on_click(move |event, window, cx| on_click(event, window, cx))
            })
    }
}

/// A selectable card in a small set of choices: leading visual, title and a
/// sentence of description. Behaves as a radio button.
#[derive(IntoElement)]
pub(crate) struct ChoiceCard {
    id: ElementId,
    leading: AnyElement,
    title: SharedString,
    description: SharedString,
    selected: bool,
    on_click: Option<ClickHandler>,
}

impl ChoiceCard {
    pub(crate) fn new(
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

    pub(crate) fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub(crate) fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for ChoiceCard {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let focus =
            window.use_keyed_state(self.id.clone(), cx, |_, cx| cx.focus_handle().tab_stop(true)).read(cx).clone();
        let focused = focus.is_focused(window);
        let theme = cx.theme();
        let (hover, border, selected_border, selected_bg) = (theme.muted, theme.border, theme.primary, theme.accent);
        v_flex()
            .id(self.id)
            .test_support()
            .role(Role::RadioButton)
            .aria_label(self.title.clone())
            .aria_selected(self.selected)
            .track_focus(&focus)
            .gap_1()
            .p_3()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(if self.selected { selected_border } else { border })
            .when(self.selected, |this| this.bg(selected_bg))
            .when(!self.selected, |this| this.hover(move |this| this.bg(hover)))
            .when(focused, |this| this.focus_ring_style(window, cx))
            .child(h_flex().gap_2().child(self.leading).child(div().text_sm().font_medium().child(self.title)))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(self.description))
            .when_some(self.on_click, |this, on_click| {
                this.on_click(move |event, window, cx| on_click(event, window, cx))
            })
    }
}

/// The key binding of `action` as muted text, for controls that show their shortcut.
pub(crate) fn shortcut_hint(action: &dyn Action, window: &Window, cx: &App) -> Option<AnyElement> {
    let kbd = Kbd::binding_for_action(action, None, window)?;
    Some(div().text_xs().text_color(cx.theme().muted_foreground).child(kbd.appearance(false)).into_any_element())
}
