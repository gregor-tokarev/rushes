//! The properties panel of a video page: built-in fields, custom properties
//! with an editor for each kind, and the dialog that adds a property.

use std::collections::HashMap;

use chrono::{NaiveDate, Utc};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::calendar::{Calendar, CalendarEvent, CalendarState, Date};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Backspace, Input, InputEvent, InputState};
use gpui_kit::component::link::Link;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Selectable as _, Side, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Task, TestSupportExt as _, Window, div,
};
use videocrm_core::input::normalize_link;
use videocrm_core::{
    Column, PropertyDef, PropertyId, PropertyKind, PropertyValue, SelectOption, Video, VideoFormat, VideoId,
    VideoStatus, ViewKind,
};

use crate::database::Chip;
use crate::database::colors::{format_color, free_tag_color, tag_colors};
use crate::database::{PropertyEditor, PropertyEditorMode, format, kind_icon, multi_select_list, query, select_menu};
use crate::state::{AppEvent, AppState};

/// Retained editors of one video's properties. Values are written through
/// [`AppState`] as soon as an editor commits.
pub struct PropertiesPanel {
    state: Entity<AppState>,
    video_id: VideoId,
    collapsed: bool,
    /// The calendar of the date popover, shared by the date rows.
    calendar: Entity<CalendarState>,
    /// The date row whose calendar is open.
    date_open: Option<DateField>,
    tag_input: Entity<InputState>,
    /// Whether the media folder exists, checked off the UI thread after each data change.
    folder_exists: bool,
    folder_check: Option<Task<()>>,
    /// Editors of text, number and URL properties, each with the subscription
    /// that commits it, so both go away when the property changes type.
    inputs: HashMap<PropertyId, (Entity<InputState>, Subscription)>,
    _subscriptions: Vec<Subscription>,
}

/// A date row of the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DateField {
    Publish,
    Custom(PropertyId),
}

