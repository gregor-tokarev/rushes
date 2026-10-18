//! The chat with Codex about a video's thumbnails, in a sheet over the Publish tab:
//! what the creator asked, what Codex answered with the thumbnails it drew
//! (choosing one makes it the video's thumbnail), and the box to ask for more.
//! Codex remembers the chat, so a message can refer to anything said or drawn
//! before; [`PublishAssistant`] owns it and keeps it.

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::bubble::{Bubble, BubbleVariant};
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::message::{Message, MessageAlignment, MessageContent};
use gpui_kit::component::message_scroller::{MessageScroller, MessageScrollerState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, Render, SharedString, StyleRefinement, Styled as _, Subscription, WeakEntity, Window,
    div,
};
use rushes_core::VideoId;

use super::assistant::{ChatMessage, ChatRole, PublishAssistant, ThumbnailJob};
use super::panel::PublishPanel;
use super::ui::{file_name, help_text, status_line, thumbnail_tile};
use crate::ui::{EmptyState, state_tooltip};

/// The sheet's width, in rems: two thumbnails side by side at a size that shows them.
const SHEET_WIDTH: f32 = 32.;
/// At most this share of the window, when zoomed in.
const SHEET_MAX_SHARE: f32 = 0.9;
/// What "Draw 3 ideas" says for the creator.
const IDEAS_MESSAGE: &str = "Draw 3 different ideas from the title and script.";

/// Opens the chat about the thumbnails of `video_id`, whose Publish tab is `panel`.
/// `has_video` says whether there is a video to show Codex frames of.
pub(super) fn open_thumbnail_chat(
    panel: WeakEntity<PublishPanel>,
    video_id: VideoId,
    has_video: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(assistant) = PublishAssistant::try_global(cx) else { return };
    let chat = cx.new(|cx| ThumbnailChatView::new(assistant, panel, video_id, has_video, window, cx));
    let composer = chat.read(cx).composer.clone();
    window.open_sheet(cx, move |sheet, window, cx| {
        let width = (window.rem_size() * SHEET_WIDTH).min(window.viewport_size().width * SHEET_MAX_SHARE);
        // The chat lays out its own insets, so its scrollbar meets the sheet's edge.
        sheet.title(chat.read(cx).render_title(cx)).size(width).px_0().child(chat.clone())
    });
    composer.update(cx, |input, cx| input.focus(window, cx));
}

/// A row of the conversation.
#[derive(Clone, Debug, PartialEq)]
enum Row {
    Message(ChatMessage),
    /// Codex answers the last message.
    Working {
        activity: Option<SharedString>,
        cancelling: bool,
    },
    /// The last message got no answer: Codex was cancelled, or Rushes closed.
    Unanswered,
}

/// What the rows need from the view to draw and to act.
struct RowContext {
    view: WeakEntity<ThumbnailChatView>,
    rows: Vec<Row>,
    chosen: Option<PathBuf>,
}

