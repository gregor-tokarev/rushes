//! Small presentation elements shared by the table and the board.

use gpui_kit::assets::IconName;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::menu::{DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon, Selectable, Side, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Div, ElementId, InteractiveElement, Interactivity, IntoElement, ParentElement, RenderOnce, Role,
    SharedString, Stateful, StatefulInteractiveElement, StyleRefinement, Styled, TestSupportExt as _, Window, div,
    rems,
};
use videocrm_core::{SelectOption, TagColor};

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
    v_flex().size_full().min_w_0().justify_center().child(content)
}

/// Read-only cell content, such as script stats or pipeline chips.
///
/// Inset like the content of a [`CellButton`] (its `px_2` less the
/// `mx_neg_1` every cell trigger gets), so read-only values line up with
/// editable ones and with the column header.
pub fn static_cell(content: impl IntoElement) -> Div {
    h_flex().min_w_0().px_1().child(content)
}

/// One line of chips in a table cell. Chips that do not fit wrap onto lines
/// the cell clips, so they are hidden whole instead of cut mid-word; a lone
/// chip wider than the cell ends in an ellipsis. The cell's popover lists
/// every value.
pub fn chip_line(chips: impl IntoIterator<Item = AnyElement>) -> Div {
    h_flex()
        .flex_1()
        .min_w_0()
        .h(rems(1.25))
        .flex_wrap()
        .gap_1()
        .overflow_hidden()
        .children(chips.into_iter().map(|chip| div().min_w_0().child(chip)))
}

/// A muted icon whose tooltip explains a newer attempt the cell does not show,
/// such as a failed upload of a video that is already on YouTube.
pub fn attempt_hint(id: ElementId, icon: IconName, tooltip: SharedString, cx: &App) -> impl IntoElement {
    div()
        .id(id)
        .test_support()
        .flex_shrink_0()
        .role(Role::Image)
        .aria_label(tooltip.clone())
        .child(muted_icon(icon, cx))
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
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
    div().text_color(cx.theme().muted_foreground.opacity(0.6)).child("—")
}

/// A small icon in the muted foreground, for labels and metadata.
pub fn muted_icon(icon: IconName, cx: &App) -> Icon {
    Icon::new(icon).xsmall().text_color(cx.theme().muted_foreground)
}

/// Where a select without options sends the creator, from a table cell.
pub(crate) const NO_OPTIONS_IN_TABLE: &str = "No options yet. Add them with Edit options… in the column’s menu.";
/// Where a select without options sends the creator, from the video page.
pub(crate) const NO_OPTIONS_ON_PAGE: &str =
    "No options yet. Add them with Edit options… in the property’s column menu in the Videos table.";

/// The menu of a select value, the same in table cells and on the video page:
/// one item per option, the current one ticked, and Clear once a value is set.
/// Without options it says `no_options`.
pub(crate) fn select_menu(
    menu: PopupMenu,
    options: &[SelectOption],
    current: Option<&str>,
    no_options: &'static str,
    set: impl Fn(Option<String>, &mut App) + Clone + 'static,
) -> PopupMenu {
    if options.is_empty() {
        return menu.label(no_options);
    }
    let menu = options.iter().fold(menu.check_side(Side::Right), |menu, option| {
        let (name, color, id) = (option.name.clone(), option.color, option.id.clone());
        let set = set.clone();
        menu.item(
            PopupMenuItem::element(move |_, _| Chip::new(name.clone(), color))
                .checked(current == Some(option.id.as_str()))
                .on_click(move |_, _, cx| set(Some(id.clone()), cx)),
        )
    });
    match current {
        Some(_) => {
            menu.separator().item(PopupMenuItem::new("Clear").icon(IconName::X).on_click(move |_, _, cx| set(None, cx)))
        }
        None => menu,
    }
}

/// The checklist of a multi-select value, the same in table cells and on the
/// video page: a popover that stays open while options are ticked. Without
/// options it says `no_options`.
pub(crate) fn multi_select_list(
    options: &[SelectOption],
    selected: &[String],
    no_options: &'static str,
    toggle: impl Fn(&str, &mut App) + Clone + 'static,
    cx: &App,
) -> Div {
    v_flex()
        .gap_1()
        .min_w_48()
        .children(options.iter().map(|option| {
            let (toggle, option_id) = (toggle.clone(), option.id.clone());
            // The chip is the checkbox's label, so the whole option row toggles it.
            Checkbox::new(SharedString::from(format!("option-{}", option.id)))
                .accessibility_label(option.name.clone())
                .checked(selected.contains(&option.id))
                .on_click(move |_, _, cx| toggle(&option_id, cx))
                .child(h_flex().child(Chip::new(option.name.clone(), option.color)))
        }))
        .when(options.is_empty(), |this| {
            this.child(div().text_sm().text_color(cx.theme().muted_foreground).child(no_options))
        })
}