impl PropertiesPanel {
    pub fn new(video_id: VideoId, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = AppState::global(cx);
        let calendar = cx.new(|cx| CalendarState::new(window, cx));
        let tag_input = cx.new(|cx| InputState::new(window, cx).placeholder("Add a tag…"));
        let subscriptions = vec![
            cx.subscribe_in(&state, window, |this, _, event: &AppEvent, window, cx| {
                if *event == AppEvent::DataChanged {
                    this.sync(window, cx);
                }
            }),
            cx.subscribe(&calendar, |this, _, event: &CalendarEvent, cx| {
                let CalendarEvent::Selected(Date::Single(Some(date))) = event else { return };
                if let Some(field) = this.date_open.take() {
                    this.set_date(field, Some(*date), cx);
                }
                cx.notify();
            }),
            // A typed tag is added on Enter, or when the field is left.
            cx.subscribe_in(&tag_input, window, |this, input, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    let tag = input.read(cx).value().trim().to_string();
                    if !tag.is_empty() {
                        input.update(cx, |input, cx| input.set_value("", window, cx));
                        this.add_tag(tag, cx);
                    }
                }
            }),
        ];
        let mut panel = Self {
            state,
            video_id,
            collapsed: false,
            calendar,
            date_open: None,
            tag_input,
            folder_exists: false,
            folder_check: None,
            inputs: HashMap::new(),
            _subscriptions: subscriptions,
        };
        panel.sync(window, cx);
        panel
    }

    fn set_collapsed(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        self.collapsed = collapsed;
        cx.notify();
    }

    fn video<'a>(&self, cx: &'a App) -> Option<&'a Video> {
        self.state.read(cx).video(self.video_id)
    }

    /// Applies `edit` to the stored video and saves it when it changed.
    fn edit(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Video)) {
        let Some(mut video) = self.video(cx).cloned() else { return };
        let before = video.clone();
        edit(&mut video);
        if video != before {
            video.updated_at = Utc::now();
            self.state.update(cx, |state, cx| state.update_video(video, cx));
        }
    }

    fn add_tag(&mut self, tag: String, cx: &mut Context<Self>) {
        if tag.is_empty() {
            return;
        }
        self.edit(cx, |video| {
            if !video.tags.iter().any(|known| known.eq_ignore_ascii_case(&tag)) {
                video.tags.push(tag);
            }
        });
    }

    fn remove_tag(&mut self, tag: &str, cx: &mut Context<Self>) {
        self.edit(cx, |video| video.tags.retain(|known| known != tag));
    }

    /// Backspace in the empty tag field removes the last tag, the keyboard
    /// path to the remove buttons that show on hover.
    fn on_tag_backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        if !self.tag_input.read(cx).value().is_empty() {
            return;
        }
        let Some(last) = self.video(cx).and_then(|video| video.tags.last().cloned()) else { return };
        cx.stop_propagation();
        self.remove_tag(&last, cx);
    }

    /// Saves what the property fields hold but have not committed yet, such
    /// as a value typed right before quitting or leaving the page by keyboard.
    pub fn flush(&mut self, cx: &mut Context<Self>) {
        let ids: Vec<PropertyId> = self.inputs.keys().copied().collect();
        for id in ids {
            self.commit_input(id, cx);
        }
        let tag = self.tag_input.read(cx).value().trim().to_string();
        self.add_tag(tag, cx);
    }

    /// Commits a text, number or URL field when its text differs from the stored value.
    fn commit_input(&mut self, id: PropertyId, cx: &mut Context<Self>) {
        let Some((input, _)) = self.inputs.get(&id) else { return };
        let text = input.read(cx).value().trim().to_string();
        let stored = self.video(cx).and_then(|video| video.properties.get(&id)).cloned();
        let unchanged = match &stored {
            Some(value) => text == property_text(value) || text == display_text(value),
            None => text.is_empty(),
        };
        if !unchanged {
            self.commit_text(id, text, cx);
        }
    }

    /// Shows the stored value in a field that is not being edited.
    fn show_stored(&mut self, id: PropertyId, window: &mut Window, cx: &mut Context<Self>) {
        let Some((input, _)) = self.inputs.get(&id) else { return };
        let input = input.clone();
        if input.read(cx).focus_handle(cx).is_focused(window) {
            return;
        }
        let text = self.video(cx).and_then(|video| video.properties.get(&id)).map(display_text).unwrap_or_default();
        if input.read(cx).value().as_ref() != text {
            input.update(cx, |input, cx| input.set_value(text, window, cx));
        }
    }

    /// A number is edited as typed, without the grouping it is shown with.
    fn show_for_editing(&mut self, id: PropertyId, window: &mut Window, cx: &mut Context<Self>) {
        let Some((input, _)) = self.inputs.get(&id) else { return };
        let input = input.clone();
        let Some(value @ PropertyValue::Number(_)) =
            self.video(cx).and_then(|video| video.properties.get(&id)).cloned()
        else {
            return;
        };
        if input.read(cx).value().as_ref() == display_text(&value) {
            input.update(cx, |input, cx| input.set_value(property_text(&value), window, cx));
        }
    }

    fn set_date(&mut self, field: DateField, date: Option<NaiveDate>, cx: &mut Context<Self>) {
        match field {
            DateField::Publish => self.edit(cx, |video| video.publish_date = date),
            DateField::Custom(id) => self.set_value(id, date.map(|date| PropertyValue::Date(Some(date))), cx),
        }
    }

    /// Opens the calendar of a date row on its date, or closes it.
    fn set_date_open(&mut self, field: Option<DateField>, window: &mut Window, cx: &mut Context<Self>) {
        self.date_open = field;
        if let Some(field) = field {
            let date = self.date(field, cx);
            self.calendar.update(cx, |calendar, cx| calendar.set_date(Date::Single(date), window, cx));
        }
        cx.notify();
    }

    fn date(&self, field: DateField, cx: &App) -> Option<NaiveDate> {
        let video = self.video(cx)?;
        match field {
            DateField::Publish => video.publish_date,
            DateField::Custom(id) => match video.properties.get(&id) {
                Some(PropertyValue::Date(date)) => *date,
                _ => None,
            },
        }
    }

    /// Creates editors for new custom properties and shows the stored values
    /// in editors that are not being edited.
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(video) = self.video(cx).cloned() else { return };
        let properties = self.state.read(cx).properties().to_vec();

        // A typed tag that `flush` added is done.
        let typed = self.tag_input.read(cx).value().trim().to_string();
        if !typed.is_empty() && video.tags.iter().any(|tag| tag.eq_ignore_ascii_case(&typed)) {
            self.tag_input.update(cx, |input, cx| input.set_value("", window, cx));
        }

        let folder = video.folder.clone();
        self.folder_check = Some(cx.spawn(async move |this, cx| {
            let exists = match folder {
                Some(folder) => cx.background_spawn(async move { folder.is_dir() }).await,
                None => false,
            };
            _ = this.update(cx, |this, cx| {
                if this.folder_exists != exists {
                    this.folder_exists = exists;
                    cx.notify();
                }
            });
        }));

        self.inputs
            .retain(|id, _| properties.iter().any(|property| property.id == *id && is_text_like(&property.kind)));
        if let Some(DateField::Custom(id)) = self.date_open
            && !properties.iter().any(|property| property.id == id && property.kind == PropertyKind::Date)
        {
            self.date_open = None;
        }
        for property in properties.iter().filter(|property| is_text_like(&property.kind)) {
            let id = property.id;
            if self.inputs.contains_key(&id) {
                self.show_stored(id, window, cx);
                continue;
            }
            let text = video.properties.get(&id).map(display_text).unwrap_or_default();
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("Empty").default_value(text));
            let subscription =
                cx.subscribe_in(&input, window, move |this, _, event: &InputEvent, window, cx| match event {
                    InputEvent::Focus => this.show_for_editing(id, window, cx),
                    InputEvent::PressEnter { .. } | InputEvent::Blur => {
                        this.commit_input(id, cx);
                        this.show_stored(id, window, cx);
                    }
                    InputEvent::Change => {}
                });
            self.inputs.insert(id, (input, subscription));
        }
        cx.notify();
    }

    /// Sets or, with `None`, clears a custom property's value.
    fn set_value(&mut self, id: PropertyId, value: Option<PropertyValue>, cx: &mut Context<Self>) {
        self.edit(cx, |video| match value {
            Some(value) => {
                video.properties.insert(id, value);
            }
            None => {
                video.properties.remove(&id);
            }
        });
    }

    fn commit_text(&mut self, id: PropertyId, text: String, cx: &mut Context<Self>) {
        let Some(property) = self.state.read(cx).properties().iter().find(|property| property.id == id).cloned() else {
            return;
        };
        let text = text.trim().to_string();
        let value = if text.is_empty() {
            None
        } else {
            match property.kind {
                PropertyKind::Number => match text.replace(',', "").parse::<f64>() {
                    Ok(number) => Some(PropertyValue::Number(number)),
                    Err(_) => {
                        let message = format!("“{text}” isn’t a number, so {} wasn’t changed.", property.name);
                        self.state.update(cx, |state, cx| state.report_error(message, cx));
                        self.state.update(cx, |state, cx| state.reload(cx));
                        return;
                    }
                },
                PropertyKind::Url => Some(PropertyValue::Url(text)),
                _ => Some(PropertyValue::Text(text)),
            }
        };
        self.set_value(id, value, cx);
    }

    fn open_media_folder(&mut self, cx: &mut Context<Self>) {
        let id = self.video_id;
        if let Some(folder) = self.state.update(cx, |state, cx| state.ensure_video_folder(id, cx)) {
            cx.open_with_system(&folder);
        }
    }

    fn render_row(
        &self,
        id: impl Into<SharedString>,
        icon: IconName,
        label: impl Into<SharedString>,
        value: impl IntoElement,
        cx: &App,
    ) -> impl IntoElement {
        let id: SharedString = id.into();
        h_flex()
            .id(SharedString::from(format!("property-{id}")))
            .test_support()
            .w_full()
            .min_h_8()
            .gap_1()
            .rounded(cx.theme().radius)
            .hover(|this| this.bg(cx.theme().muted.opacity(0.5)))
            .child(
                h_flex()
                    .flex_none()
                    .w_40()
                    .h_8()
                    .px_2()
                    .gap_2()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(Icon::new(icon).small())
                    .child(div().min_w_0().truncate().child(label.into())),
            )
            .child(h_flex().flex_1().min_w_0().min_h_8().child(value))
    }

    fn render_status(&self, video: &Video, cx: &mut Context<Self>) -> impl IntoElement {
        let panel = cx.weak_entity();
        let current = video.status;
        Button::new("property-status-value")
            .ghost()
            .small()
            .accessibility_label(format!("Status: {}", current.label()))
            .child(Chip::new(current.label(), current.color()).dot(true))
            .dropdown_menu(move |menu, _, _| {
                VideoStatus::ALL.iter().fold(menu.check_side(Side::Right), |menu, status| {
                    let panel = panel.clone();
                    let status = *status;
                    menu.item(
                        PopupMenuItem::element(move |_, _| Chip::new(status.label(), status.color()).dot(true))
                            .checked(status == current)
                            .on_click(move |_, _, cx| {
                                _ = panel.update(cx, |panel, cx| panel.edit(cx, |video| video.status = status));
                            }),
                    )
                })
            })
    }

    fn render_format(&self, video: &Video, cx: &mut Context<Self>) -> impl IntoElement {
        let panel = cx.weak_entity();
        let current = video.format;
        Button::new("property-format-value")
            .ghost()
            .small()
            .accessibility_label(format!("Format: {}", current.label()))
            .child(Chip::new(current.label(), format_color(current)))
            .dropdown_menu(move |menu, _, _| {
                VideoFormat::ALL.iter().fold(menu.check_side(Side::Right), |menu, format| {
                    let panel = panel.clone();
                    let format = *format;
                    menu.item(
                        PopupMenuItem::element(move |_, _| Chip::new(format.label(), format_color(format)))
                            .checked(format == current)
                            .on_click(move |_, _, cx| {
                                _ = panel.update(cx, |panel, cx| panel.edit(cx, |video| video.format = format));
                            }),
                    )
                })
            })
    }

    /// Tags as chips. A chip's remove button shows on hover; from the
    /// keyboard, Backspace in the empty tag field removes the last tag.
    fn render_tags(&self, video: &Video, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .flex_wrap()
            .w_full()
            .gap_1()
            .px_2()
            .py_1()
            .children(video.tags.iter().map(|tag| {
                let name = tag.clone();
                let group = SharedString::from(format!("tag-chip-{tag}"));
                let colors = tag_colors(free_tag_color(tag), cx);
                h_flex()
                    .id(SharedString::from(format!("tag-{tag}")))
                    .group(group.clone())
                    .relative()
                    .child(Chip::new(tag.clone(), free_tag_color(tag)))
                    .child(
                        // Over the chip's end, in the chip's own colors.
                        h_flex()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .right_0()
                            .rounded(cx.theme().radius_tokens().sm)
                            .bg(colors.background)
                            .invisible()
                            .group_hover(group, |this| this.visible())
                            .child(
                                Button::new(SharedString::from(format!("remove-tag-{tag}")))
                                    .ghost()
                                    .xsmall()
                                    .icon(Icon::new(IconName::X).text_color(colors.foreground))
                                    .tab_stop(false)
                                    .accessibility_label(format!("Remove tag {tag}"))
                                    .on_click(cx.listener(move |this, _, _, cx| this.remove_tag(&name, cx))),
                            ),
                    )
            }))
            .child(
                div().capture_action(cx.listener(Self::on_tag_backspace)).child(
                    Input::new(&self.tag_input).id("property-tags-input").appearance(false).xsmall().px_0().w_32(),
                ),
            )
    }

    /// A date shown like the other values; its calendar opens in a popover.
    fn render_date(&self, field: DateField, cx: &mut Context<Self>) -> AnyElement {
        let date = self.date(field, cx);
        let key = match field {
            DateField::Publish => "publish".to_string(),
            DateField::Custom(id) => id.to_string(),
        };
        let trigger = Button::new(SharedString::from(format!("property-date-{key}")))
            .ghost()
            .small()
            .selected(self.date_open == Some(field))
            .map(|this| match date {
                Some(date) => this.label(date.format("%b %-d, %Y").to_string()),
                None => this.child(empty_value(cx)),
            });
        let calendar = self.calendar.clone();
        let panel = cx.weak_entity();
        Popover::new(SharedString::from(format!("property-date-popover-{key}")))
            .open(self.date_open == Some(field))
            .on_open_change(cx.listener(move |this, open: &bool, window, cx| {
                this.set_date_open(open.then_some(field), window, cx);
            }))
            .trigger(trigger)
            .content(move |_, _, _| {
                let panel = panel.clone();
                v_flex().gap_2().child(Calendar::new(&calendar).small()).when(date.is_some(), |this| {
                    this.child(
                        Button::new("clear-date").ghost().small().icon(IconName::X).label("Clear date").on_click(
                            move |_, _, cx| {
                                _ = panel.update(cx, |panel, cx| {
                                    panel.date_open = None;
                                    panel.set_date(field, None, cx);
                                    cx.notify();
                                });
                            },
                        ),
                    )
                })
            })
            .into_any_element()
    }

    fn render_media_folder(&self, video: &Video, cx: &mut Context<Self>) -> impl IntoElement {
        let folder = video.folder.clone().filter(|_| self.folder_exists);
        h_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .pl_2()
            .child(div().flex_1().min_w_0().truncate().text_sm().text_color(cx.theme().muted_foreground).child(
                match &folder {
                    Some(folder) => folder.display().to_string(),
                    None => "Created when you open it".to_string(),
                },
            ))
            .child(
                Button::new("open-media-folder")
                    .ghost()
                    .small()
                    .icon(IconName::FolderOpen)
                    .label("Open folder")
                    .on_click(cx.listener(|this, _, _, cx| this.open_media_folder(cx))),
            )
    }

    fn render_youtube(&self, video: &Video, cx: &App) -> AnyElement {
        match &video.youtube {
            Some(link) => h_flex()
                .gap_2()
                .px_2()
                .text_sm()
                .child(Link::new("youtube-link").href(link.url.clone()).child(link.url.clone()))
                .child(div().text_color(cx.theme().muted_foreground).child(link.privacy.label()))
                .into_any_element(),
            None => {
                div().px_2().text_sm().text_color(cx.theme().muted_foreground).child("Not uploaded").into_any_element()
            }
        }
    }

    fn render_custom(&self, property: &PropertyDef, video: &Video, cx: &mut Context<Self>) -> AnyElement {
        let value = video.properties.get(&property.id);
        let id = property.id;
        match &property.kind {
            PropertyKind::Text | PropertyKind::Number | PropertyKind::Url => {
                let Some((input, _)) = self.inputs.get(&id) else { return div().into_any_element() };
                let url = match value {
                    Some(PropertyValue::Url(url)) if !url.trim().is_empty() => Some(url.clone()),
                    _ => None,
                };
                // Links open through the same rule as everywhere else: a bare
                // address gets https://, and only web and mail links open.
                h_flex()
                    .w_full()
                    .gap_1()
                    .child(
                        Input::new(input)
                            .id(SharedString::from(format!("property-input-{id}")))
                            .appearance(false)
                            .small()
                            .flex_1(),
                    )
                    .when_some(url, |this, url| {
                        this.child(
                            Button::new(SharedString::from(format!("open-url-{id}")))
                                .ghost()
                                .xsmall()
                                .icon(IconName::ExternalLink)
                                .accessibility_label("Open link")
                                .tooltip("Open link")
                                .on_click(move |_, window, cx| open_link(&url, window, cx)),
                        )
                    })
                    .into_any_element()
            }
            PropertyKind::Checkbox => {
                let checked = matches!(value, Some(PropertyValue::Checkbox(true)));
                div()
                    .px_2()
                    .child(Checkbox::new(SharedString::from(format!("property-check-{id}"))).checked(checked).on_click(
                        cx.listener(move |this, checked: &bool, _, cx| {
                            this.set_value(id, Some(PropertyValue::Checkbox(*checked)), cx);
                        }),
                    ))
                    .into_any_element()
            }
            PropertyKind::Date => self.render_date(DateField::Custom(id), cx),
            PropertyKind::Select { options } => {
                let selected = match value {
                    Some(PropertyValue::Select(Some(option))) => options.iter().find(|known| known.id == *option),
                    _ => None,
                };
                let panel = cx.weak_entity();
                let options = options.clone();
                let current = selected.map(|option| option.id.clone());
                Button::new(SharedString::from(format!("property-select-{id}")))
                    .ghost()
                    .small()
                    .accessibility_label(property.name.clone())
                    .map(|this| match selected {
                        Some(option) => this.child(Chip::new(option.name.clone(), option.color)),
                        None => this.child(empty_value(cx)),
                    })
                    .dropdown_menu(move |menu, _, _| {
                        let panel = panel.clone();
                        select_menu(menu, &options, current.as_deref(), move |value, cx| {
                            let value = value.map(|option| PropertyValue::Select(Some(option)));
                            _ = panel.update(cx, |panel, cx| panel.set_value(id, value, cx));
                        })
                    })
                    .into_any_element()
            }
            PropertyKind::MultiSelect { options } => {
                let selected: Vec<String> = match value {
                    Some(PropertyValue::MultiSelect(ids)) => ids.clone(),
                    _ => Vec::new(),
                };
                let chips: Vec<&SelectOption> = options.iter().filter(|option| selected.contains(&option.id)).collect();
                let trigger = Button::new(SharedString::from(format!("property-multi-{id}")))
                    .ghost()
                    .small()
                    .accessibility_label(property.name.clone())
                    .map(|this| {
                        if chips.is_empty() {
                            this.child(empty_value(cx))
                        } else {
                            this.child(
                                h_flex()
                                    .gap_1()
                                    .children(chips.iter().map(|option| Chip::new(option.name.clone(), option.color))),
                            )
                        }
                    });
                let panel = cx.weak_entity();
                let options = options.clone();
                // Like the table's cell: the list stays open while options are ticked.
                Popover::new(SharedString::from(format!("property-multi-popover-{id}")))
                    .trigger(trigger)
                    .content(move |_, _, cx| {
                        let (panel, selected) = (panel.clone(), selected.clone());
                        multi_select_list(
                            &options,
                            &selected,
                            move |option, cx| {
                                _ = panel.update(cx, |panel, cx| panel.toggle_option(id, option, cx));
                            },
                            cx,
                        )
                    })
                    .into_any_element()
            }
        }
    }

    /// Ticks or unticks `option` of a multi-select property.
    fn toggle_option(&mut self, id: PropertyId, option: &str, cx: &mut Context<Self>) {
        let Some(video) = self.video(cx) else { return };
        let mut ids = match video.properties.get(&id) {
            Some(PropertyValue::MultiSelect(ids)) => ids.clone(),
            _ => Vec::new(),
        };
        match ids.iter().position(|known| known == option) {
            Some(ix) => {
                ids.remove(ix);
            }
            None => ids.push(option.to_owned()),
        }
        let value = (!ids.is_empty()).then_some(PropertyValue::MultiSelect(ids));
        self.set_value(id, value, cx);
    }

    /// The database's New property dialog; the property also shows as a column
    /// in the database's tables, as one added there does.
    fn open_add_property(&self, window: &mut Window, cx: &mut Context<Self>) {
        let position = self.state.read(cx).properties().iter().map(|property| property.position + 1).max().unwrap_or(0);
        let editor = cx.new(|cx| PropertyEditor::new(None, PropertyEditorMode::Create, position, window, cx));
        PropertyEditor::open(editor, |property, cx| add_property_to_tables(property, cx), window, cx);
    }
}

