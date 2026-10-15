//! The table view: a [`TableDelegate`] rendering one video per row, with an
//! inline editor for every editable column.
//!
//! The delegate only renders. It holds a snapshot of what the active view
//! shows ([`TableModel`]) that [`DatabasePanel`] replaces after every change,
//! and it routes every edit back to the panel.

use std::rc::Rc;

use chrono::{DateTime, Local, NaiveDate, Utc};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::calendar::CalendarState;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::table::{Column as TableColumn, TableDelegate, TableState};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, Div, ElementId, Entity, Focusable as _, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, SharedString, Stateful, StatefulInteractiveElement as _, Styled as _,
    TestSupportExt as _, WeakEntity, Window, div, px,
};
use rushes_core::input::normalize_link;
use rushes_core::settings::WeekStart;
use rushes_core::{
    BuiltinField, Column, ColumnLayout, PropertyDef, PropertyKind, PropertyValue, RunStatus, SortDirection, SortSpec,
    UploadStatus, VideoFormat, VideoId, VideoRow, VideoStatus, YoutubeState,
};

use super::colors::{format_color, free_tag_color, run_status_color, youtube_color};
use super::elements::{
    CellButton, Chip, NO_OPTIONS_IN_TABLE, attempt_hint, cell_slot, chip_line, empty_value, multi_select_list,
    muted_icon, select_menu, static_cell, youtube_gone,
};
use super::format;
use super::icons::column_icon;
use super::panel::{CELL_EDITOR_CONTEXT, CellRef, DatabasePanel, TEXT_ENTRY_CONTEXT};
use super::property_editor::PropertyEditorMode;
use super::query::column_name;
use crate::publish::{ChannelChoices, ChooseChannel};
use crate::ui::{EmptyState, date_field, format_when, menu_item, video_icon};

/// Narrowest a column can be resized to, in pixels.
const MIN_COLUMN_WIDTH: f32 = 72.0;
/// Width of the trailing "+" column that adds a property, in pixels.
const ADD_COLUMN_WIDTH: f32 = 44.0;
/// Most tag suggestions offered at once.
const MAX_TAG_SUGGESTIONS: usize = 8;

/// Why the table has no rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum EmptyReason {
    #[default]
    NoVideos,
    /// The search finds nothing; the view's filters alone may well match.
    Search,
    /// The view's filters leave nothing.
    Filter,
    /// Only "Hide archived" leaves nothing: every video the view would show is archived.
    Archived,
}

/// What the table shows, prepared by the panel.
pub(crate) struct TableModel {
    pub rows: Vec<VideoRow>,
    pub columns: Vec<ColumnLayout>,
    pub properties: Rc<[PropertyDef]>,
    pub sorts: Vec<SortSpec>,
    /// Every tag used by any video, for suggestions.
    pub known_tags: Rc<[String]>,
    pub editing: Option<CellRef>,
    pub date_target: Option<CellRef>,
    pub tags_target: Option<VideoId>,
    pub empty: EmptyReason,
    pub now: DateTime<Utc>,
    pub week_start: WeekStart,
    /// The interface zoom's scale, 1.0 at 100%: column widths are kept at 100%.
    pub scale: f32,
}

impl Default for TableModel {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            columns: Vec::new(),
            properties: Rc::from([]),
            sorts: Vec::new(),
            known_tags: Rc::from([]),
            editing: None,
            date_target: None,
            tags_target: None,
            empty: EmptyReason::default(),
            now: DateTime::default(),
            week_start: WeekStart::default(),
            scale: 1.,
        }
    }
}

impl TableModel {
    fn today(&self) -> NaiveDate {
        format::local_day(self.now)
    }
}

/// Editors shared by every cell; only one of each is ever open.
#[derive(Clone)]
pub(crate) struct CellEditors {
    pub cell_input: Entity<InputState>,
    pub tag_input: Entity<InputState>,
    pub calendar: Entity<CalendarState>,
}

pub(crate) struct VideoTable {
    panel: WeakEntity<DatabasePanel>,
    editors: CellEditors,
    model: TableModel,
}

impl VideoTable {
    pub fn new(panel: WeakEntity<DatabasePanel>, editors: CellEditors) -> Self {
        Self { panel, editors, model: TableModel::default() }
    }

    pub fn set_model(&mut self, model: TableModel) {
        self.model = model;
    }

