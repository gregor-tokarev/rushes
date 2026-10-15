//! [`DatabasePanel`]: the videos database with its views, toolbar and editors.
//!
//! The panel owns an in-memory copy of the database. Edits are applied to that
//! copy immediately and reported as [`DatabaseEvent`]s; the owner persists them
//! and hands the stored state back through [`DatabasePanel::set_data`].

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use chrono::{NaiveDate, Utc};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::calendar::{CalendarEvent, CalendarState, Date};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::table::{DataTable, TableEvent, TableState};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, Size, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, Global, InteractiveElement as _,
    IntoElement, KeyBinding, NoAction, ParentElement as _, Pixels, Render, ScrollHandle, SharedString, Styled as _,
    Subscription, Task, Window, actions, div,
};
use rushes_core::input::{normalize_title, parse_number};
use rushes_core::settings::{WeekStart, Zoom};
use rushes_core::{
    BuiltinField, Column, DatabaseView, FilterSpec, PropertyDef, PropertyId, PropertyKind, PropertyValue,
    SortDirection, SortSpec, Video, VideoId, VideoRow, VideoStatus, ViewId, ViewKind,
};

use super::format;
use super::property_editor::{PropertyEditor, PropertyEditorMode};
use super::query::{self, resolve_columns, view_rows, visible_columns};
use super::table::{CellEditors, EmptyReason, TableModel, VideoTable};
use crate::publish::{YoutubeAccount, YoutubeEvent, YoutubeService};
use crate::ui::PAGE_INSET;
use crate::ui::dialog::confirm_delete_videos;

actions!(video_database, [OpenSelected, RenameSelected, DeleteSelected, CancelEdit]);

const KEY_CONTEXT: &str = "VideoDatabase";

/// Key context of a board card, which opens on Enter like a table row.
pub(crate) const BOARD_CARD_CONTEXT: &str = "BoardCard";

/// How long a click on a title waits before opening the video, so the second
/// click of a double-click can rename it instead. Just under GPUI's
/// double-click interval (400 ms) counted from the first click's release.
pub(crate) const OPEN_AFTER_CLICK: Duration = Duration::from_millis(350);

/// Key context around the inline cell editor, so Escape cancels the edit
/// before the table takes it to clear the row selection.
pub(crate) const CELL_EDITOR_CONTEXT: &str = "VideoDatabaseCellEditor";

/// Key context around a text input that lives inside a popover, a dialog or
/// the table: Enter and Space belong to the input there, so they must not
/// also confirm the surrounding popover or dialog, or open the selected row.
pub(crate) const TEXT_ENTRY_CONTEXT: &str = "VideoDatabaseTextEntry";

/// What the owner of a [`DatabasePanel`] must act on. The panel has already
/// applied the change to its own copy; the owner persists it.
// Events are emitted one at a time on user actions, so the size of the
// `Video` variant does not matter.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq)]
pub enum DatabaseEvent {
    /// Show the video's page.
    OpenVideo(VideoId),
    /// Show the video's page with the caret at the start of its script: the
    /// creator opened it from the keyboard, with Enter on a row or a card.
    OpenScript(VideoId),
    /// Create a video, in the given status when created from a board column.
    /// The panel shows it once the owner hands it back through `set_data`.
    CreateVideo {
        status: Option<VideoStatus>,
    },
    /// The whole video after an edit, with `updated_at` stamped.
    UpdateVideo(Video),
    /// Already confirmed by the user.
    DeleteVideos(Vec<VideoId>),
    /// The owner makes the copy and hands it back through `set_data`.
    DuplicateVideo(VideoId),
    CreateProperty(PropertyDef),
    UpdateProperty(PropertyDef),
    /// Already confirmed; the panel has dropped the property's values.
    DeleteProperty(PropertyId),
    /// Columns, widths, sorts, filters or the name of a view changed.
    SaveView(DatabaseView),
    CreateView(DatabaseView),
    DeleteView(ViewId),
    /// The user switched views; not sent for `set_active_view`.
    ActiveViewChanged(ViewId),
}

/// The sort rule the Sort popover is choosing a column for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SortPicker {
    /// The existing rule at this index.
    Rule(usize),
    /// A rule to add.
    New,
}

/// One cell of the database: a video and a column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct CellRef {
    pub video: VideoId,
    pub column: Column,
}