/// Adds `property` and shows it in every table view.
fn add_property_to_tables(property: PropertyDef, cx: &mut App) {
    AppState::global(cx).update(cx, |state, cx| {
        state.insert_property(property.clone(), cx);
        let properties = state.properties().to_vec();
        let tables: Vec<_> = state.views().iter().filter(|view| view.kind == ViewKind::Table).cloned().collect();
        for mut view in tables {
            let mut columns = query::resolve_columns(&view, &properties);
            for layout in &mut columns {
                if layout.column == Column::Custom(property.id) {
                    layout.visible = true;
                }
            }
            view.columns = columns;
            state.save_view(view, cx);
        }
    });
}

/// Status, format, publish date, tags, media folder and YouTube.
const BUILT_IN_ROWS: usize = 6;

/// Kinds edited in a text input.
fn is_text_like(kind: &PropertyKind) -> bool {
    matches!(kind, PropertyKind::Text | PropertyKind::Number | PropertyKind::Url)
}

/// A stored text, number or URL value as it is edited.
fn property_text(value: &PropertyValue) -> String {
    match value {
        PropertyValue::Text(text) | PropertyValue::Url(text) => text.clone(),
        PropertyValue::Number(number) => number.to_string(),
        _ => String::new(),
    }
}

/// A stored value as a field shows it at rest: numbers grouped like the
/// database's cells.
fn display_text(value: &PropertyValue) -> String {
    match value {
        PropertyValue::Number(number) => format::number(*number),
        value => property_text(value),
    }
}