    /// The visible database column at a table column index; `None` for the
    /// trailing add-property column.
    pub fn column_at(&self, col_ix: usize) -> Option<Column> {
        self.model.columns.get(col_ix).map(|layout| layout.column)
    }

    fn render_cell(&self, row: &VideoRow, column: Column, window: &mut Window, cx: &mut App) -> AnyElement {
        let cell = CellRef { video: row.video.id, column };
        if self.model.editing == Some(cell) {
            return self.render_inline_editor(cell, window, cx);
        }

        let video = &row.video;
        match column {
            Column::Builtin(field) => match field {
                BuiltinField::Title => self.render_title(row, cx),
                BuiltinField::Status => self.render_status(video.id, video.status),
                BuiltinField::Format => self.render_format(video.id, video.format),
                BuiltinField::PublishDate => self.render_date(cell, video.publish_date, cx),
                BuiltinField::Tags => self.render_tags(video.id, &video.tags, cx),
                BuiltinField::Script => {
                    static_cell(match format::script_summary(row.script_words, row.script_seconds) {
                        Some(summary) => muted_text(summary, cx),
                        None => empty_value(cx),
                    })
                    .into_any_element()
                }
                BuiltinField::Edit => self.render_edit(row, cx),
                BuiltinField::Youtube => self.render_youtube(row, cx),
                BuiltinField::Channel => self.render_channel(row, cx),
                BuiltinField::CreatedAt => {
                    static_cell(muted_text(format_when(video.created_at, self.model.now.with_timezone(&Local)), cx))
                        .into_any_element()
                }
                BuiltinField::UpdatedAt => {
                    static_cell(muted_text(format_when(video.updated_at, self.model.now.with_timezone(&Local)), cx))
                        .into_any_element()
                }
            },
            Column::Custom(property) => {
                let Some(def) = self.model.properties.iter().find(|def| def.id == property) else {
                    return empty_value(cx).into_any_element();
                };
                self.render_custom(cell, def, video.properties.get(&property), cx)
            }
        }
    }

    fn render_title(&self, row: &VideoRow, cx: &mut App) -> AnyElement {
        let video = &row.video;
        let id = video.id;
        // A click on the title text opens the page once a double-click can no
        // longer follow; a double-click anywhere in the cell renames instead.
        let open_soon = {
            let panel = self.panel.clone();
            move |event: &ClickEvent, _: &mut Window, cx: &mut App| {
                // The second click of a double-click goes on to the cell.
                if event.click_count() == 1 {
                    cx.stop_propagation();
                    _ = panel.update(cx, |panel, cx| panel.open_video_after_click(id, cx));
                }
            }
        };
        let rename = {
            let panel = self.panel.clone();
            move |event: &ClickEvent, window: &mut Window, cx: &mut App| {
                if event.click_count() == 2 {
                    let cell = CellRef { video: id, column: Column::Builtin(BuiltinField::Title) };
                    _ = panel.update(cx, |panel, cx| panel.start_editing(cell, window, cx));
                }
            }
        };
        let open = {
            let panel = self.panel.clone();
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                cx.stop_propagation();
                _ = panel.update(cx, |panel, cx| panel.open_video(id, cx));
            }
        };
        let title = video.title.trim();

