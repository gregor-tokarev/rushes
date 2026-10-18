//! The dialog that asks the agent for a video's details: what it works from, and
//! the creator's own words for it.

use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::{WindowExt as _, v_flex};
use gpui_kit::{App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _, Window};
use rushes_core::VideoId;

use super::assistant::{DetailSources, PublishAssistant};
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
