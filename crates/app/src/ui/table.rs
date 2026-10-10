//! Collection tables whose rows open something (Pipelines, Uploads), drawn
//! with the geometry of the Videos table (the kit's data table): the table
//! head fill, muted labels, row hairlines and hover, medium cell padding.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::component::{ActiveTheme as _, Size, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Div, ElementId, InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton,
    ParentElement as _, Pixels, Rems, RenderOnce, Role, SharedString, StatefulInteractiveElement as _, Styled as _,
    TestSupportExt as _, Window, canvas, div,
};

use super::KeyboardFocus as _;

/// The narrowest a fill column gets: room for a short title.
const FILL_MIN_WIDTH: Rems = Rems(10.);

/// One column: its header label and width (`None` takes the remaining room).
#[derive(Clone, Copy, Debug)]
pub struct TableColumn {
    label: &'static str,
    width: Option<Rems>,
    end: bool,
    optional: bool,
}

impl TableColumn {
    /// A column that takes the room the fixed ones leave, at least
    /// [`FILL_MIN_WIDTH`] wide.
    pub const fn fill(label: &'static str) -> Self {
        Self { label, width: None, end: false, optional: false }
    }

    pub const fn fixed(label: &'static str, width: Rems) -> Self {
        Self { label, width: Some(width), end: false, optional: false }
    }

    /// Aligns the column's content to its trailing edge, for numbers and actions.
    pub const fn end(mut self) -> Self {
        self.end = true;
        self
    }

    /// A secondary column the table drops when it is too narrow for all of
    /// them, so identity, status and actions keep their room.
    pub const fn optional(mut self) -> Self {
        self.optional = true;
        self
    }

    /// The room the column needs, in rems.
    fn needs(self) -> f32 {
        self.width.unwrap_or(FILL_MIN_WIDTH).0
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
                None => this.flex_1().min_w(FILL_MIN_WIDTH),
            })
            .when(self.end, |this| this.justify_end())
            .child(content)
    }
}

/// The columns a table `width` rems wide shows: all of them when they fit
/// (or before the table is measured), otherwise without its optional columns,
/// dropped from the trailing end until the rest fit.
fn visible_columns(columns: &[TableColumn], width: Option<f32>) -> Vec<TableColumn> {
    let mut visible = columns.to_vec();
    let Some(width) = width else { return visible };
    while visible.iter().map(|column| column.needs()).sum::<f32>() > width {
        let Some(ix) = visible.iter().rposition(|column| column.optional) else { break };
        visible.remove(ix);
    }
    visible
}

/// A collection: [`table_header`] over its rows, on the full width of its
/// parent. Too narrow for every column, it drops the optional ones.
#[derive(IntoElement)]
pub struct CollectionTable {
    id: ElementId,
    columns: &'static [TableColumn],
    rows: Vec<TableRowButton>,
}

impl CollectionTable {
    pub fn new(id: impl Into<ElementId>, columns: &'static [TableColumn]) -> Self {
        Self { id: id.into(), columns, rows: Vec::new() }
    }

    pub fn rows(mut self, rows: impl IntoIterator<Item = TableRowButton>) -> Self {
        self.rows.extend(rows);
        self
    }
}

impl RenderOnce for CollectionTable {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        // Measured in the previous frame; a change of the visible columns
        // draws the table once more.
        let width =
            window.use_keyed_state(self.id.clone(), cx, |_, _| Rc::new(Cell::new(None::<Pixels>))).read(cx).clone();
        let rem = window.rem_size();
        let columns = self.columns;
        let shown = move |width: Option<Pixels>| visible_columns(columns, width.map(|width| width / rem)).len();
        let visible = visible_columns(columns, width.get().map(|width| width / rem));
        let measure = canvas(
            move |bounds, window, _| {
                let previous = width.replace(Some(bounds.size.width));
                if previous.is_none() || shown(previous) != shown(Some(bounds.size.width)) {
                    window.request_animation_frame();
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        v_flex()
            .id(self.id)
            .relative()
            .w_full()
            .child(table_header(&visible, cx))
            .children(self.rows.into_iter().map(|row| row.columns(&visible)))
            .child(measure)
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
/// their own: put them in an [`TableRowButton::action_cell`].
#[derive(IntoElement)]
pub struct TableRowButton {
    id: ElementId,
    label: SharedString,
    cells: Vec<(TableColumn, AnyElement)>,
    on_open: Option<OpenRow>,
}

/// What activating a row does.
type OpenRow = Rc<dyn Fn(&mut Window, &mut App)>;

impl TableRowButton {
    /// `label` names the row for assistive technology.
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self { id: id.into(), label: label.into(), cells: Vec::new(), on_open: None }
    }

    pub fn cell(mut self, column: TableColumn, content: impl IntoElement) -> Self {
        self.cells.push((column, content.into_any_element()));
        self
    }

    /// A cell of controls, such as buttons, that act on their own: pressing one
    /// (enabled or not) doesn't also open the row.
    pub fn action_cell(self, column: TableColumn, content: impl IntoElement) -> Self {
        let controls = div().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(content);
        self.cell(column, controls)
    }

    /// Keeps the cells of `visible` columns only.
    fn columns(mut self, visible: &[TableColumn]) -> Self {
        self.cells.retain(|(column, _)| visible.iter().any(|shown| shown.label == column.label));
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
        let (key_open, key_focus) = (open.clone(), focus.clone());
        row.role(Role::Button)
            .aria_label(self.label)
            .keyboard_focus(&focus, window, cx)
            .hover(move |this| this.bg(hover))
            .on_key_down(move |event: &KeyDownEvent, window, cx| {
                // Keys pressed on a focused button inside the row are the button's.
                if key_focus.is_focused(window) && matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    cx.stop_propagation();
                    key_open(window, cx);
                }
            })
            .on_click(move |_, window, cx| open(window, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COLUMNS: [TableColumn; 4] = [
        TableColumn::fill("Video"),
        TableColumn::fixed("Status", Rems(10.)),
        TableColumn::fixed("Started", Rems(9.)).optional(),
        TableColumn::fixed("Updated", Rems(9.)).optional(),
    ];

    fn labels(columns: Vec<TableColumn>) -> Vec<&'static str> {
        columns.into_iter().map(|column| column.label).collect()
    }

    #[test]
    fn narrow_tables_drop_optional_columns_from_the_end() {
        assert_eq!(labels(visible_columns(&COLUMNS, None)), ["Video", "Status", "Started", "Updated"]);
        assert_eq!(labels(visible_columns(&COLUMNS, Some(40.))), ["Video", "Status", "Started", "Updated"]);
        assert_eq!(labels(visible_columns(&COLUMNS, Some(35.))), ["Video", "Status", "Started"]);
        assert_eq!(labels(visible_columns(&COLUMNS, Some(25.))), ["Video", "Status"]);
        // Required columns stay even when they do not fit.
        assert_eq!(labels(visible_columns(&COLUMNS, Some(10.))), ["Video", "Status"]);
    }
}