        // The Open button overlays the end of the title on an opaque surface
        // instead of taking room from it, the way Notion shows it on hover.
        h_flex()
            .id(element_id("title", id))
            .group("video-title")
            .relative()
            .h_6()
            .gap_1p5()
            .min_w_0()
            .on_click(rename)
            .child(video_icon(video.icon.as_deref()))
            .child(
                div()
                    .id(element_id("title-text", id))
                    .test_support()
                    .min_w_0()
                    .truncate()
                    .font_weight(FontWeight::MEDIUM)
                    .cursor_pointer()
                    .map(|this| {
                        if title.is_empty() {
                            this.text_color(cx.theme().muted_foreground).child("Untitled")
                        } else {
                            this.child(title.to_string())
                        }
                    })
                    .on_click(open_soon),
            )
            .child(
                div()
                    .absolute()
                    .right_0()
                    .top_0p5()
                    .pl_1()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().background)
                    .invisible()
                    .group_hover("video-title", |this| this.visible())
                    .child(
                        Button::new(element_id("open", id))
                            .xsmall()
                            .outline()
                            .icon(IconName::PanelRightOpen)
                            .label("Open")
                            .on_click(open),
                    ),
            )
            .into_any_element()
    }

    /// The AI edit state: a finished cut stays "Done" when a later attempt
    /// failed or was cancelled; that attempt is a hint beside it.
    fn render_edit(&self, row: &VideoRow, cx: &mut App) -> AnyElement {
        let Some(status) = row.edit_status() else {
            return static_cell(empty_value(cx)).into_any_element();
        };
        static_cell(
            h_flex()
                .id(element_id("edit", row.video.id))
                .test_support()
                .aria_label(status.label())
                .gap_1()
                .min_w_0()
                .child(Chip::new(status.label(), run_status_color(status)).dot(true))
                .when_some(row.newer_edit_attempt(), |this, attempt| {
                    this.child(attempt_hint(
                        element_id("edit-attempt", row.video.id),
                        attempt_icon(attempt == RunStatus::Failed),
                        format!("Latest edit: {}", attempt.label()).into(),
                        cx,
                    ))
                }),
        )
        .into_any_element()
    }

    /// Where the video is on YouTube, with a button to open it there; before
    /// that, the latest upload attempt. A newer attempt for a video that is
    /// already on YouTube is a hint beside it.
    fn render_youtube(&self, row: &VideoRow, cx: &mut App) -> AnyElement {
        let Some(state) = row.youtube_state() else {
            if row.left_youtube() {
                return static_cell(youtube_gone(element_id("youtube", row.video.id))).into_any_element();
            }
            return static_cell(empty_value(cx)).into_any_element();
        };
        let id = row.video.id;
        let scheduled = format::is_scheduled(row.video.publish.publish_at, self.model.now);
        let link = match state {
            YoutubeState::Live(link) => normalize_link(&link.url),
            YoutubeState::Upload(_) => None,
        };
        let label = format::youtube(state, scheduled);
        static_cell(
            h_flex()
                .id(element_id("youtube", id))
                .test_support()
                .aria_label(label.clone())
                .gap_1()
                .min_w_0()
                .child(Chip::new(label, youtube_color(state, scheduled)).dot(true))
                .when_some(row.newer_upload_attempt(), |this, attempt| {
                    this.child(attempt_hint(
                        element_id("youtube-attempt", id),
                        attempt_icon(matches!(attempt, UploadStatus::Failed { .. })),
                        format!("Latest upload: {}", format::upload(attempt)).into(),
                        cx,
                    ))
                })
                .when_some(link, |this, url| {
                    this.child(
                        Button::new(element_id("open-youtube", id))
                            .ghost()
                            .xsmall()
                            .icon(IconName::ExternalLink)
                            .tooltip("Open on YouTube")
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    )
                }),
        )
        .into_any_element()
    }

    /// The YouTube account the video uploads to, with a menu of the signed-in
    /// accounts. A video that chose none shows the default account, muted.
    fn render_channel(&self, row: &VideoRow, cx: &mut App) -> AnyElement {
        let id = row.video.id;
        let choices = ChannelChoices::of(&row.video, cx);
        let content =
            choices.label(cx).map_or_else(|| empty_value(cx).into_any_element(), IntoElement::into_any_element);
        // Nothing to choose from, or the upload goes to the account it has.
        if !choices.is_choosable() {
            return static_cell(div().id(element_id("channel", id)).min_w_0().child(content).test_support())
                .into_any_element();
        }
        let panel = self.panel.clone();
        let choose: ChooseChannel = Rc::new(move |key, cx| {
            _ = panel.update(cx, |panel, cx| panel.set_channel(id, key, cx));
        });
        CellButton::new(element_id("channel", id))
            .mx_neg_1()
            .child(content)
            .dropdown_menu(move |menu, _, _| choices.menu(menu, choose.clone()))
            .into_any_element()
    }

    fn render_status(&self, id: VideoId, current: VideoStatus) -> AnyElement {
        let panel = self.panel.clone();
        CellButton::new(element_id("status", id))
            .mx_neg_1()
            .child(Chip::new(current.label(), current.color()).dot(true))
            .dropdown_menu(move |menu, _, _| {
                VideoStatus::ALL.into_iter().fold(menu, |menu, status| {
                    let panel = panel.clone();
                    menu.item(
                        PopupMenuItem::element(move |_, _| Chip::new(status.label(), status.color()).dot(true))
                            .checked(status == current)
                            .on_click(move |_, _, cx| {
                                _ = panel.update(cx, |panel, cx| panel.set_status(id, status, cx));
                            }),
                    )
                })
            })
            .into_any_element()
    }

    fn render_format(&self, id: VideoId, current: VideoFormat) -> AnyElement {
        let panel = self.panel.clone();
        CellButton::new(element_id("format", id))
            .mx_neg_1()
            .child(Chip::new(current.label(), format_color(current)))
            .dropdown_menu(move |menu, _, _| {
                VideoFormat::ALL.into_iter().fold(menu, |menu, format| {
                    let panel = panel.clone();
                    menu.item(
                        PopupMenuItem::element(move |_, _| Chip::new(format.label(), format_color(format)))
                            .checked(format == current)
                            .on_click(move |_, _, cx| {
                                _ = panel
                                    .update(cx, |panel, cx| panel.update_video(id, |video| video.format = format, cx));
                            }),
                    )
                })
            })
            .into_any_element()
    }

    fn render_date(&self, cell: CellRef, value: Option<NaiveDate>, cx: &mut App) -> AnyElement {
        let trigger = CellButton::new(element_id(&format!("date-{}", column_key(cell.column)), cell.video))
            .mx_neg_1()
            .child(match value {
                Some(date) => div().child(format::date(date, self.model.today())),
                None => empty_value(cx),
            });

        let calendar = self.editors.calendar.clone();
        let first_day = self.model.week_start.weekday();
        let clear = self.panel.clone();
        let on_open_change = {
            let panel = self.panel.clone();
            move |open: &bool, window: &mut Window, cx: &mut App| {
                let target = open.then_some(cell);
                _ = panel.update(cx, |panel, cx| panel.set_date_target(target, window, cx));
            }
        };

        Popover::new(element_id(&format!("date-popover-{}", column_key(cell.column)), cell.video))
            .open(self.model.date_target == Some(cell))
            .on_open_change(on_open_change)
            .trigger(trigger)
            .content(move |_, _, cx| {
                let clear = clear.clone();
                let calendar = date_field::calendar(&calendar, first_day, cx);
                v_flex().gap_2().child(calendar).when(value.is_some(), |this| {
                    this.child(
                        Button::new("clear-date").ghost().small().icon(IconName::X).label("Clear date").on_click(
                            move |_, window, cx| {
                                _ = clear.update(cx, |panel, cx| panel.set_date(cell, None, window, cx));
                            },
                        ),
                    )
                })
            })
            .into_any_element()
    }

    fn render_tags(&self, id: VideoId, tags: &[String], cx: &mut App) -> AnyElement {
        let trigger = CellButton::new(element_id("tags", id)).mx_neg_1().map(|this| {
            if tags.is_empty() {
                this.child(empty_value(cx))
            } else {
                this.child(chip_line(
                    tags.iter().map(|tag| Chip::new(tag.clone(), free_tag_color(tag)).into_any_element()),
                ))
            }
        });

        let on_open_change = {
            let panel = self.panel.clone();
            move |open: &bool, window: &mut Window, cx: &mut App| {
                let target = open.then_some(id);
                _ = panel.update(cx, |panel, cx| panel.set_tags_target(target, window, cx));
            }
        };
        let tags: Rc<[String]> = tags.into();
        let known_tags = self.model.known_tags.clone();
        let input = self.editors.tag_input.clone();
        let panel = self.panel.clone();

        Popover::new(element_id("tags-popover", id))
            .open(self.model.tags_target == Some(id))
            .on_open_change(on_open_change)
            .track_focus(&input.focus_handle(cx))
            .trigger(trigger)
            .content(move |_, _, cx| tags_editor(id, &tags, &known_tags, &input, &panel, cx))
            .into_any_element()
    }

    fn render_custom(
        &self,
        cell: CellRef,
        def: &PropertyDef,
        value: Option<&PropertyValue>,
        cx: &mut App,
    ) -> AnyElement {
        let id = cell.video;
        let property = def.id;
        match &def.kind {
            PropertyKind::Text => {
                let text = match value {
                    Some(PropertyValue::Text(text)) => text.as_str(),
                    _ => "",
                };
                self.editable_text(cell, text.to_string(), false, cx)
            }
            PropertyKind::Number => {
                let text = match value {
                    Some(PropertyValue::Number(number)) => format::number(*number),
                    _ => String::new(),
                };
                self.editable_text(cell, text, true, cx)
            }
            PropertyKind::Url => {
                let url = match value {
                    Some(PropertyValue::Url(url)) => url.trim().to_string(),
                    _ => String::new(),
                };
                let Some(target) = normalize_link(&url) else {
                    return self.editable_text(cell, url, false, cx);
                };
                h_flex()
                    .size_full()
                    .gap_1()
                    .child(div().flex_1().min_w_0().h_full().child(self.editable_text(cell, url, false, cx)))
                    .child(
                        Button::new(element_id(&format!("open-url-{}", property), id))
                            .ghost()
                            .xsmall()
                            .icon(IconName::ExternalLink)
                            .tooltip("Open link")
                            .on_click(move |_, _, cx| cx.open_url(&target)),
                    )
                    .into_any_element()
            }
            PropertyKind::Select { options } => {
                let current = match value {
                    Some(PropertyValue::Select(Some(option))) => options.iter().find(|o| &o.id == option),
                    _ => None,
                };
                let options = options.clone();
                let selected = current.map(|option| option.id.clone());
                let panel = self.panel.clone();
                CellButton::new(element_id(&format!("select-{}", property), id))
                    .mx_neg_1()
                    .map(|this| match current {
                        Some(option) => this.child(Chip::new(option.name.clone(), option.color)),
                        None => this.child(empty_value(cx)),
                    })
                    .dropdown_menu(move |menu, _, _| {
                        let panel = panel.clone();
                        select_menu(menu, &options, selected.as_deref(), NO_OPTIONS_IN_TABLE, move |value, cx| {
                            let value = PropertyValue::Select(value);
                            _ = panel.update(cx, |panel, cx| panel.set_property_value(id, property, Some(value), cx));
                        })
                    })
                    .into_any_element()
            }
            PropertyKind::MultiSelect { options } => {
                let selected: Vec<String> = match value {
                    Some(PropertyValue::MultiSelect(ids)) => ids.clone(),
                    _ => Vec::new(),
                };
                let chips: Vec<AnyElement> = options
                    .iter()
                    .filter(|option| selected.contains(&option.id))
                    .map(|option| Chip::new(option.name.clone(), option.color).into_any_element())
                    .collect();
                let trigger = CellButton::new(element_id(&format!("multi-{}", property), id)).mx_neg_1().map(|this| {
                    if chips.is_empty() { this.child(empty_value(cx)) } else { this.child(chip_line(chips)) }
                });
                let options = options.clone();
                let panel = self.panel.clone();
                Popover::new(element_id(&format!("multi-popover-{}", property), id))
                    .trigger(trigger)
                    .content(move |_, _, cx| {
                        let panel = panel.clone();
                        multi_select_list(
                            &options,
                            &selected,
                            NO_OPTIONS_IN_TABLE,
                            move |option, cx| {
                                _ = panel.update(cx, |panel, cx| panel.toggle_multi_option(id, property, option, cx));
                            },
                            cx,
                        )
                    })
                    .into_any_element()
            }
            PropertyKind::Date => {
                let date = match value {
                    Some(PropertyValue::Date(date)) => *date,
                    _ => None,
                };
                self.render_date(cell, date, cx)
            }
            PropertyKind::Checkbox => {
                let checked = matches!(value, Some(PropertyValue::Checkbox(true)));
                let panel = self.panel.clone();
                h_flex()
                    .child(Checkbox::new(element_id(&format!("checkbox-{}", property), id)).checked(checked).on_click(
                        move |checked, _, cx| {
                            let value = PropertyValue::Checkbox(*checked);
                            _ = panel.update(cx, |panel, cx| panel.set_property_value(id, property, Some(value), cx));
                        },
                    ))
                    .into_any_element()
            }
        }
    }

    /// Text that turns into an inline editor when clicked.
    fn editable_text(&self, cell: CellRef, text: String, align_right: bool, cx: &mut App) -> AnyElement {
        let panel = self.panel.clone();
        CellButton::new(element_id(&format!("text-{}", column_key(cell.column)), cell.video))
            .mx_neg_1()
            .when(align_right, |this| this.justify_end())
            .map(|this| {
                if text.is_empty() {
                    this.child(empty_value(cx))
                } else {
                    this.child(div().min_w_0().truncate().child(text))
                }
            })
            .on_click(move |_, window, cx| {
                _ = panel.update(cx, |panel, cx| panel.start_editing(cell, window, cx));
            })
            .text_color(cx.theme().foreground)
            .into_any_element()
    }

    fn render_inline_editor(&self, cell: CellRef, _: &mut Window, _: &mut App) -> AnyElement {
        // Escape is bound in the editor's own context (see `CancelEdit`);
        // the text-entry context keeps Enter and Space in the input.
        div()
            .id(element_id("cell-editor", cell.video))
            .key_context(CELL_EDITOR_CONTEXT)
            .size_full()
            .child(
                div()
                    .key_context(TEXT_ENTRY_CONTEXT)
                    .size_full()
                    .flex()
                    .items_center()
                    .mx_neg_1()
                    .child(Input::new(&self.editors.cell_input).id("cell-input").small().w_full()),
            )
            .into_any_element()
    }
}

