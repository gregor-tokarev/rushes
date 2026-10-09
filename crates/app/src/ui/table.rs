//! Collection tables whose rows open something (Pipelines, Uploads), drawn
//! with the geometry of the Videos table (the kit's data table): the table
//! head fill, muted labels, row hairlines and hover, medium cell padding.

use std::rc::Rc;

use gpui_kit::component::{ActiveTheme as _, Size, ThemeStyled as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Div, ElementId, InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Rems,
    RenderOnce, Role, SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div,
};

/// One column: its header label and width (`None` takes the remaining room).
#[derive(Clone, Copy, Debug)]
pub struct TableColumn {
    label: &'static str,
    width: Option<Rems>,
    end: bool,
}

impl TableColumn {
    /// A column that takes the room the fixed ones leave, at least 12rem wide.
    pub const fn fill(label: &'static str) -> Self {
        Self { label, width: None, end: false }
    }

    pub const fn fixed(label: &'static str, width: Rems) -> Self {
        Self { label, width: Some(width), end: false }
    }

    /// Aligns the column's content to its trailing edge, for numbers and actions.
    pub const fn end(mut self) -> Self {
        self.end = true;
        self
    }

    /// A cell of this column holding `content`.
    fn cell(self, content: impl IntoElement) -> Div {
        let padding = Size::Medium.table_cell_padding().left;
        div()
            .flex()
            .items_center()
            .min_w_0()
            .px(padding)
            .map(|this| match self.width {
                Some(width) => this.w(width).flex_none(),
                None => this.flex_1().min_w(Rems(12.)),
            })
            .when(self.end, |this| this.justify_end())
            .child(content)
    }
}

/// The header row: the kit's table head fill and muted labels.
pub fn table_header(columns: &[TableColumn], cx: &App) -> Div {
    let theme = cx.theme();
    h_flex()
        .w_full()
        .flex_shrink_0()
        .h(Size::Medium.table_row_height())
        .bg(theme.tokens.table_head)
        .text_sm()
        .text_color(theme.muted_foreground)
        .border_b_1()
        .border_color(theme.table_row_border)
        .children(columns.iter().map(|column| column.cell(div().truncate().child(column.label))))
}

/// A row that opens what it stands for: by click, or with Enter or Space
/// once Tab reaches it, like a button. Buttons and links inside it act on
/// their own.
#[derive(IntoElement)]
pub struct TableRowButton {
    id: ElementId,
    label: SharedString,
    cells: Vec<(TableColumn, AnyElement)>,
    on_open: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
}

impl TableRowButton {
    /// `label` names the row for assistive technology.
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self { id: id.into(), label: label.into(), cells: Vec::new(), on_open: None }
    }

    pub fn cell(mut self, column: TableColumn, content: impl IntoElement) -> Self {
        self.cells.push((column, content.into_any_element()));
        self
    }

    /// What the row opens; a row without it is inert.
    pub fn on_open(mut self, open: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_open = Some(Rc::new(open));
        self
    }
}

impl RenderOnce for TableRowButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let (hover, border) = (theme.tokens.table_hover, theme.table_row_border);
        let row = h_flex()
            .id(self.id.clone())
            .test_support()
            .w_full()
            .min_h(Size::Medium.table_row_height())
            .py_1p5()
            .text_sm()
            .border_b_1()
            .border_color(border)
            .children(self.cells.into_iter().map(|(column, content)| column.cell(content)));
        let Some(open) = self.on_open else { return row.aria_label(self.label) };
        let focus = window.use_keyed_state(self.id, cx, |_, cx| cx.focus_handle().tab_stop(true)).read(cx).clone();
        let focused = focus.is_focused(window);
        let key_open = open.clone();
        row.role(Role::Button)
            .aria_label(self.label)
            .track_focus(&focus)
            .when(focused, |this| this.focus_ring_style(window, cx))
            .hover(move |this| this.bg(hover))
            .on_key_down(move |event: &KeyDownEvent, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    cx.stop_propagation();
                    key_open(window, cx);
                }
            })
            .on_click(move |_, window, cx| open(window, cx))
    }
}
