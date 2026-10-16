//! The dialogs that ask the agent for a video's details and Codex for thumbnails:
//! what it works from, and the creator's own words for it.

use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::radio::RadioGroup;
use gpui_kit::component::{Disableable as _, WindowExt as _, v_flex};
use gpui_kit::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, SharedString, Styled as _, Window,
};
use rushes_core::VideoId;
use rushes_pipeline::MAX_THUMBNAILS;

use super::assistant::{DetailSources, PublishAssistant, ThumbnailOptions};
use super::ui::{FieldRow, help_text};
use crate::ui::dialog::form_footer;
use crate::ui::notification;

/// Asks for instructions, then has the agent write the details of `video_id`.
pub(super) fn open_details_dialog(video_id: VideoId, sources: DetailSources, window: &mut Window, cx: &mut App) {
    let Some(assistant) = PublishAssistant::try_global(cx) else { return };
    let last = assistant.read(cx).last_instructions(video_id).to_owned();
    let form = cx.new(|cx| DetailsForm::new(sources, last, window, cx));
    let instructions = form.read(cx).instructions.clone();
    window.open_dialog(cx, move |dialog, window, _| {
        let (form, assistant) = (form.clone(), assistant.clone());
        dialog
            .title("Write title, description and tags")
            .width(window.rem_size() * 32.)
            .child(form.clone())
            .footer(form_footer("write-details-cancel", "write-details-commit", "Write"))
            .on_ok(move |_, window, cx| {
                let instructions = form.read(cx).instructions.read(cx).value().to_string();
                let started = assistant.update(cx, |assistant, cx| assistant.write_details(video_id, instructions, cx));
                if let Err(error) = started {
                    window.push_notification(
                        notification::error(error.to_string(), cx).title("Couldn’t write the details"),
                        cx,
                    );
                }
                true
            })
    });
    instructions.update(cx, |input, cx| input.focus(window, cx));
}

/// The details dialog's content.
struct DetailsForm {
    sources: DetailSources,
    instructions: Entity<TextareaState>,
}

impl DetailsForm {
    fn new(sources: DetailSources, last: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let instructions = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 8)
                .placeholder("Mention the giveaway, keep the title short, write in Spanish…")
                .default_value(last)
        });
        Self { sources, instructions }
    }

    /// What the agent works from, and what happens to what the form holds.
    fn description(&self) -> &'static str {
        match (self.sources.script, self.sources.transcript) {
            (true, true) => {
                "The agent writes them from the script and the transcript of the footage, in place of what \
                 the form has. Links and the chapter list in the description stay. You can undo it."
            }
            (true, false) => {
                "The agent writes them from the script, in place of what the form has. There’s no \
                 transcript yet: an AI edit makes one. Links and the chapter list in the description stay. \
                 You can undo it."
            }
            _ => {
                "The agent writes them from the transcript of the footage, in place of what the form has. \
                 Links and the chapter list in the description stay. You can undo it."
            }
        }
    }
}

impl Render for DetailsForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex().gap_4().child(help_text(self.description(), cx).text_sm()).child(
            FieldRow::new("Instructions")
                .help("Optional. The agent follows them before its own rules.")
                .control(Textarea::new(&self.instructions).aria_label("Instructions")),
        )
    }
}

/// Asks how many thumbnails and which ideas, then has Codex draw them for `video_id`.
/// `has_video` says whether there is a video to show Codex frames of.
pub(super) fn open_thumbnails_dialog(video_id: VideoId, has_video: bool, window: &mut Window, cx: &mut App) {
    let Some(assistant) = PublishAssistant::try_global(cx) else { return };
    let last = assistant.read(cx).last_thumbnail_options(video_id);
    let form = cx.new(|cx| ThumbnailsForm::new(last, has_video, window, cx));
    let ideas = form.read(cx).ideas.clone();
    window.open_dialog(cx, move |dialog, window, _| {
        let (form, assistant) = (form.clone(), assistant.clone());
        dialog
            .title("Generate thumbnails")
            .width(window.rem_size() * 32.)
            .child(form.clone())
            .footer(form_footer("generate-thumbnails-cancel", "generate-thumbnails-commit", "Generate"))
            .on_ok(move |_, window, cx| {
                let options = form.read(cx).options(cx);
                let started =
                    assistant.update(cx, |assistant, cx| assistant.generate_thumbnails(video_id, options, cx));
                if let Err(error) = started {
                    window.push_notification(
                        notification::error(error.to_string(), cx).title("Couldn’t generate thumbnails"),
                        cx,
                    );
                }
                true
            })
    });
    ideas.update(cx, |input, cx| input.focus(window, cx));
}

/// The thumbnails dialog's content.
struct ThumbnailsForm {
    count: usize,
    use_frames: bool,
    has_video: bool,
    ideas: Entity<TextareaState>,
}

impl ThumbnailsForm {
    fn new(last: ThumbnailOptions, has_video: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let ideas = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 8)
                .placeholder("Big text “24 HOURS”, me holding the loaf, warm colors…")
                .default_value(last.ideas)
        });
        Self { count: last.count.clamp(1, MAX_THUMBNAILS), use_frames: last.use_frames, has_video, ideas }
    }

    fn options(&self, cx: &App) -> ThumbnailOptions {
        ThumbnailOptions {
            count: self.count,
            ideas: self.ideas.read(cx).value().to_string(),
            use_frames: self.use_frames && self.has_video,
        }
    }
}

impl Render for ThumbnailsForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let counts: Vec<SharedString> = (1..=MAX_THUMBNAILS).map(|count| count.to_string().into()).collect();
        v_flex()
            .gap_4()
            .child(
                help_text(
                    "Codex draws them from the video’s title and script, as 1280 × 720 JPGs in its \
                     thumbnails folder. It takes a few minutes; pick one when they’re ready.",
                    cx,
                )
                .text_sm(),
            )
            .child(
                FieldRow::new("How many").control(
                    RadioGroup::horizontal("generate-thumbnails-count")
                        .children(counts)
                        .selected_index(Some(self.count - 1))
                        .on_change(cx.listener(|this, ix: &usize, _, cx| {
                            this.count = ix + 1;
                            cx.notify();
                        })),
                ),
            )
            .child(
                FieldRow::new("Ideas")
                    .help("Optional: the picture, the words on it, the mood.")
                    .control(Textarea::new(&self.ideas).aria_label("Ideas")),
            )
            .child(
                v_flex()
                    .gap_1p5()
                    .child(
                        Checkbox::new("generate-thumbnails-frames")
                            .label("Show Codex frames of the video")
                            .checked(self.use_frames && self.has_video)
                            .disabled(!self.has_video)
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.use_frames = *checked;
                                cx.notify();
                            })),
                    )
                    .child(help_text(
                        if self.has_video {
                            "Keeps the people and the set recognisable."
                        } else {
                            "Choose the video file or add footage first."
                        },
                        cx,
                    )),
            )
    }
}