impl TableDelegate for VideoTable {
    fn columns_count(&self, _: &App) -> usize {
        self.model.columns.len() + 1
    }

    fn rows_count(&self, _: &App) -> usize {
        self.model.rows.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> TableColumn {
        let Some(layout) = self.model.columns.get(col_ix) else {
            return TableColumn::new("add-property", "Add property")
                .width(px(ADD_COLUMN_WIDTH * self.model.scale))
                .resizable(false)
                .movable(false)
                .selectable(false);
        };
        let column = layout.column;
        let name = column_name(column, &self.model.properties).unwrap_or_default();
        let is_number = matches!(
            column,
            Column::Custom(id) if self
                .model
                .properties
                .iter()
                .any(|def| def.id == id && def.kind == PropertyKind::Number)
        );
        TableColumn::new(column_key(column), name)
            .width(px(layout.width.max(MIN_COLUMN_WIDTH) * self.model.scale))
            .min_width(px(MIN_COLUMN_WIDTH * self.model.scale))
            .movable(false)
            .selectable(false)
            .when(column == Column::Builtin(BuiltinField::Title), |this| this.fixed_left())
            .when(is_number, |this| this.text_right())
    }

    fn render_th(&mut self, col_ix: usize, _: &mut Window, cx: &mut Context<TableState<Self>>) -> impl IntoElement {
        let Some(column) = self.column_at(col_ix) else {
            let panel = self.panel.clone();
            return Button::new("add-property")
                .ghost()
                .xsmall()
                .icon(IconName::Plus)
                .tooltip("Add property")
                .on_click(move |_, window, cx| {
                    _ = panel.update(cx, |panel, cx| {
                        panel.open_property_editor(None, PropertyEditorMode::Create, window, cx)
                    });
                })
                .into_any_element();
        };

        let properties = self.model.properties.clone();
        let name = column_name(column, &properties).unwrap_or_default();
        let sort = self.model.sorts.iter().find(|sort| sort.column == column).map(|sort| sort.direction);
        let panel = self.panel.clone();
        cell_slot(
            CellButton::new(SharedString::from(format!("column-{}", column_key(column))))
                .mx_neg_1()
                .text_color(cx.theme().muted_foreground)
                .child(Icon::new(column_icon(column, &properties)).xsmall().flex_shrink_0())
                .child(div().min_w_0().truncate().child(name))
                .when_some(sort, |this, direction| {
                    this.child(
                        Icon::new(match direction {
                            SortDirection::Ascending => IconName::ArrowUp,
                            SortDirection::Descending => IconName::ArrowDown,
                        })
                        .xsmall()
                        .flex_shrink_0()
                        .text_color(cx.theme().foreground),
                    )
                })
                .dropdown_menu(move |menu, _, _| column_menu(menu, column, &properties, &panel)),
        )
        .into_any_element()
    }

    fn render_tr(&mut self, row_ix: usize, _: &mut Window, _: &mut Context<TableState<Self>>) -> Stateful<Div> {
        match self.model.rows.get(row_ix) {
            Some(row) => div().id(element_id("video-row", row.video.id)),
            None => div().id(("video-row", row_ix)),
        }
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let (Some(row), Some(column)) = (self.model.rows.get(row_ix), self.column_at(col_ix)) else {
            return div().into_any_element();
        };
        cell_slot(self.render_cell(row, column, window, cx)).into_any_element()
    }

    fn context_menu(
        &mut self,
        row_ix: usize,
        menu: PopupMenu,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        match self.model.rows.get(row_ix) {
            Some(row) => video_menu(menu, row.video.id, true, &self.panel),
            None => menu,
        }
    }

    fn render_empty(&mut self, _: &mut Window, _: &mut Context<TableState<Self>>) -> impl IntoElement {
        render_empty_state(self.model.empty, &self.panel)
    }
}

/// Commands on one video, shared by table rows and board cards.
pub(crate) fn video_menu(
    menu: PopupMenu,
    id: VideoId,
    renamable: bool,
    panel: &WeakEntity<DatabasePanel>,
) -> PopupMenu {
    let command = |run: fn(&mut DatabasePanel, VideoId, &mut Window, &mut Context<DatabasePanel>)| {
        let panel = panel.clone();
        move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            _ = panel.update(cx, |panel, cx| run(panel, id, window, cx));
        }
    };
    menu.item(
        menu_item("Open").icon(IconName::PanelRightOpen).on_click(command(|panel, id, _, cx| panel.open_video(id, cx))),
    )
    .when(renamable, |menu| {
        menu.item(menu_item("Rename").icon(IconName::Pencil).on_click(command(|_, id, window, cx| {
            // Once the menu has closed: it takes the keyboard back while it is open.
            let panel = cx.weak_entity();
            window.defer(cx, move |window, cx| {
                let cell = CellRef { video: id, column: Column::Builtin(BuiltinField::Title) };
                _ = panel.update(cx, |panel, cx| panel.start_editing(cell, window, cx));
            });
        })))
    })
    .item(
        menu_item("Duplicate").icon(IconName::Copy).on_click(command(|panel, id, _, cx| panel.duplicate_video(id, cx))),
    )
    .separator()
    .item(
        menu_item("Delete…")
            .icon(IconName::Trash)
            .on_click(command(|panel, id, window, cx| panel.confirm_delete_videos(vec![id], window, cx))),
    )
}