/// Opens a link property's value, or says why it can't be opened.
fn open_link(url: &str, window: &mut Window, cx: &mut App) {
    match normalize_link(url) {
        Some(link) => cx.open_url(&link),
        None => {
            window.push_notification(format!("“{}” isn’t a web or email link, so it wasn’t opened.", url.trim()), cx)
        }
    }
}

fn empty_value(cx: &App) -> impl IntoElement {
    div().text_sm().text_color(cx.theme().muted_foreground).child("Empty")
}

impl Render for PropertiesPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(video) = self.video(cx).cloned() else { return div().into_any_element() };
        let properties = self.state.read(cx).properties().to_vec();
        let count = BUILT_IN_ROWS + properties.len();

        if self.collapsed {
            return h_flex()
                .child(
                    Button::new("show-properties")
                        .ghost()
                        .small()
                        .icon(IconName::ChevronRight)
                        .label(format!("{count} properties"))
                        .on_click(cx.listener(|this, _, _, cx| this.set_collapsed(false, cx))),
                )
                .into_any_element();
        }

        let custom: Vec<AnyElement> = properties
            .iter()
            .map(|property| {
                let value = self.render_custom(property, &video, cx);
                self.render_row(property.id.to_string(), kind_icon(&property.kind), property.name.clone(), value, cx)
                    .into_any_element()
            })
            .collect();

        let status = self.render_status(&video, cx).into_any_element();
        let format = self.render_format(&video, cx).into_any_element();
        let tags = self.render_tags(&video, cx).into_any_element();
        let folder = self.render_media_folder(&video, cx).into_any_element();
        let youtube = self.render_youtube(&video, cx);
        let publish_date = self.render_date(DateField::Publish, cx);

        v_flex()
            .id("properties")
            .w_full()
            .gap_0p5()
            .child(self.render_row("status", IconName::CircleDot, "Status", status, cx))
            .child(self.render_row("format", IconName::CircleChevronDown, "Format", format, cx))
            .child(self.render_row("publish-date", IconName::Calendar, "Publish date", publish_date, cx))
            .child(self.render_row("tags", IconName::Tags, "Tags", tags, cx))
            .child(self.render_row("media-folder", IconName::Folder, "Media folder", folder, cx))
            .child(self.render_row("youtube", IconName::CirclePlay, "YouTube", youtube, cx))
            .children(custom)
            .child(
                h_flex()
                    .pt_1()
                    .gap_1()
                    .child(
                        Button::new("add-property")
                            .ghost()
                            .small()
                            .icon(IconName::Plus)
                            .label("Add property…")
                            .on_click(cx.listener(|this, _, window, cx| this.open_add_property(window, cx))),
                    )
                    .child(
                        Button::new("hide-properties")
                            .ghost()
                            .small()
                            .icon(IconName::ChevronDown)
                            .label("Hide properties")
                            .on_click(cx.listener(|this, _, _, cx| this.set_collapsed(true, cx))),
                    ),
            )
            .into_any_element()
    }
}