pub(super) struct ThumbnailChatView {
    video_id: VideoId,
    assistant: Entity<PublishAssistant>,
    panel: WeakEntity<PublishPanel>,
    has_video: bool,
    use_frames: bool,
    composer: Entity<TextareaState>,
    scroller: Entity<MessageScrollerState>,
    rows: Vec<Row>,
    /// The `rem` the rows were measured at: zooming changes their heights.
    rem_size: Pixels,
    /// Why the last message couldn't be sent.
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl ThumbnailChatView {
    fn new(
        assistant: Entity<PublishAssistant>,
        panel: WeakEntity<PublishPanel>,
        video_id: VideoId,
        has_video: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 8)
                .submit_on_enter(true)
                .placeholder("Describe a thumbnail, or ask for a change…")
        });
        let mut subscriptions = vec![
            cx.observe(&assistant, |this, _, cx| this.sync_rows(cx)),
            cx.subscribe_in(&composer, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { shift: false, .. } => this.send(None, window, cx),
                InputEvent::Change => cx.notify(),
                _ => {}
            }),
        ];
        // The chosen thumbnail is the Publish tab's.
        if let Some(panel) = panel.upgrade() {
            subscriptions.push(cx.observe(&panel, |_, _, cx| cx.notify()));
        }
        let mut view = Self {
            video_id,
            assistant,
            panel,
            has_video,
            use_frames: has_video,
            composer,
            scroller: cx.new(|cx| MessageScrollerState::new(0, cx)),
            rows: Vec::new(),
            rem_size: window.rem_size(),
            error: None,
            _subscriptions: subscriptions,
        };
        view.sync_rows(cx);
        view
    }

    fn working(&self, cx: &App) -> bool {
        matches!(self.assistant.read(cx).thumbnails(self.video_id), Some(ThumbnailJob::Working { .. }))
    }

    /// The conversation as the assistant has it now.
    fn current_rows(&self, cx: &App) -> Vec<Row> {
        let assistant = self.assistant.read(cx);
        let Some(chat) = assistant.thumbnail_chat(self.video_id) else { return Vec::new() };
        let mut rows: Vec<Row> = chat.messages.iter().cloned().map(Row::Message).collect();
        match assistant.thumbnails(self.video_id) {
            Some(ThumbnailJob::Working { activity }) => rows.push(Row::Working {
                activity: activity.clone(),
                cancelling: assistant.is_cancelling_thumbnails(self.video_id),
            }),
            _ if chat.unanswered() => rows.push(Row::Unanswered),
            _ => {}
        }
        rows
    }

    /// Brings the list up to date, keeping the rows that didn't change measured.
    fn sync_rows(&mut self, cx: &mut Context<Self>) {
        let rows = self.current_rows(cx);
        if rows == self.rows {
            cx.notify();
            return;
        }
        let same = self.rows.iter().zip(&rows).take_while(|(old, new)| old == new).count();
        let (old_len, new_len) = (self.rows.len(), rows.len());
        self.rows = rows;
        self.scroller.update(cx, |scroller, cx| {
            if !scroller.splice(same..old_len, new_len - same, cx) {
                scroller.reset(new_len, cx);
            }
        });
        cx.notify();
    }

    /// Sends what the composer holds, or `text`, to Codex.
    fn send(&mut self, text: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        let message = match text {
            Some(text) => text.to_owned(),
            None => self.composer.read(cx).value().trim().to_owned(),
        };
        if message.is_empty() || self.working(cx) {
            return;
        }
        let (video_id, use_frames) = (self.video_id, self.use_frames && self.has_video);
        let sent = self
            .assistant
            .update(cx, |assistant, cx| assistant.send_thumbnail_message(video_id, message, use_frames, cx));
        match sent {
            Ok(()) => {
                self.error = None;
                // The next message is typed while Codex answers this one.
                self.composer.update(cx, |input, cx| {
                    if text.is_none() {
                        input.set_value("", window, cx);
                    }
                    input.focus(window, cx);
                });
                self.scroller.update(cx, |scroller, cx| scroller.scroll_to_end(cx));
            }
            Err(error) => self.error = Some(error.to_string().into()),
        }
        cx.notify();
    }

    fn choose(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(panel) = self.panel.upgrade() {
            panel.update(cx, |panel, cx| panel.set_thumbnail(Some(path), window, cx));
        }
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        let video_id = self.video_id;
        self.assistant.update(cx, |assistant, cx| assistant.cancel_thumbnails(video_id, cx));
    }

    /// The sheet's title, with the command that starts over.
    fn render_title(&self, cx: &App) -> AnyElement {
        let (assistant, composer, video_id) = (self.assistant.clone(), self.composer.clone(), self.video_id);
        let empty = self.assistant.read(cx).thumbnail_chat(video_id).is_none();
        let working = self.working(cx);
        h_flex()
            .flex_1()
            .min_w_0()
            .justify_between()
            .gap_2()
            .child("Thumbnail chat")
            .child(state_tooltip(
                "new-thumbnail-chat-reason",
                working.then(|| "Wait for Codex to answer, or cancel it.".into()),
                Button::new("new-thumbnail-chat")
                    .label("New chat…")
                    .icon(IconName::Plus)
                    .small()
                    .ghost()
                    .disabled(empty || working)
                    .on_click(move |_, window, cx| {
                        confirm_new_chat(assistant.clone(), composer.clone(), video_id, window, cx)
                    }),
            ))
            .into_any_element()
    }

    fn render_empty(&self, cx: &mut Context<Self>) -> AnyElement {
        EmptyState::new(
            "thumbnail-chat-empty",
            IconName::Sparkles,
            "Thumbnails with Codex",
            "Describe the picture, the words on it and the mood. Codex draws from the video’s title and script, \
             and remembers the chat, so you can ask for changes, more ideas or its opinion.",
        )
        .action(
            Button::new("thumbnail-chat-ideas")
                .label("Draw 3 ideas")
                .small()
                .outline()
                .on_click(cx.listener(|this, _, window, cx| this.send(Some(IDEAS_MESSAGE), window, cx))),
        )
        .into_any_element()
    }

    fn render_messages(&self, cx: &mut Context<Self>) -> AnyElement {
        let context = Rc::new(RowContext {
            view: cx.entity().downgrade(),
            rows: self.rows.clone(),
            chosen: self.panel.upgrade().and_then(|panel| panel.read(cx).chosen_thumbnail().map(PathBuf::from)),
        });
        MessageScroller::new("thumbnail-chat-messages", self.scroller.clone(), move |ix, _, cx| {
            render_row(&context, ix, cx)
        })
        .with_bottom_fade(cx.theme().background)
        .with_row_style(StyleRefinement::default().px_4())
        .into_any_element()
    }

    fn render_composer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let assistant = self.assistant.read(cx);
        let started = assistant.thumbnail_chat(self.video_id).is_some_and(|chat| chat.session_id.is_some());
        let working = self.working(cx);
        let empty = self.composer.read(cx).value().trim().is_empty();
        let theme = cx.theme();
        // Frames go with a chat's first message, when there is a video to take them from.
        let leading = if started {
            Some(help_text("Codex remembers this chat.", cx).into_any_element())
        } else {
            self.has_video.then(|| {
                Checkbox::new("thumbnail-chat-frames")
                    .label("Show Codex frames of the video")
                    .small()
                    .checked(self.use_frames)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.use_frames = *checked;
                        cx.notify();
                    }))
                    .into_any_element()
            })
        };
        let send = Button::new("thumbnail-chat-send")
            .label("Send")
            .small()
            .primary()
            .disabled(working || empty)
            .on_click(cx.listener(|this, _, window, cx| this.send(None, window, cx)));
        v_flex()
            .flex_none()
            .gap_2()
            .px_4()
            .py_3()
            .border_t_1()
            .border_color(theme.border)
            .child(Textarea::new(&self.composer).aria_label("Message to Codex"))
            .when_some(self.error.clone(), |this, error| {
                this.child(div().text_xs().text_color(theme.danger).child(error))
            })
            .child(h_flex().justify_between().gap_2().child(div().min_w_0().children(leading)).child(state_tooltip(
                "thumbnail-chat-send-reason",
                working.then(|| "Codex is still answering.".into()),
                send,
            )))
    }
}