type PanelCommand = Rc<dyn Fn(&mut DatabasePanel, &mut Window, &mut Context<DatabasePanel>)>;

fn column_menu(
    menu: PopupMenu,
    column: Column,
    properties: &[PropertyDef],
    panel: &WeakEntity<DatabasePanel>,
) -> PopupMenu {
    let command = |run: PanelCommand| {
        let panel = panel.clone();
        move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            _ = panel.update(cx, |panel, cx| run(panel, window, cx));
        }
    };
    let sort = |direction| {
        command(Rc::new(move |panel: &mut DatabasePanel, _: &mut Window, cx: &mut Context<DatabasePanel>| {
            panel.sort_by(column, direction, cx)
        }))
    };

    let menu = menu
        .item(menu_item("Sort ascending").icon(IconName::ArrowUp).on_click(sort(SortDirection::Ascending)))
        .item(menu_item("Sort descending").icon(IconName::ArrowDown).on_click(sort(SortDirection::Descending)));
    let menu = menu.when(column != Column::Builtin(BuiltinField::Title), |menu| {
        menu.separator().item(
            menu_item("Hide")
                .icon(IconName::EyeOff)
                .on_click(command(Rc::new(move |panel, _, cx| panel.set_column_visible(column, false, cx)))),
        )
    });

    let Column::Custom(property) = column else {
        return menu;
    };
    let has_options = properties.iter().any(|def| {
        def.id == property && matches!(def.kind, PropertyKind::Select { .. } | PropertyKind::MultiSelect { .. })
    });
    let edit = |mode: PropertyEditorMode| {
        command(Rc::new(move |panel, window, cx| panel.open_property_editor(Some(property), mode, window, cx)))
    };
    menu.separator()
        .item(menu_item("Rename…").icon(IconName::Pencil).on_click(edit(PropertyEditorMode::Rename)))
        .when(has_options, |menu| {
            menu.item(menu_item("Edit options…").icon(IconName::List).on_click(edit(PropertyEditorMode::Edit)))
        })
        .separator()
        .item(
            menu_item("Delete property…").icon(IconName::Trash).on_click(command(Rc::new(move |panel, window, cx| {
                panel.confirm_delete_property(property, window, cx)
            }))),
        )
}