/// The Notion-like videos database: view tabs, toolbar, and a table or board.
pub struct DatabasePanel {
    rows: Vec<VideoRow>,
    properties: Vec<PropertyDef>,
    /// Ordered by `position`.
    views: Vec<DatabaseView>,
    active_view: Option<ViewId>,
    /// Indices into `rows` the active view shows, filtered and sorted.
    shown: Vec<usize>,
    selected: Option<VideoId>,
    editing: Option<CellRef>,
    date_target: Option<CellRef>,
    tags_target: Option<VideoId>,
    /// The day the date cells' calendar starts its weeks on.
    week_start: WeekStart,
    /// The interface zoom: the table's rows and columns are sized in pixels.
    zoom: Zoom,
    /// The view whose tab was right-clicked, for the tab context menu.
    pub(super) menu_view: Option<ViewId>,
    /// The sort rule whose column the Sort popover is choosing, if any.
    pub(super) sort_picker: Option<SortPicker>,
    /// Whether the view tabs overflowed their strip in the last frame, so the
    /// strip offers a menu of every view.
    pub(super) view_tabs_overflow: Rc<Cell<bool>>,
    pub(super) view_tabs_scroll: ScrollHandle,
    /// A title click waiting out the double-click interval before opening.
    pending_open: Option<Task<()>>,
    pub(super) table: Entity<TableState<VideoTable>>,
    pub(super) search: Entity<InputState>,
    pub(super) view_name: Entity<InputState>,
    pub(super) board_scroll: ScrollHandle,
    /// A card was dropped and the board hasn't been drawn since the mouse moved.
    pub(super) card_dropped: bool,
    editors: CellEditors,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DatabaseEvent> for DatabasePanel {}

impl Focusable for DatabasePanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.table.focus_handle(cx)
    }
}

struct Keymap;

impl Global for Keymap {}

/// Binds the panel's keys once per application.
fn bind_keys(cx: &mut App) {
    if cx.has_global::<Keymap>() {
        return;
    }
    let table = Some("VideoDatabase > DataTable");
    let text_entry = Some(TEXT_ENTRY_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("enter", OpenSelected, table),
        KeyBinding::new("enter", OpenSelected, Some("VideoDatabase > BoardCard")),
        KeyBinding::new("f2", RenameSelected, table),
        KeyBinding::new("backspace", DeleteSelected, table),
        KeyBinding::new("delete", DeleteSelected, table),
        KeyBinding::new("escape", CancelEdit, Some(CELL_EDITOR_CONTEXT)),
        // The input still sees these keys; `NoAction` only keeps the
        // shallower Popover, Dialog and table bindings from firing too.
        KeyBinding::new("enter", NoAction, text_entry),
        KeyBinding::new("space", NoAction, text_entry),
    ]);
    cx.set_global(Keymap);
}

