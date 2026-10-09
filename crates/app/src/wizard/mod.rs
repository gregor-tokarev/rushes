//! "New video" dialog: title, hook, format, template, status and publish date.

use chrono::NaiveDate;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::date_picker::{DatePicker, DatePickerEvent, DatePickerState, DateTime};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::form::{Field, Form};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme as _, Icon, Side, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, Window, div,
};
use videocrm_core::templates::TemplateKind;
use videocrm_core::{NewVideo, VideoFormat, VideoStatus};

use crate::database::Chip;
use crate::shell::elements::ChoiceCard;
use crate::state::AppState;

/// Icon of the blank template, which has no page icon of its own.
const BLANK_TEMPLATE_ICON: &str = "📄";

/// Opens the new-video dialog. `status` preselects the stage (board column "+").
pub fn open_new_video_dialog(status: Option<VideoStatus>, window: &mut Window, cx: &mut App) {
    if window.has_active_dialog(cx) {
        return;
    }
    let form = cx.new(|cx| NewVideoForm::new(status.unwrap_or_default(), window, cx));
    let title = form.read(cx).title.clone();
    let width = window.rem_size() * 36.;
    window.open_dialog(cx, move |dialog, _, _| {
        let form = form.clone();
        dialog
            .title("New video")
            .width(width)
            .child(form.clone())
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().child(Button::new("new-video-cancel").outline().label("Cancel")))
                    .child(DialogAction::new().child(Button::new("new-video-create").primary().label("Create"))),
            )
            .on_ok(move |_, window, cx| form.update(cx, |form, cx| form.submit(window, cx)))
    });
    // Opening the dialog focuses the dialog itself; the title comes first.
    title.update(cx, |input, cx| input.focus(window, cx));
}