fn tags_editor(
    id: VideoId,
    tags: &[String],
    known_tags: &[String],
    input: &Entity<InputState>,
    panel: &WeakEntity<DatabasePanel>,
    cx: &App,
) -> AnyElement {
    let query = input.read(cx).value().trim().to_lowercase();
    let suggestions: Vec<&String> = known_tags
        .iter()
        .filter(|tag| !tags.iter().any(|own| own.eq_ignore_ascii_case(tag)))
        .filter(|tag| query.is_empty() || tag.to_lowercase().contains(&query))
        .take(MAX_TAG_SUGGESTIONS)
        .collect();

    let tag_button = |tag: &String, removing: bool| {
        let panel = panel.clone();
        let value = tag.clone();
        let label = if removing { "remove-tag" } else { "add-tag" };
        div()
            .id(SharedString::from(format!("{label}-{tag}")))
            .flex()
            .items_center()
            .gap_0p5()
            .child(Chip::new(tag.clone(), free_tag_color(tag)))
            .when(removing, |this| this.child(muted_icon(IconName::X, cx)))
            .on_click(move |_, _, cx| {
                _ = panel.update(cx, |panel, cx| {
                    if removing {
                        panel.remove_tag(id, &value, cx);
                    } else {
                        panel.add_tag(id, &value, cx);
                    }
                });
            })
    };

    v_flex()
        .w_64()
        .gap_2()
        .child(div().key_context(TEXT_ENTRY_CONTEXT).child(Input::new(input).id("tag-input").small()))
        .when(!tags.is_empty(), |this| {
            this.child(h_flex().flex_wrap().gap_1().children(tags.iter().map(|tag| tag_button(tag, true))))
        })
        .when(!suggestions.is_empty(), |this| {
            this.child(muted_text("Suggestions", cx).text_xs())
                .child(h_flex().flex_wrap().gap_1().children(suggestions.into_iter().map(|tag| tag_button(tag, false))))
        })
        .into_any_element()
}

