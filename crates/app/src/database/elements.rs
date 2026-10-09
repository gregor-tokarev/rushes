//! Small presentation elements shared by the table and the board.

use gpui_kit::assets::IconName;
use gpui_kit::component::menu::DropdownMenu;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Icon, Selectable, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Div, ElementId, InteractiveElement, Interactivity, IntoElement, ParentElement,
    RenderOnce, Role, SharedString, Stateful, StatefulInteractiveElement, StyleRefinement, Styled,
    TestSupportExt as _, Window, div,
};
use videocrm_core::TagColor;

use super::colors::tag_colors;

/// A colored, Notion-style value chip: select options, statuses and tags.
#[derive(IntoElement)]
pub struct Chip {
    label: SharedString,
    color: TagColor,
    dot: bool,
}

impl Chip {
    pub fn new(label: impl Into<SharedString>, color: TagColor) -> Self {
        Self {
            label: label.into(),
            color,
            dot: false,
        }
    }

    /// Lead the label with a dot in the chip's color, the way workflow
    /// states (status, pipeline, upload) are told apart from plain options.
    pub fn dot(mut self, dot: bool) -> Self {
        self.dot = dot;
        self
    }
}

impl RenderOnce for Chip {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = tag_colors(self.color, cx);
        Tag::custom(colors.background, colors.foreground, colors.background)
            .rounded(cx.theme().radius_tokens().sm)
            .px_1p5()
            .py_0()
            .gap_1()
            .flex_shrink_0()
            .min_w_0()
            .when(self.dot, |this| {
                this.child(div().size_1p5().rounded_full().bg(colors.dot))
            })
            .child(div().min_w_0().truncate().child(self.label))
    }
}

/// A full-cell trigger for value pickers: left-aligned content, quiet hover
/// and a pressed look while its popup is open.
///
/// Implements [`DropdownMenu`] so status, format and select cells open a
/// standard [`gpui_kit::component::menu::PopupMenu`].
#[derive(IntoElement)]
pub struct CellButton {
    base: Stateful<Div>,
    open: bool,
}

impl CellButton {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            base: div().id(id),
            open: false,
        }
    }
}

impl Styled for CellButton {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl InteractiveElement for CellButton {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.base.interactivity()
    }
}

impl StatefulInteractiveElement for CellButton {}

impl ParentElement for CellButton {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.base.extend(elements);
    }
}

impl Selectable for CellButton {
    fn selected(mut self, selected: bool) -> Self {
        self.open = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.open
    }
}

impl DropdownMenu for CellButton {}

impl RenderOnce for CellButton {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let hover = cx.theme().secondary_hover;
        self.base
            .role(Role::Button)
            .test_support()
            .flex()
            .items_center()
            .gap_1()
            .w_full()
            .min_h_6()
            .min_w_0()
            .overflow_hidden()
            .px_2()
            .rounded(cx.theme().radius_tokens().sm)
            .hover(move |this| this.bg(hover))
            .when(self.open, |this| this.bg(cx.theme().secondary_active))
    }
}

/// The slot every table cell and header renders into: full width, content
/// vertically centered.
///
/// A flex column, so a popup trigger's container stretches across the cell
/// and the [`CellButton`] inside it fills the cell. Content that should keep
/// its natural width, such as a lone [`Chip`], goes in an `h_flex` first.
pub fn cell_slot(content: impl IntoElement) -> Div {
    v_flex()
        .size_full()
        .min_w_0()
        .justify_center()
        .child(content)
}

/// Read-only cell content, such as script stats or pipeline chips.
///
/// Inset like the content of a [`CellButton`] (its `px_2` less the
/// `mx_neg_1` every cell trigger gets), so read-only values line up with
/// editable ones and with the column header.
pub fn static_cell(content: impl IntoElement) -> Div {
    h_flex().min_w_0().px_1().child(content)
}

/// A small count on a toolbar trigger, e.g. the number of active filters.
pub fn count_badge(count: usize, cx: &App) -> Div {
    h_flex()
        .h_4()
        .min_w_4()
        .px_1()
        .justify_center()
        .rounded_full()
        .text_xs()
        .bg(cx.theme().primary)
        .text_color(cx.theme().primary_foreground)
        .child(count.to_string())
}

/// Muted placeholder for an empty cell value.
pub fn empty_value(cx: &App) -> Div {
    div()
        .text_color(cx.theme().muted_foreground.opacity(0.6))
        .child("—")
}

/// A small icon in the muted foreground, for labels and metadata.
pub fn muted_icon(icon: IconName, cx: &App) -> Icon {
    Icon::new(icon)
        .xsmall()
        .text_color(cx.theme().muted_foreground)
}
