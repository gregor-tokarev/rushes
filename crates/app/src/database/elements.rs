//! Small presentation elements shared by the table and the board.

use gpui_kit::assets::IconName;
use gpui_kit::component::menu::DropdownMenu;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Icon, Selectable, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Div, ElementId, InteractiveElement, Interactivity, IntoElement,
    ParentElement, RenderOnce, SharedString, Stateful, StatefulInteractiveElement, StyleRefinement,
    Styled, Window, div,
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
        Self { label: label.into(), color, dot: false }
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
            .when(self.dot, |this| this.child(div().size_1p5().rounded_full().bg(colors.dot)))
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
        Self { base: div().id(id), open: false }
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
            .flex()
            .items_center()
            .gap_1()
            .size_full()
            .min_w_0()
            .overflow_hidden()
            .px_2()
            .rounded(cx.theme().radius_tokens().sm)
            .hover(move |this| this.bg(hover))
            .when(self.open, |this| this.bg(cx.theme().secondary_active))
    }
}

/// Muted placeholder for an empty cell value.
pub fn empty_value(cx: &App) -> Div {
    div().text_color(cx.theme().muted_foreground.opacity(0.6)).child("—")
}

/// A small icon in the muted foreground, for labels and metadata.
pub fn muted_icon(icon: IconName, cx: &App) -> Icon {
    Icon::new(icon).xsmall().text_color(cx.theme().muted_foreground)
}