/// The empty state of the table and the board.
pub(crate) fn render_empty_state(reason: EmptyReason, panel: &WeakEntity<DatabasePanel>) -> AnyElement {
    let panel = panel.clone();
    let (icon, title, description, action) = match reason {
        EmptyReason::NoVideos => (
            IconName::Clapperboard,
            "No videos yet",
            "Plan your first video: add an idea, then move it through scripting, filming and editing.",
            Button::new("empty-new-video").outline().small().icon(IconName::Plus).label("New video…").on_click(
                move |_, _, cx| {
                    _ = panel.update(cx, |panel, cx| panel.create_video_in_view(cx));
                },
            ),
        ),
        EmptyReason::Search => (
            IconName::SearchX,
            "No videos match",
            "Try another search.",
            Button::new("empty-clear-search").outline().small().label("Clear search").on_click(move |_, window, cx| {
                _ = panel.update(cx, |panel, cx| panel.clear_search(window, cx));
            }),
        ),
        EmptyReason::Filter => (
            IconName::SearchX,
            "No videos match",
            "Remove some filters to see more videos.",
            Button::new("empty-clear-filters").outline().small().label("Clear filters").on_click(
                move |_, window, cx| {
                    _ = panel.update(cx, |panel, cx| panel.clear_filters(window, cx));
                },
            ),
        ),
        EmptyReason::Archived => (
            IconName::Archive,
            "Only archived videos",
            "This view hides archived videos, and every video here is archived.",
            Button::new("empty-show-archived").outline().small().label("Show archived").on_click(move |_, _, cx| {
                _ = panel.update(cx, |panel, cx| panel.show_archived(cx));
            }),
        ),
    };

    EmptyState::new("database-empty", icon, title, description).action(action).into_any_element()
}

/// The hint icon of a newer attempt: a warning when it failed.
fn attempt_icon(failed: bool) -> IconName {
    if failed { IconName::CircleAlert } else { IconName::Info }
}

pub(crate) fn muted_text(text: impl Into<SharedString>, cx: &App) -> Div {
    div().min_w_0().truncate().text_color(cx.theme().muted_foreground).child(text.into())
}

/// A stable element id for a part of one video's row or card.
pub(crate) fn element_id(part: &str, video: VideoId) -> ElementId {
    ElementId::Name(format!("{part}-{video}").into())
}

/// A stable key for a database column, used in element ids.
pub(crate) fn column_key(column: Column) -> String {
    match column {
        Column::Builtin(field) => format!("{field:?}").to_lowercase(),
        Column::Custom(id) => id.to_string(),
    }
}
