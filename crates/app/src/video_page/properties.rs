//! The properties panel of a video page: built-in fields, custom properties
//! with an editor for each kind, and the dialog that adds a property.

use std::collections::HashMap;

use chrono::Utc;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::calendar::Date;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::date_picker::{DatePicker, DatePickerEvent, DatePickerState, DateTime};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::form::{Field, Form};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::link::Link;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, Icon, Side, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Task, TestSupportExt as _, Window, div,
};
use videocrm_core::{
    PropertyDef, PropertyId, PropertyKind, PropertyValue, SelectOption, TagColor, Video, VideoFormat, VideoId,
    VideoStatus,
};

use crate::database::Chip;
use crate::database::colors::{format_color, free_tag_color};
use crate::state::{AppEvent, AppState};

/// Retained editors of one video's properties. Values are written through
/// [`AppState`] as soon as an editor commits.
pub struct PropertiesPanel {
    state: Entity<AppState>,
    video_id: VideoId,
    collapsed: bool,
    publish_date: Entity<DatePickerState>,
    tag_input: Entity<InputState>,
    /// Whether the media folder exists, checked off the UI thread after each data change.
    folder_exists: bool,
    folder_check: Option<Task<()>>,
    /// Text, number and URL properties.
    inputs: HashMap<PropertyId, Entity<InputState>>,
    dates: HashMap<PropertyId, Entity<DatePickerState>>,
    editor_subscriptions: HashMap<PropertyId, Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl PropertiesPanel {
    pub fn new(video_id: VideoId, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = AppState::global(cx);
        let publish_date = cx.new(|cx| DatePickerState::new(window, cx).date_format("%b %-d, %Y"));
        let tag_input = cx.new(|cx| InputState::new(window, cx).placeholder("Add a tag…"));
        let subscriptions = vec![
            cx.subscribe_in(&state, window, |this, _, event: &AppEvent, window, cx| {
                if *event == AppEvent::DataChanged {
                    this.sync(window, cx);
                }
            }),
            cx.subscribe(&publish_date, |this, _, event: &DatePickerEvent, cx| {
                let DatePickerEvent::Change(DateTime::Single(date)) = event else { return };
                let date = date.map(|date| date.date());
                this.edit(cx, |video| video.publish_date = date);
            }),
            cx.subscribe_in(&tag_input, window, |this, input, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    let tag = input.read(cx).value().trim().to_string();
                    input.update(cx, |input, cx| input.set_value("", window, cx));
                    this.add_tag(tag, cx);
                }
            }),
        ];
        let mut panel = Self {
            state,
            video_id,
            collapsed: false,
            publish_date,
            tag_input,
            folder_exists: false,
            folder_check: None,
            inputs: HashMap::new(),
            dates: HashMap::new(),
            editor_subscriptions: HashMap::new(),
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

    /// Creates editors for new custom properties and shows the stored values
    /// in editors that are not being edited.
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(video) = self.video(cx).cloned() else { return };
        let properties = self.state.read(cx).properties().to_vec();

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

        let stored = video.publish_date;
        if self.publish_date.read(cx).date() != Date::Single(stored) {
            self.publish_date.update(cx, |picker, cx| picker.set_date(Date::Single(stored), window, cx));
        }

        self.inputs.retain(|id, _| properties.iter().any(|property| property.id == *id));
        self.dates.retain(|id, _| properties.iter().any(|property| property.id == *id));
        for property in &properties {
            let value = video.properties.get(&property.id);
            match &property.kind {
                PropertyKind::Text | PropertyKind::Number | PropertyKind::Url => {
                    let text = value.map(property_text).unwrap_or_default();
                    match self.inputs.get(&property.id) {
                        Some(input) => {
                            let editing = input.read(cx).focus_handle(cx).is_focused(window);
                            if !editing && input.read(cx).value().as_ref() != text {
                                input.update(cx, |input, cx| input.set_value(text, window, cx));
                            }
                        }
                        None => {
                            let input =
                                cx.new(|cx| InputState::new(window, cx).placeholder("Empty").default_value(text));
                            let id = property.id;
                            let subscription =
                                cx.subscribe_in(&input, window, move |this, input, event: &InputEvent, _, cx| {
                                    if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                                        let text = input.read(cx).value().to_string();
                                        this.commit_text(id, text, cx);
                                    }
                                });
                            self.editor_subscriptions.insert(property.id, subscription);
                            self.inputs.insert(property.id, input);
                        }
                    }
                }
                PropertyKind::Date => {
                    let date = match value {
                        Some(PropertyValue::Date(date)) => *date,
                        _ => None,
                    };
                    match self.dates.get(&property.id) {
                        Some(picker) => {
                            if picker.read(cx).date() != Date::Single(date) {
                                picker.update(cx, |picker, cx| picker.set_date(Date::Single(date), window, cx));
                            }
                        }
                        None => {
                            let picker = cx.new(|cx| {
                                let mut picker = DatePickerState::new(window, cx).date_format("%b %-d, %Y");
                                picker.set_date(Date::Single(date), window, cx);
                                picker
                            });
                            let id = property.id;
                            let subscription = cx.subscribe(&picker, move |this, _, event: &DatePickerEvent, cx| {
                                let DatePickerEvent::Change(DateTime::Single(date)) = event else { return };
                                let date = date.map(|date| date.date());
                                this.set_value(id, date.map(|date| PropertyValue::Date(Some(date))), cx);
                            });
                            self.editor_subscriptions.insert(property.id, subscription);
                            self.dates.insert(property.id, picker);
                        }
                    }
                }
                _ => {}
            }
        }
        self.editor_subscriptions.retain(|id, _| self.inputs.contains_key(id) || self.dates.contains_key(id));
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