/// The dialog's form. Created per opening; [`Self::submit`] creates the video.
struct NewVideoForm {
    title: Entity<InputState>,
    hook: Entity<InputState>,
    format: VideoFormat,
    template: TemplateKind,
    /// Whether the user picked the template, so a format change keeps it.
    template_chosen: bool,
    status: VideoStatus,
    publish_date: Entity<DatePickerState>,
    publish_on: Option<NaiveDate>,
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl NewVideoForm {
    fn new(status: VideoStatus, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let title = cx.new(|cx| InputState::new(window, cx).placeholder("Video title"));
        let hook = cx.new(|cx| InputState::new(window, cx).placeholder("One-line hook — why would someone click?"));
        let publish_date = cx.new(|cx| DatePickerState::new(window, cx).date_format("%b %-d, %Y"));
        let subscriptions = vec![
            cx.subscribe(&publish_date, |this: &mut Self, _, event: &DatePickerEvent, cx| {
                let DatePickerEvent::Change(DateTime::Single(date)) = event else { return };
                this.publish_on = date.map(|date| date.date());
                cx.notify();
            }),
            cx.subscribe(&title, |this: &mut Self, _, _: &gpui_kit::component::input::InputEvent, cx| {
                if this.error.take().is_some() {
                    cx.notify();
                }
            }),
        ];
        Self {
            title,
            hook,
            format: VideoFormat::LongForm,
            template: TemplateKind::Blank,
            template_chosen: false,
            status,
            publish_date,
            publish_on: None,
            error: None,
            _subscriptions: subscriptions,
        }
    }

    fn set_format(&mut self, format: VideoFormat, cx: &mut Context<Self>) {
        self.format = format;
        if !self.template_chosen {
            self.template = default_template(format);
        }
        cx.notify();
    }

    fn set_template(&mut self, template: TemplateKind, cx: &mut Context<Self>) {
        self.template = template;
        self.template_chosen = true;
        cx.notify();
    }

    fn set_status(&mut self, status: VideoStatus, cx: &mut Context<Self>) {
        self.status = status;
        cx.notify();
    }

    /// Creates the video and opens it. Returns whether the dialog may close.
    fn submit(&mut self, _: &mut Window, cx: &mut Context<Self>) -> bool {
        let title = self.title.read(cx).value().trim().to_string();
        if title.is_empty() {
            self.error = Some("Give the video a title.".into());
            cx.notify();
            return false;
        }
        let new = NewVideo {
            title,
            format: self.format,
            hook: self.hook.read(cx).value().trim().to_string(),
            publish_date: self.publish_on,
            template: self.template,
        };
        let status = self.status;
        let state = AppState::global(cx);
        state.update(cx, |state, cx| {
            let Some(id) = state.create_video(new, cx) else { return };
            if status != VideoStatus::default()
                && let Some(mut video) = state.video(id).cloned()
            {
                video.status = status;
                state.update_video(video, cx);
            }
            state.open_new_video(id, cx);
        });
        true
    }

    fn render_format(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = VideoFormat::ALL.iter().position(|format| *format == self.format).unwrap_or(0);
        TabBar::new("new-video-format")
            .segmented()
            .small()
            .selected_index(selected)
            .children(VideoFormat::ALL.iter().map(|format| Tab::new().label(format.label())))
            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                if let Some(format) = VideoFormat::ALL.get(*ix) {
                    this.set_format(*format, cx);
                }
            }))
    }

    fn render_status(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let form = cx.weak_entity();
        let current = self.status;
        Button::new("new-video-status")
            .outline()
            .small()
            .accessibility_label(format!("Status: {}", current.label()))
            .child(Chip::new(current.label(), current.color()).dot(true))
            .dropdown_caret(true)
            .dropdown_menu(move |menu, _, _| {
                VideoStatus::ALL.iter().fold(menu.check_side(Side::Right), |menu, status| {
                    let form = form.clone();
                    let status = *status;
                    menu.item(
                        PopupMenuItem::element(move |_, _| Chip::new(status.label(), status.color()).dot(true))
                            .checked(status == current)
                            .on_click(move |_, _, cx| {
                                _ = form.update(cx, |form, cx| form.set_status(status, cx));
                            }),
                    )
                })
            })
    }

    fn render_templates(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let cards = TemplateKind::ALL.iter().map(|kind| {
            let kind = *kind;
            ChoiceCard::new(
                SharedString::from(format!("template-{}", kind.label().to_lowercase())),
                div().text_base().child(kind.icon().unwrap_or(BLANK_TEMPLATE_ICON)),
                kind.label(),
                kind.description(),
            )
            .selected(kind == self.template)
            .on_click(cx.listener(move |this, _, _, cx| this.set_template(kind, cx)))
        });
        div().grid().grid_cols(3).gap_2().children(cards)
    }
}

/// The template a format starts with until the user picks one.
fn default_template(format: VideoFormat) -> TemplateKind {
    match format {
        VideoFormat::Short => TemplateKind::Short,
        VideoFormat::LongForm | VideoFormat::Livestream => TemplateKind::Blank,
    }
}

impl Render for NewVideoForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("new-video-form")
            .gap_4()
            .child(
                Form::new()
                    .child(
                        Field::new().label("Title").required(true).child(Input::new(&self.title).id("new-video-title")),
                    )
                    .child(Field::new().label("Hook").child(Input::new(&self.hook).id("new-video-hook")))
                    .child(
                        Field::new().label_indent(false).child(
                            h_flex()
                                .w_full()
                                .gap_4()
                                .items_end()
                                .child(
                                    v_flex()
                                        .gap_1()
                                        .child(div().text_sm().font_medium().child("Format"))
                                        .child(self.render_format(cx)),
                                )
                                .child(
                                    v_flex()
                                        .gap_1()
                                        .child(div().text_sm().font_medium().child("Status"))
                                        .child(self.render_status(cx)),
                                )
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .gap_1()
                                        .child(div().text_sm().font_medium().child("Publish date"))
                                        .child(
                                            DatePicker::new(&self.publish_date)
                                                .small()
                                                .cleanable(true)
                                                .placeholder("No date"),
                                        ),
                                ),
                        ),
                    )
                    .child(Field::new().label("Template").child(self.render_templates(cx))),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(Icon::new(IconName::CircleAlert).small())
                        .child(error),
                )
            })
    }
}