impl DatabasePanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        bind_keys(cx);

        let editors = CellEditors {
            cell_input: cx.new(|cx| InputState::new(window, cx)),
            tag_input: cx.new(|cx| InputState::new(window, cx).placeholder("Add a tag…")),
            calendar: cx.new(|cx| CalendarState::new(window, cx)),
        };
        let table = {
            let delegate = VideoTable::new(cx.weak_entity(), editors.clone());
            cx.new(|cx| {
                TableState::new(delegate, window, cx)
                    .col_movable(false)
                    .col_selectable(false)
                    .sortable(false)
                    .loop_selection(false)
            })
        };
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let view_name = cx.new(|cx| InputState::new(window, cx));

        let mut subscriptions = vec![
            cx.subscribe_in(&table, window, Self::on_table_event),
            cx.subscribe_in(&search, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.refresh(cx);
                }
            }),
            cx.subscribe_in(&editors.cell_input, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    this.commit_editing(window, cx);
                }
            }),
            cx.subscribe_in(&editors.tag_input, window, Self::on_tag_input_event),
            cx.subscribe_in(&editors.calendar, window, |this, _, event: &CalendarEvent, window, cx| {
                let CalendarEvent::Selected(Date::Single(Some(date))) = event else {
                    return;
                };
                if let Some(cell) = this.date_target {
                    this.set_date(cell, Some(*date), window, cx);
                }
            }),
        ];
        // The Channel column names accounts: it follows sign-ins, sign-outs and the default.
        if let Some(service) = YoutubeService::try_global(cx) {
            subscriptions.push(cx.subscribe(&service, |this, _, event: &YoutubeEvent, cx| {
                if *event == YoutubeEvent::ConnectionChanged {
                    this.table.update(cx, |_, cx| cx.notify());
                }
            }));
        }

        Self {
            rows: Vec::new(),
            properties: Vec::new(),
            views: Vec::new(),
            active_view: None,
            shown: Vec::new(),
            selected: None,
            editing: None,
            date_target: None,
            tags_target: None,
            week_start: WeekStart::default(),
            zoom: Zoom::DEFAULT,
            menu_view: None,
            sort_picker: None,
            view_tabs_overflow: Rc::default(),
            view_tabs_scroll: ScrollHandle::new(),
            pending_open: None,
            table,
            search,
            view_name,
            board_scroll: ScrollHandle::new(),
            card_dropped: false,
            editors,
            _subscriptions: subscriptions,
        }
    }

    /// Replaces the panel's copy of the database.
    ///
    /// Keeps the active view, the selected video and scroll positions when they
    /// still exist; otherwise falls back to the first view.
    pub fn set_data(
        &mut self,
        rows: Vec<VideoRow>,
        properties: Vec<PropertyDef>,
        mut views: Vec<DatabaseView>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        views.sort_by_key(|view| view.position);
        self.rows = rows;
        self.properties = properties;
        self.properties.sort_by_key(|property| property.position);
        self.views = views;
        if !self.active_view.is_some_and(|id| self.views.iter().any(|view| view.id == id)) {
            self.active_view = self.views.first().map(|view| view.id);
        }
        if self.editing.is_some_and(|cell| !self.rows.iter().any(|row| row.video.id == cell.video)) {
            self.editing = None;
        }
        self.date_target = self.date_target.filter(|cell| self.has_video(cell.video));
        self.tags_target = self.tags_target.filter(|id| self.has_video(*id));
        self.refresh(cx);
    }

    pub fn set_week_start(&mut self, week_start: WeekStart, cx: &mut Context<Self>) {
        if self.week_start != week_start {
            self.week_start = week_start;
            self.refresh(cx);
        }
    }

    /// Sizes the table's rows and columns for `zoom`. Views keep their column
    /// widths at 100%, so every zoom shows the same proportions.
    pub fn set_zoom(&mut self, zoom: Zoom, cx: &mut Context<Self>) {
        if self.zoom != zoom {
            self.zoom = zoom;
            self.refresh(cx);
        }
    }

    /// Shows `view` without reporting it back as [`DatabaseEvent::ActiveViewChanged`].
    pub fn set_active_view(&mut self, view: ViewId, window: &mut Window, cx: &mut Context<Self>) {
        if self.views.iter().any(|known| known.id == view) && self.active_view != Some(view) {
            self.active_view = Some(view);
            self.sort_picker = None;
            // A view picked from the overflow menu, or just added, may sit
            // outside the scrolled tab strip.
            if let Some(ix) = self.active_index() {
                self.view_tabs_scroll.scroll_to_item(ix);
            }
            self.save_editing(window, cx);
            self.refresh(cx);
        }
    }

    pub fn active_view(&self) -> Option<&DatabaseView> {
        let id = self.active_view?;
        self.views.iter().find(|view| view.id == id)
    }

    pub fn views(&self) -> &[DatabaseView] {
        &self.views
    }

    pub fn properties(&self) -> &[PropertyDef] {
        &self.properties
    }

    /// The videos the active view shows, in display order.
    pub fn visible_videos(&self) -> Vec<VideoId> {
        self.shown.iter().map(|&ix| self.rows[ix].video.id).collect()
    }

    pub fn video(&self, id: VideoId) -> Option<&Video> {
        self.rows.iter().map(|row| &row.video).find(|video| video.id == id)
    }

    pub fn selected_video(&self) -> Option<VideoId> {
        self.selected
    }

    /// Applies `edit` to a video, stamps it as updated and reports it.
    pub fn update_video(&mut self, id: VideoId, edit: impl FnOnce(&mut Video), cx: &mut Context<Self>) {
        let Some(row) = self.rows.iter_mut().find(|row| row.video.id == id) else {
            return;
        };
        let before = row.video.clone();
        edit(&mut row.video);
        if row.video == before {
            return;
        }
        row.video.updated_at = Utc::now();
        let video = row.video.clone();
        self.refresh(cx);
        cx.emit(DatabaseEvent::UpdateVideo(video));
    }

    pub fn set_status(&mut self, id: VideoId, status: VideoStatus, cx: &mut Context<Self>) {
        self.update_video(id, |video| video.status = status, cx);
    }

    /// Lets the video upload to the signed-in YouTube account `key`.
    pub fn set_channel(&mut self, id: VideoId, key: &str, cx: &mut Context<Self>) {
        let Some(service) = YoutubeService::try_global(cx) else { return };
        let Some(channel) = service.read(cx).find_account(key).map(YoutubeAccount::publish_channel) else { return };
        self.update_video(id, |video| video.publish.channel = Some(channel), cx);
    }

    pub fn open_video(&mut self, id: VideoId, cx: &mut Context<Self>) {
        self.pending_open = None;
        cx.emit(DatabaseEvent::OpenVideo(id));
    }

    /// Opens a video from the keyboard, to write: the caret goes to its script.
    pub fn open_script(&mut self, id: VideoId, cx: &mut Context<Self>) {
        self.pending_open = None;
        cx.emit(DatabaseEvent::OpenScript(id));
    }

    /// Opens a video once a click on its title can no longer turn into a
    /// double-click, which renames it instead.
    pub(crate) fn open_video_after_click(&mut self, id: VideoId, cx: &mut Context<Self>) {
        self.pending_open = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(OPEN_AFTER_CLICK).await;
            _ = this.update(cx, |this, cx| this.open_video(id, cx));
        }));
    }

    pub fn create_video(&mut self, status: Option<VideoStatus>, cx: &mut Context<Self>) {
        cx.emit(DatabaseEvent::CreateVideo { status });
    }

    /// Creates a video the active view shows: in the first status its filter
    /// allows, if it filters by status.
    pub(crate) fn create_video_in_view(&mut self, cx: &mut Context<Self>) {
        let status = self
            .active_view()
            .and_then(|view| VideoStatus::ALL.into_iter().find(|status| view.filter.statuses.contains(status)));
        self.create_video(status, cx);
    }

    pub(crate) fn duplicate_video(&mut self, id: VideoId, cx: &mut Context<Self>) {
        cx.emit(DatabaseEvent::DuplicateVideo(id));
    }

    pub(crate) fn confirm_delete_videos(&mut self, ids: Vec<VideoId>, window: &mut Window, cx: &mut Context<Self>) {
        let videos =
            ids.iter().map(|id| (*id, self.video(*id).map(|video| video.title.clone()).unwrap_or_default())).collect();
        let panel = cx.weak_entity();
        confirm_delete_videos(videos, window, cx, move |ids, cx| {
            _ = panel.update(cx, |panel, cx| panel.delete_videos(ids, cx));
        });
    }

    fn delete_videos(&mut self, ids: Vec<VideoId>, cx: &mut Context<Self>) {
        self.rows.retain(|row| !ids.contains(&row.video.id));
        if self.selected.is_some_and(|id| ids.contains(&id)) {
            self.selected = None;
        }
        self.refresh(cx);
        cx.emit(DatabaseEvent::DeleteVideos(ids));
    }

    pub(crate) fn start_editing(&mut self, cell: CellRef, window: &mut Window, cx: &mut Context<Self>) {
        // Another cell's edit is saved, not replaced, even when its field
        // wasn't told it lost focus (as in a window that isn't active).
        if self.editing.is_some_and(|editing| editing != cell) {
            self.save_editing(window, cx);
        }
        let Some(text) = self.cell_text(cell) else {
            return;
        };
        // The second click of a double-click on a title renames, not opens.
        self.pending_open = None;
        self.editing = Some(cell);
        self.editors.cell_input.update(cx, |input, cx| {
            input.set_value(text, window, cx);
            input.focus(window, cx);
        });
        self.sync_table(cx);
    }

    pub(crate) fn cancel_editing(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if self.editing.take().is_some() {
            self.sync_table(cx);
        }
    }

    /// Saves the cell being edited and hands the keyboard back to the table.
    fn commit_editing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.is_some() {
            self.save_editing(window, cx);
            self.table.focus_handle(cx).focus(window, cx);
        }
    }

    /// Saves the cell being edited, e.g. before the database page is left.
    pub fn save_editing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.take_edit(cx) {
            Some(Ok(edited)) => self.update_video(edited.id, |video| *video = edited, cx),
            // Not a number: keep the previous value, and say so.
            Some(Err(message)) => window.push_notification(Notification::error(message), cx),
            None => {}
        }
    }

    /// Ends the cell edit and returns its video with the typed value when
    /// that changes it, for the owner to save without this panel's events,
    /// as when the app quits.
    pub fn take_edited_video(&mut self, cx: &mut Context<Self>) -> Option<Video> {
        let mut edited = self.take_edit(cx)?.ok()?;
        if self.video(edited.id) == Some(&edited) {
            return None;
        }
        edited.updated_at = Utc::now();
        Some(edited)
    }

    /// Ends the cell edit: the video with the typed value, `None` when there
    /// is nothing to save, or why the typed value was refused.
    fn take_edit(&mut self, cx: &mut Context<Self>) -> Option<Result<Video, String>> {
        let cell = self.editing.take()?;
        let text = self.editors.cell_input.read(cx).value().to_string();
        self.sync_table(cx);
        let mut video = self.video(cell.video)?.clone();

        match cell.column {
            // An emptied title is not saved, as on the video page; the cell
            // shows the stored one again.
            Column::Builtin(BuiltinField::Title) => video.title = normalize_title(&text)?,
            Column::Custom(property) => {
                let def = self.properties.iter().find(|def| def.id == property)?;
                let text = text.trim();
                let value = match def.kind {
                    _ if text.is_empty() => None,
                    PropertyKind::Number => match parse_number(text) {
                        Some(number) => Some(PropertyValue::Number(number)),
                        None => return Some(Err(format::not_a_number(text, &def.name))),
                    },
                    PropertyKind::Url => Some(PropertyValue::Url(text.to_string())),
                    _ => Some(PropertyValue::Text(text.to_string())),
                };
                match value {
                    Some(value) => video.properties.insert(property, value),
                    None => video.properties.remove(&property),
                };
            }
            Column::Builtin(_) => return None,
        }
        Some(Ok(video))
    }

    /// The editable text of a cell, or `None` when it is not edited as text.
    fn cell_text(&self, cell: CellRef) -> Option<String> {
        let video = self.video(cell.video)?;
        match cell.column {
            Column::Builtin(BuiltinField::Title) => Some(video.title.clone()),
            Column::Custom(property) => {
                let def = self.properties.iter().find(|def| def.id == property)?;
                let value = video.properties.get(&property);
                match (&def.kind, value) {
                    (PropertyKind::Text, Some(PropertyValue::Text(text)))
                    | (PropertyKind::Url, Some(PropertyValue::Url(text))) => Some(text.clone()),
                    (PropertyKind::Number, Some(PropertyValue::Number(number))) => Some(number.to_string()),
                    (PropertyKind::Text | PropertyKind::Url | PropertyKind::Number, _) => Some(String::new()),
                    _ => None,
                }
            }
            Column::Builtin(_) => None,
        }
    }

    pub(crate) fn set_property_value(
        &mut self,
        id: VideoId,
        property: PropertyId,
        value: Option<PropertyValue>,
        cx: &mut Context<Self>,
    ) {
        self.update_video(
            id,
            |video| match value {
                Some(value) => {
                    video.properties.insert(property, value);
                }
                None => {
                    video.properties.remove(&property);
                }
            },
            cx,
        );
    }

    pub(crate) fn toggle_multi_option(
        &mut self,
        id: VideoId,
        property: PropertyId,
        option: &str,
        cx: &mut Context<Self>,
    ) {
        self.update_video(
            id,
            |video| {
                let mut selected = match video.properties.remove(&property) {
                    Some(PropertyValue::MultiSelect(selected)) => selected,
                    _ => Vec::new(),
                };
                if let Some(ix) = selected.iter().position(|known| known == option) {
                    selected.remove(ix);
                } else {
                    selected.push(option.to_string());
                }
                if !selected.is_empty() {
                    video.properties.insert(property, PropertyValue::MultiSelect(selected));
                }
            },
            cx,
        );
    }

    pub(crate) fn set_date_target(&mut self, target: Option<CellRef>, window: &mut Window, cx: &mut Context<Self>) {
        if self.date_target == target {
            return;
        }
        self.date_target = target;
        if let Some(cell) = target {
            let current = self.cell_date(cell);
            self.editors.calendar.update(cx, |calendar, cx| {
                // The calendar is shared by every date cell: an empty date
                // opens on this month, not on the last date shown.
                if current.is_none() {
                    calendar.apply_date(Date::Single(Some(format::local_day(Utc::now()))));
                }
                calendar.set_date(Date::Single(current), window, cx)
            });
        }
        self.sync_table(cx);
    }

    pub(crate) fn set_date(&mut self, cell: CellRef, date: Option<NaiveDate>, _: &mut Window, cx: &mut Context<Self>) {
        self.date_target = None;
        match cell.column {
            Column::Builtin(BuiltinField::PublishDate) => {
                self.update_video(cell.video, |video| video.publish_date = date, cx);
            }
            Column::Custom(property) => {
                self.set_property_value(cell.video, property, date.map(|date| PropertyValue::Date(Some(date))), cx);
            }
            Column::Builtin(_) => {}
        }
        self.sync_table(cx);
    }

    fn cell_date(&self, cell: CellRef) -> Option<NaiveDate> {
        let video = self.video(cell.video)?;
        match cell.column {
            Column::Builtin(BuiltinField::PublishDate) => video.publish_date,
            Column::Custom(property) => match video.properties.get(&property) {
                Some(PropertyValue::Date(date)) => *date,
                _ => None,
            },
            Column::Builtin(_) => None,
        }
    }

    pub(crate) fn set_tags_target(&mut self, target: Option<VideoId>, window: &mut Window, cx: &mut Context<Self>) {
        if self.tags_target == target {
            return;
        }
        self.tags_target = target;
        self.editors.tag_input.update(cx, |input, cx| input.set_value("", window, cx));
        self.sync_table(cx);
    }

    pub(crate) fn add_tag(&mut self, id: VideoId, tag: &str, cx: &mut Context<Self>) {
        let tag = tag.trim();
        if tag.is_empty() {
            return;
        }
        self.update_video(
            id,
            |video| {
                if !video.tags.iter().any(|known| known.eq_ignore_ascii_case(tag)) {
                    video.tags.push(tag.to_string());
                }
            },
            cx,
        );
    }

    pub(crate) fn remove_tag(&mut self, id: VideoId, tag: &str, cx: &mut Context<Self>) {
        self.update_video(id, |video| video.tags.retain(|known| known != tag), cx);
    }

    fn on_tag_input_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::PressEnter { .. } => {
                let Some(id) = self.tags_target else {
                    return;
                };
                let tag = input.read(cx).value().to_string();
                input.update(cx, |input, cx| input.set_value("", window, cx));
                self.add_tag(id, &tag, cx);
            }
            // Suggestions follow the typed text.
            InputEvent::Change => self.sync_table(cx),
            _ => {}
        }
    }

    pub(crate) fn sort_by(&mut self, column: Column, direction: SortDirection, cx: &mut Context<Self>) {
        self.update_view(|view| view.sorts = vec![SortSpec { column, direction }], cx);
    }

    pub(crate) fn set_column_visible(&mut self, column: Column, visible: bool, cx: &mut Context<Self>) {
        let properties = self.properties.clone();
        self.update_view(
            |view| {
                let mut columns = resolve_columns(view, &properties);
                if let Some(layout) = columns.iter_mut().find(|layout| layout.column == column) {
                    layout.visible = visible || column == Column::Builtin(BuiltinField::Title);
                }
                view.columns = columns;
            },
            cx,
        );
    }

    /// Empties the search box. The search is never saved, so the view
    /// stays as it is.
    pub(crate) fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |search, cx| search.set_value("", window, cx));
        self.refresh(cx);
    }

    pub(crate) fn clear_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |search, cx| search.set_value("", window, cx));
        self.update_view(
            |view| {
                view.filter.statuses.clear();
                view.filter.formats.clear();
                view.filter.search.clear();
            },
            cx,
        );
        self.refresh(cx);
    }

    /// Applies `edit` to the active view and reports it.
    pub(crate) fn update_view(&mut self, edit: impl FnOnce(&mut DatabaseView), cx: &mut Context<Self>) {
        let Some(view) = self.active_view.and_then(|id| self.views.iter_mut().find(|view| view.id == id)) else {
            return;
        };
        let before = view.clone();
        edit(view);
        if *view == before {
            return;
        }
        let view = view.clone();
        self.refresh(cx);
        cx.emit(DatabaseEvent::SaveView(view));
    }

    /// Switches views on the user's behalf and reports it.
    pub(crate) fn select_view(&mut self, id: ViewId, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_view == Some(id) || !self.views.iter().any(|view| view.id == id) {
            return;
        }
        self.set_active_view(id, window, cx);
        cx.emit(DatabaseEvent::ActiveViewChanged(id));
    }

    pub(crate) fn create_view(&mut self, kind: ViewKind, window: &mut Window, cx: &mut Context<Self>) {
        let base = match kind {
            ViewKind::Table => "Table",
            ViewKind::Board => "Board",
        };
        let name = (1..)
            .map(|n| if n == 1 { base.to_string() } else { format!("{base} {n}") })
            .find(|name| !self.views.iter().any(|view| &view.name == name))
            .unwrap_or_else(|| base.to_string());
        let view = DatabaseView {
            id: ViewId::new(),
            name,
            kind,
            columns: self.active_view().map(|view| view.columns.clone()).unwrap_or_default(),
            sorts: Vec::new(),
            filter: FilterSpec { hide_archived: true, ..FilterSpec::default() },
            position: self.views.iter().map(|view| view.position + 1).max().unwrap_or(0),
        };
        let id = view.id;
        self.views.push(view.clone());
        cx.emit(DatabaseEvent::CreateView(view));
        self.select_view(id, window, cx);
    }

    pub(crate) fn rename_view(&mut self, id: ViewId, name: &str, cx: &mut Context<Self>) {
        let name = name.trim();
        let Some(view) = self.views.iter_mut().find(|view| view.id == id) else {
            return;
        };
        if name.is_empty() || view.name == name {
            return;
        }
        view.name = name.to_string();
        let view = view.clone();
        cx.notify();
        cx.emit(DatabaseEvent::SaveView(view));
    }

    pub(crate) fn delete_view(&mut self, id: ViewId, window: &mut Window, cx: &mut Context<Self>) {
        if self.views.len() <= 1 {
            return;
        }
        let Some(ix) = self.views.iter().position(|view| view.id == id) else {
            return;
        };
        self.views.remove(ix);
        cx.emit(DatabaseEvent::DeleteView(id));
        if self.active_view == Some(id) {
            self.active_view = None;
            let next = self.views[ix.min(self.views.len() - 1)].id;
            self.select_view(next, window, cx);
        }
        cx.notify();
    }

    pub(crate) fn open_property_editor(
        &mut self,
        property: Option<PropertyId>,
        mode: PropertyEditorMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let existing = property.and_then(|id| self.properties.iter().find(|def| def.id == id).cloned());
        if property.is_some() && existing.is_none() {
            return;
        }
        let position = self.properties.iter().map(|def| def.position + 1).max().unwrap_or(0);
        let editor = cx.new(|cx| PropertyEditor::new(existing, mode, position, window, cx));
        let panel = cx.weak_entity();
        PropertyEditor::open(
            editor,
            move |def, cx| {
                _ = panel.update(cx, |panel, cx| panel.save_property(def, cx));
            },
            window,
            cx,
        );
    }

    /// Adds a new property, shown in the active view, or replaces an existing one.
    pub(crate) fn save_property(&mut self, def: PropertyDef, cx: &mut Context<Self>) {
        match self.properties.iter_mut().find(|known| known.id == def.id) {
            Some(known) => {
                if *known == def {
                    return;
                }
                *known = def.clone();
                self.refresh(cx);
                cx.emit(DatabaseEvent::UpdateProperty(def));
            }
            None => {
                self.properties.push(def.clone());
                cx.emit(DatabaseEvent::CreateProperty(def.clone()));
                self.set_column_visible(Column::Custom(def.id), true, cx);
                self.refresh(cx);
                self.reveal_column(Column::Custom(def.id), cx);
            }
        }
    }

    /// Scrolls the table horizontally until `column` and the column after it
    /// are in view; after the last column that is the add-property column.
    fn reveal_column(&mut self, column: Column, cx: &mut Context<Self>) {
        let Some(view) = self.active_view() else {
            return;
        };
        let columns = visible_columns(view, &self.properties);
        if let Some(ix) = columns.iter().position(|layout| layout.column == column) {
            self.table.update(cx, |table, cx| table.scroll_to_col(ix + 1, cx));
        }
    }

    pub(crate) fn confirm_delete_property(&mut self, id: PropertyId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(def) = self.properties.iter().find(|def| def.id == id) else {
            return;
        };
        let title: SharedString = format!("Delete property “{}”?", def.name).into();
        let panel = cx.weak_entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let panel = panel.clone();
            alert
                .title(title.clone())
                .description("Its values are removed from every video. This can’t be undone.")
                .confirm()
                .ok_text("Delete")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    _ = panel.update(cx, |panel, cx| panel.delete_property(id, cx));
                    true
                })
        });
    }

    fn delete_property(&mut self, id: PropertyId, cx: &mut Context<Self>) {
        self.properties.retain(|def| def.id != id);
        for row in &mut self.rows {
            row.video.properties.remove(&id);
        }
        self.refresh(cx);
        cx.emit(DatabaseEvent::DeleteProperty(id));
    }

    /// Moves a board card into `status`, before `before` or at the end.
    ///
    /// With a sorted view the order is the sort's, so only the status changes.
    pub(crate) fn move_card(
        &mut self,
        id: VideoId,
        status: VideoStatus,
        before: Option<VideoId>,
        cx: &mut Context<Self>,
    ) {
        if before == Some(id) {
            return;
        }
        let manual = self.active_view().is_some_and(|view| view.sorts.is_empty());
        let lane: Vec<&Video> =
            self.shown.iter().map(|&ix| &self.rows[ix].video).filter(|video| video.status == status).collect();
        let others: Vec<&Video> = lane.iter().copied().filter(|video| video.id != id).collect();
        let slot = before.and_then(|before| others.iter().position(|video| video.id == before)).unwrap_or(others.len());
        let previous = slot.checked_sub(1).and_then(|ix| others.get(ix)).map(|video| video.position);
        let next = others.get(slot).map(|video| video.position);

        let unchanged_slot = lane.iter().position(|video| video.id == id) == Some(slot);
        let position = (manual && !unchanged_slot).then(|| query::position_between(previous, next));
        self.update_video(
            id,
            |video| {
                video.status = status;
                if let Some(position) = position {
                    video.position = position;
                }
            },
            cx,
        );
    }

    pub(super) fn active_index(&self) -> Option<usize> {
        let id = self.active_view?;
        self.views.iter().position(|view| view.id == id)
    }

    pub(super) fn shown_rows(&self) -> impl Iterator<Item = &VideoRow> {
        self.shown.iter().map(|&ix| &self.rows[ix])
    }

    /// How many archived videos the view's "Hide archived" keeps out of what
    /// its other rules and the search would show.
    pub(super) fn hidden_archived(&self, cx: &App) -> usize {
        self.shown_filter(cx).map_or(0, |filter| query::hidden_archived(&self.rows, &filter))
    }

    /// Lets the view show its archived videos.
    pub(crate) fn show_archived(&mut self, cx: &mut Context<Self>) {
        self.update_view(|view| view.filter.hide_archived = false, cx);
        self.refresh(cx);
    }

    /// The active view's filter with the typed search, which is not saved, applied.
    fn shown_filter(&self, cx: &App) -> Option<FilterSpec> {
        let mut filter = self.active_view()?.filter.clone();
        let search = self.search_query(cx);
        if !search.trim().is_empty() {
            filter.search = search;
        }
        Some(filter)
    }

    pub(super) fn empty_reason(&self, cx: &App) -> EmptyReason {
        if self.rows.is_empty() {
            EmptyReason::NoVideos
        } else if !self.search_query(cx).trim().is_empty() {
            EmptyReason::Search
        } else if self.active_view().is_none_or(|view| query::active_filter_count(&view.filter) == 0)
            && self.hidden_archived(cx) > 0
        {
            EmptyReason::Archived
        } else {
            EmptyReason::Filter
        }
    }

    pub(super) fn search_query(&self, cx: &App) -> String {
        self.search.read(cx).value().to_string()
    }

    fn has_video(&self, id: VideoId) -> bool {
        self.rows.iter().any(|row| row.video.id == id)
    }

    /// Re-runs the active view's query and redraws.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.shown = match (self.active_view(), self.shown_filter(cx)) {
            (Some(view), Some(filter)) => {
                let view = DatabaseView { filter, ..view.clone() };
                view_rows(&self.rows, &view, &self.properties)
            }
            _ => Vec::new(),
        };
        self.sync_table(cx);
        cx.notify();
    }

    /// Hands the table delegate a fresh snapshot of what it shows.
    fn sync_table(&mut self, cx: &mut Context<Self>) {
        let (columns, sorts) = match self.active_view() {
            Some(view) => (visible_columns(view, &self.properties), view.sorts.clone()),
            None => (Vec::new(), Vec::new()),
        };
        let mut known_tags: Vec<String> = Vec::new();
        for tag in self.rows.iter().flat_map(|row| &row.video.tags) {
            if !known_tags.iter().any(|known| known.eq_ignore_ascii_case(tag)) {
                known_tags.push(tag.clone());
            }
        }
        known_tags.sort_by_key(|tag| tag.to_lowercase());

        let model = TableModel {
            rows: self.shown_rows().cloned().collect(),
            columns,
            properties: Rc::from(self.properties.as_slice()),
            sorts,
            known_tags: known_tags.into(),
            editing: self.editing,
            date_target: self.date_target,
            tags_target: self.tags_target,
            empty: self.empty_reason(cx),
            now: Utc::now(),
            week_start: self.week_start,
            scale: self.zoom.factor(),
        };
        let selected_ix = self.selected.and_then(|id| model.rows.iter().position(|row| row.video.id == id));
        self.table.update(cx, |table, cx| {
            table.delegate_mut().set_model(model);
            table.refresh(cx);
            match selected_ix {
                Some(ix) if table.selected_row() != Some(ix) => table.set_selected_row(ix, cx),
                Some(_) => {}
                None => {
                    if table.selected_row().is_some() {
                        table.clear_selection(cx);
                    }
                }
            }
            cx.notify();
        });
    }

    fn on_table_event(
        &mut self,
        table: &Entity<TableState<VideoTable>>,
        event: &TableEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            TableEvent::SelectRow(ix) => {
                self.selected = self.shown.get(*ix).map(|&row| self.rows[row].video.id);
            }
            TableEvent::ClearSelection => self.selected = None,
            TableEvent::ColumnWidthsChanged(widths) => {
                let scale = self.zoom.factor();
                let widths: Vec<(Column, Pixels)> = widths
                    .iter()
                    .enumerate()
                    .filter_map(|(ix, width)| Some((table.read(cx).delegate().column_at(ix)?, *width)))
                    .collect();
                let properties = self.properties.clone();
                self.update_view(
                    |view| {
                        let mut columns = resolve_columns(view, &properties);
                        for (column, width) in widths {
                            if let Some(layout) = columns.iter_mut().find(|layout| layout.column == column) {
                                layout.width = (f32::from(width) / scale).round();
                            }
                        }
                        view.columns = columns;
                    },
                    cx,
                );
            }
            _ => {}
        }
    }

    /// Whether the row keys apply: the table itself has focus, not a cell's
    /// menu or popover opened inside it.
    fn row_keys_apply(&self, window: &Window, cx: &App) -> bool {
        self.table.focus_handle(cx).is_focused(window)
    }

    fn on_open_selected(&mut self, _: &OpenSelected, window: &mut Window, cx: &mut Context<Self>) {
        if !self.row_keys_apply(window, cx) {
            cx.propagate();
            return;
        }
        if let Some(id) = self.selected {
            self.open_script(id, cx);
        }
    }

    fn on_rename_selected(&mut self, _: &RenameSelected, window: &mut Window, cx: &mut Context<Self>) {
        if !self.row_keys_apply(window, cx) {
            cx.propagate();
            return;
        }
        if let Some(id) = self.selected {
            let cell = CellRef { video: id, column: Column::Builtin(BuiltinField::Title) };
            self.start_editing(cell, window, cx);
        }
    }

    fn on_delete_selected(&mut self, _: &DeleteSelected, window: &mut Window, cx: &mut Context<Self>) {
        if !self.row_keys_apply(window, cx) {
            cx.propagate();
            return;
        }
        if let Some(id) = self.selected {
            self.confirm_delete_videos(vec![id], window, cx);
        }
    }

    /// Escape in the inline editor: drop the edit and hand the keyboard back
    /// to the table.
    fn on_cancel_edit(&mut self, _: &CancelEdit, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_editing(window, cx);
        self.table.focus_handle(cx).focus(window, cx);
    }

    fn render_table(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let row_height = Size::Medium.table_row_height() * self.zoom.factor();
        let rows = self.shown.len();
        let mut count = match rows {
            1 => "1 video".to_string(),
            rows => format!("{rows} videos"),
        };
        // The Filter badge doesn't count the view's default, so the footer says what it leaves out.
        match self.hidden_archived(cx) {
            0 => {}
            1 => count.push_str(" · 1 archived hidden"),
            hidden => count.push_str(&format!(" · {hidden} archived hidden")),
        }
        let panel = cx.weak_entity();

        v_flex()
            .flex_1()
            .min_h_0()
            .px(PAGE_INSET)
            .pb_2()
            .child(
                div()
                    .w_full()
                    .min_h_0()
                    .map(|this| {
                        if rows == 0 {
                            this.flex_1()
                        } else {
                            // Exactly header plus rows, so the footer follows the
                            // last row; shrinks and scrolls in a short window.
                            this.h(row_height * (rows + 1) as f32)
                        }
                    })
                    // A custom row height leaves the cells' text size to the table.
                    .child(
                        div()
                            .size_full()
                            .text_sm()
                            .child(DataTable::new(&self.table).bordered(false).with_size(Size::Size(row_height))),
                    ),
            )
            .when(rows > 0, |this| {
                this.child(
                    h_flex()
                        .w_full()
                        .flex_shrink_0()
                        .h(row_height)
                        .border_b_1()
                        .border_color(cx.theme().table_row_border)
                        .child(
                            Button::new("table-new-video")
                                .ghost()
                                .small()
                                .icon(IconName::Plus)
                                .label("New video…")
                                .text_color(cx.theme().muted_foreground)
                                .on_click(move |_, _, cx| {
                                    _ = panel.update(cx, |panel, cx| panel.create_video_in_view(cx));
                                }),
                        ),
                )
                .child(
                    div().flex_shrink_0().pt_2().px_2().text_xs().text_color(cx.theme().muted_foreground).child(count),
                )
            })
    }
}

impl Render for DatabasePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let kind = self.active_view().map(|view| view.kind).unwrap_or_default();
        v_flex()
            .id("video-database")
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(Self::on_open_selected))
            .on_action(cx.listener(Self::on_rename_selected))
            .on_action(cx.listener(Self::on_delete_selected))
            .on_action(cx.listener(Self::on_cancel_edit))
            .size_full()
            .min_h_0()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_toolbar(window, cx))
            .map(|this| match kind {
                ViewKind::Table => this.child(self.render_table(cx)),
                ViewKind::Board => this.child(self.render_board(window, cx)),
            })
    }
}