impl Render for ThumbnailChatView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if window.rem_size() != self.rem_size {
            self.rem_size = window.rem_size();
            self.scroller.update(cx, |scroller, cx| scroller.remeasure(cx));
        }
        let body = if self.rows.is_empty() { self.render_empty(cx) } else { self.render_messages(cx) };
        v_flex()
            .id("thumbnail-chat")
            .size_full()
            .child(div().flex_1().min_h_0().child(body))
            .child(self.render_composer(cx))
    }
}

fn render_row(context: &RowContext, ix: usize, cx: &mut App) -> AnyElement {
    match &context.rows[ix] {
        Row::Message(message) if message.role == ChatRole::Creator => Message::new()
            .id(message_id(message))
            .alignment(MessageAlignment::End)
            .content(
                MessageContent::new()
                    .bubble(Bubble::new().with_variant(BubbleVariant::Secondary).child(message.text.clone())),
            )
            .into_any_element(),
        Row::Message(message) => render_answer(context, message, cx),
        Row::Working { activity, cancelling } => {
            let view = context.view.clone();
            let theme = cx.theme();
            h_flex()
                .id("thumbnail-chat-working")
                .gap_2()
                .items_start()
                .child(div().pt_0p5().child(Spinner::new().small().color(theme.muted_foreground)))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(if *cancelling {
                            "Stopping…"
                        } else {
                            "Codex is working…"
                        }))
                        .child(help_text(
                            activity.clone().unwrap_or_else(|| "Drawing takes a few minutes.".into()),
                            cx,
                        )),
                )
                .child(
                    Button::new("thumbnail-chat-cancel")
                        .label("Cancel")
                        .small()
                        .ghost()
                        .disabled(*cancelling)
                        .on_click(move |_, _, cx| {
                            view.update(cx, |view, cx| view.cancel(cx)).ok();
                        }),
                )
                .into_any_element()
        }
        Row::Unanswered => help_text("Codex stopped before it answered.", cx).into_any_element(),
    }
}