    fn render_tags(&self, video: &Video, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .flex_wrap()
            .w_full()
            .gap_1()
            .px_2()
            .py_1()
            .children(video.tags.iter().map(|tag| {
                let name = tag.clone();
                h_flex().gap_0p5().child(Chip::new(tag.clone(), free_tag_color(tag))).child(
                    Button::new(SharedString::from(format!("remove-tag-{tag}")))
                        .ghost()
                        .xsmall()
                        .icon(IconName::X)
                        .accessibility_label(format!("Remove tag {tag}"))
                        .on_click(cx.listener(move |this, _, _, cx| this.remove_tag(&name, cx))),
                )
            }))
            .child(Input::new(&self.tag_input).id("property-tags-input").appearance(false).xsmall().w_32())
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
                    .xsmall()
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
                let Some(input) = self.inputs.get(&id) else { return div().into_any_element() };
                let url = match value {
                    Some(PropertyValue::Url(url)) if !url.trim().is_empty() => Some(url.clone()),
                    _ => None,
                };
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
                                .on_click(move |_, _, cx| cx.open_url(&url)),
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
            PropertyKind::Date => match self.dates.get(&id) {
                Some(picker) => div()
                    .w_56()
                    .child(DatePicker::new(picker).appearance(false).small().cleanable(true).placeholder("Empty"))
                    .into_any_element(),
                None => div().into_any_element(),
            },
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
                        let menu = options_menu(
                            menu.check_side(Side::Right),
                            &options,
                            |option| current.as_deref() == Some(option.id.as_str()),
                            {
                                let panel = panel.clone();
                                move |option: &SelectOption, cx: &mut App| {
                                    let value = PropertyValue::Select(Some(option.id.clone()));
                                    _ = panel.update(cx, |panel, cx| panel.set_value(id, Some(value), cx));
                                }
                            },
                        );
                        let panel = panel.clone();
                        menu.separator().item(PopupMenuItem::new("Clear").on_click(move |_, _, cx| {
                            _ = panel.update(cx, |panel, cx| panel.set_value(id, None, cx));
                        }))
                    })
                    .into_any_element()
            }
            PropertyKind::MultiSelect { options } => {
                let selected: Vec<String> = match value {
                    Some(PropertyValue::MultiSelect(ids)) => ids.clone(),
                    _ => Vec::new(),
                };
                let chips: Vec<&SelectOption> = options.iter().filter(|option| selected.contains(&option.id)).collect();
                let panel = cx.weak_entity();
                let options = options.clone();
                Button::new(SharedString::from(format!("property-multi-{id}")))
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
                    })
                    .dropdown_menu(move |menu, _, _| {
                        let selected = selected.clone();
                        options_menu(menu.check_side(Side::Right), &options, |option| selected.contains(&option.id), {
                            let panel = panel.clone();
                            let selected = selected.clone();
                            move |option: &SelectOption, cx: &mut App| {
                                let mut ids = selected.clone();
                                match ids.iter().position(|known| *known == option.id) {
                                    Some(ix) => {
                                        ids.remove(ix);
                                    }
                                    None => ids.push(option.id.clone()),
                                }
                                let value = (!ids.is_empty()).then_some(PropertyValue::MultiSelect(ids));
                                _ = panel.update(cx, |panel, cx| panel.set_value(id, value, cx));
                            }
                        })
                    })
                    .into_any_element()
            }
        }
    }

    fn open_add_property(&self, window: &mut Window, cx: &mut Context<Self>) {
        let position = self.state.read(cx).properties().iter().map(|property| property.position + 1).max().unwrap_or(0);
        let form = cx.new(|cx| AddPropertyForm::new(position, window, cx));
        let name = form.read(cx).name.clone();
        let width = window.rem_size() * 26.;
        window.open_dialog(cx, move |dialog, _, _| {
            let form = form.clone();
            dialog
                .title("New property")
                .width(width)
                .child(form.clone())
                .footer(
                    DialogFooter::new()
                        .child(DialogClose::new().child(Button::new("add-property-cancel").outline().label("Cancel")))
                        .child(DialogAction::new().child(Button::new("add-property-create").primary().label("Create"))),
                )
                .on_ok(move |_, _, cx| form.update(cx, |form, cx| form.submit(cx)))
        });
        name.update(cx, |input, cx| input.focus(window, cx));
    }
}