/// Codex's answer: its words, the thumbnails it drew and why it stopped early.
fn render_answer(context: &RowContext, message: &ChatMessage, cx: &mut App) -> AnyElement {
    let theme = cx.theme();
    let tiles = message.images.iter().map(|path| {
        let view = context.view.clone();
        let target = path.clone();
        let name = file_name(path);
        v_flex()
            .gap_1()
            .child(
                thumbnail_tile(
                    SharedString::from(format!("chat-{name}")),
                    path,
                    context.chosen.as_ref() == Some(path),
                    cx,
                )
                .tooltip(format!("Use {name}"))
                .on_click(move |_, window, cx| {
                    view.update(cx, |view, cx| view.choose(target.clone(), window, cx)).ok();
                }),
            )
            .child(help_text(name, cx))
            .into_any_element()
    });
    let problem = message.problem.clone().map(|problem| {
        let (icon, color, title) = if message.images.is_empty() {
            (IconName::CircleAlert, theme.danger, "Codex couldn’t answer")
        } else {
            (IconName::TriangleAlert, theme.warning, "Codex stopped early")
        };
        status_line(icon, color, title, Some(problem.into()), cx)
    });
    let text = message.text.trim();
    Message::new()
        .id(message_id(message))
        .alignment(MessageAlignment::Start)
        .content(
            MessageContent::new()
                .when(!text.is_empty(), |this| {
                    this.bubble(
                        Bubble::new().with_variant(BubbleVariant::Ghost).child(
                            TextView::markdown(ElementId::from(("chat-text", message.id as usize)), text.to_owned())
                                .selectable(true),
                        ),
                    )
                })
                .when(!message.images.is_empty(), |this| {
                    this.child(div().w_full().grid().grid_cols(2).gap_2().children(tiles))
                })
                .children(problem),
        )
        .into_any_element()
}

fn message_id(message: &ChatMessage) -> ElementId {
    ElementId::from(("chat-message", message.id as usize))
}

/// Asks before forgetting the chat: Codex can't be reminded of it. The new chat
/// starts in the composer.
fn confirm_new_chat(
    assistant: Entity<PublishAssistant>,
    composer: Entity<TextareaState>,
    video_id: VideoId,
    window: &mut Window,
    cx: &mut App,
) {
    window.open_alert_dialog(cx, move |dialog, _, _| {
        let (assistant, composer) = (assistant.clone(), composer.clone());
        dialog
            .title("Start a new chat?")
            .description("Codex forgets this chat and what it drew in it. The thumbnails stay in the folder.")
            .confirm()
            .ok_text("New chat")
            .ok_variant(ButtonVariant::Danger)
            .on_ok(move |_, window, cx| {
                assistant.update(cx, |assistant, cx| assistant.new_thumbnail_chat(video_id, cx));
                // After the dialog gave focus back to the button, which the empty chat disables.
                let composer = composer.clone();
                window.defer(cx, move |window, cx| composer.update(cx, |input, cx| input.focus(window, cx)));
                true
            })
    });
}