/// Text shown for a stored text, number or URL value.
fn property_text(value: &PropertyValue) -> String {
    match value {
        PropertyValue::Text(text) | PropertyValue::Url(text) => text.clone(),
        PropertyValue::Number(number) => number.to_string(),
        _ => String::new(),
    }
}

fn empty_value(cx: &App) -> impl IntoElement {
    div().text_sm().text_color(cx.theme().muted_foreground).child("Empty")
}

fn options_menu(
    menu: gpui_kit::component::menu::PopupMenu,
    options: &[SelectOption],
    is_checked: impl Fn(&SelectOption) -> bool,
    on_pick: impl Fn(&SelectOption, &mut App) + Clone + 'static,
) -> gpui_kit::component::menu::PopupMenu {
    if options.is_empty() {
        return menu.label("No options yet. Add them from the column menu in the database.");
    }
    options.iter().fold(menu, |menu, option| {
        let chip = (option.name.clone(), option.color);
        let picked = option.clone();
        let on_pick = on_pick.clone();
        menu.item(
            PopupMenuItem::element(move |_, _| Chip::new(chip.0.clone(), chip.1))
                .checked(is_checked(option))
                .on_click(move |_, _, cx| on_pick(&picked, cx)),
        )
    })
}

impl Render for PropertiesPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(video) = self.video(cx).cloned() else { return div().into_any_element() };
        let properties = self.state.read(cx).properties().to_vec();
        let count = 7 + properties.len();

        if self.collapsed {
            return h_flex()
                .child(
                    Button::new("show-properties")
                        .ghost()
                        .xsmall()
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
        let publish_date = div()
            .w_56()
            .child(DatePicker::new(&self.publish_date).appearance(false).small().cleanable(true).placeholder("Empty"));

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
                            .xsmall()
                            .icon(IconName::Plus)
                            .label("Add property")
                            .on_click(cx.listener(|this, _, window, cx| this.open_add_property(window, cx))),
                    )
                    .child(
                        Button::new("hide-properties")
                            .ghost()
                            .xsmall()
                            .icon(IconName::ChevronDown)
                            .label("Hide properties")
                            .on_click(cx.listener(|this, _, _, cx| this.set_collapsed(true, cx))),
                    ),
            )
            .into_any_element()
    }
}

/// Icon for a custom property, matching the database's column headers.
fn kind_icon(kind: &PropertyKind) -> IconName {
    match kind {
        PropertyKind::Text => IconName::TextAlignStart,
        PropertyKind::Number => IconName::Hash,
        PropertyKind::Select { .. } => IconName::CircleChevronDown,
        PropertyKind::MultiSelect { .. } => IconName::List,
        PropertyKind::Date => IconName::Calendar,
        PropertyKind::Checkbox => IconName::SquareCheck,
        PropertyKind::Url => IconName::Link,
    }
}

/// Property types offered for a new property, in menu order.
const KINDS: [(&str, IconName); 7] = [
    ("Text", IconName::TextAlignStart),
    ("Number", IconName::Hash),
    ("Select", IconName::CircleChevronDown),
    ("Multi-select", IconName::List),
    ("Date", IconName::Calendar),
    ("Checkbox", IconName::SquareCheck),
    ("URL", IconName::Link),
];

/// The New property dialog's form: name, type and, for select types, options.
struct AddPropertyForm {
    name: Entity<InputState>,
    options: Entity<InputState>,
    kind: usize,
    position: i32,
    error: Option<SharedString>,
}

impl AddPropertyForm {
    fn new(position: i32, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            name: cx.new(|cx| InputState::new(window, cx).placeholder("Property name")),
            options: cx.new(|cx| InputState::new(window, cx).placeholder("Sponsor, Collab, Evergreen")),
            kind: 0,
            position,
            error: None,
        }
    }

    fn kind(&self, cx: &App) -> PropertyKind {
        let options = || -> Vec<SelectOption> {
            self.options
                .read(cx)
                .value()
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .enumerate()
                .map(|(ix, name)| SelectOption {
                    id: PropertyId::new().to_string(),
                    name: name.to_string(),
                    // Gray is left for the default; neighbours differ.
                    color: TagColor::ALL[1 + ix % (TagColor::ALL.len() - 1)],
                })
                .collect()
        };
        match self.kind {
            1 => PropertyKind::Number,
            2 => PropertyKind::Select { options: options() },
            3 => PropertyKind::MultiSelect { options: options() },
            4 => PropertyKind::Date,
            5 => PropertyKind::Checkbox,
            6 => PropertyKind::Url,
            _ => PropertyKind::Text,
        }
    }

    fn has_options(&self) -> bool {
        matches!(self.kind, 2 | 3)
    }

    /// Adds the property. Returns whether the dialog may close.
    fn submit(&mut self, cx: &mut Context<Self>) -> bool {
        let name = self.name.read(cx).value().trim().to_string();
        if name.is_empty() {
            self.error = Some("Give the property a name.".into());
            cx.notify();
            return false;
        }
        let property = PropertyDef { id: PropertyId::new(), name, kind: self.kind(cx), position: self.position };
        AppState::global(cx).update(cx, |state, cx| state.insert_property(property, cx));
        true
    }
}

impl Render for AddPropertyForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let form = cx.weak_entity();
        let current = self.kind;
        let (label, icon) = KINDS[current];
        v_flex()
            .gap_3()
            .child(
                Form::new()
                    .child(Field::new().label("Name").child(Input::new(&self.name).id("add-property-name")))
                    .child(
                        Field::new().label("Type").child(
                            Button::new("add-property-kind")
                                .outline()
                                .w_full()
                                .accessibility_label(label)
                                .child(h_flex().gap_2().child(Icon::new(icon).small()).child(label))
                                .dropdown_caret(true)
                                .dropdown_menu(move |menu, _, _| {
                                    KINDS.iter().enumerate().fold(
                                        menu.check_side(Side::Right),
                                        |menu, (ix, (label, icon))| {
                                            let form = form.clone();
                                            menu.item(
                                                PopupMenuItem::new(*label)
                                                    .icon(Icon::new(*icon))
                                                    .checked(ix == current)
                                                    .on_click(move |_, _, cx| {
                                                        _ = form.update(cx, |form, cx| {
                                                            form.kind = ix;
                                                            cx.notify();
                                                        });
                                                    }),
                                            )
                                        },
                                    )
                                }),
                        ),
                    )
                    .when(self.has_options(), |this| {
                        this.child(
                            Field::new()
                                .label("Options")
                                .description("Separate options with commas.")
                                .child(Input::new(&self.options).id("add-property-options")),
                        )
                    }),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(div().text_sm().text_color(cx.theme().danger).child(error))
            })
    }
}
