//! Dialog footers and the confirmations the app shares.

use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::dialog::{Cancel, Confirm, DialogFooter};
use gpui_kit::{
    Action, App, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce, SharedString,
    Styled as _, Window, div,
};
use videocrm_core::VideoId;

/// The footer of a form dialog: Cancel and the commit at their natural width on
/// the trailing edge, like the kit's alert dialogs.
///
/// Cancel dismisses the dialog; the commit confirms it exactly as Enter does,
/// so the dialog's `on_ok` stays the one place that commits.
pub fn form_footer(
    cancel_id: impl Into<ElementId>,
    commit_id: impl Into<ElementId>,
    commit: impl Into<SharedString>,
) -> DialogFooter {
    DialogFooter::new()
        .child(FooterButton {
            anchor: "form-footer-cancel",
            button: Button::new(cancel_id).label("Cancel"),
            action: Rc::new(Cancel),
        })
        .child(FooterButton {
            anchor: "form-footer-commit",
            button: Button::new(commit_id).primary().label(commit),
            action: Rc::new(Confirm { secondary: false }),
        })
}

/// A footer button that dispatches `action` on the dialog it sits in.
///
/// Kit buttons don't take focus when clicked, so an action dispatched from
/// the focused element misses the dialog once nothing in it has focus (its
/// focused field was removed, say). Like the kit's own dialog buttons, this
/// one dispatches from a focus node inside itself, whose ancestors end at
/// its dialog.
#[derive(IntoElement)]
struct FooterButton {
    /// Distinct per button: both sit in one element scope.
    anchor: &'static str,
    button: Button,
    action: Rc<dyn Action>,
}

impl RenderOnce for FooterButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let anchor = window.use_keyed_state(self.anchor, cx, |_, cx| cx.focus_handle()).read(cx).clone();
        let action = self.action;
        self.button
            // Never hovered, so it takes no focus and stays out of the Tab order.
            .child(div().absolute().size_0().track_focus(&anchor))
            .on_click(move |_, window, cx| {
                if anchor.contains(&anchor, window) {
                    anchor.dispatch_action(&*action, window, cx);
                } else {
                    window.dispatch_action(action.boxed_clone(), cx);
                }
            })
    }
}

/// Asks before deleting `videos` (id and title) and calls `delete` once the
/// creator confirms: the one confirmation for deleting videos, wherever the
/// command starts.
pub fn confirm_delete_videos(
    videos: Vec<(VideoId, String)>,
    window: &mut Window,
    cx: &mut App,
    delete: impl Fn(Vec<VideoId>, &mut App) + 'static,
) {
    let ids: Vec<VideoId> = videos.iter().map(|(id, _)| *id).collect();
    let titles: Vec<&str> = videos.iter().map(|(_, title)| title.as_str()).collect();
    let (title, description) = delete_videos_copy(&titles, crate::publish::delete_upload_notice(&ids, cx));
    let delete = std::rc::Rc::new(delete);
    window.open_alert_dialog(cx, move |alert, _, _| {
        let (ids, delete) = (ids.clone(), delete.clone());
        alert
            .title(title.clone())
            .description(description.clone())
            .confirm()
            .ok_text("Delete")
            .ok_variant(ButtonVariant::Danger)
            .on_ok(move |_, _, cx| {
                delete(ids.clone(), cx);
                true
            })
    });
}

/// Title and description of the delete confirmation for videos with `titles`:
/// the object, what goes and what stays, and that it can't be undone.
fn delete_videos_copy(titles: &[&str], upload_notice: Option<&str>) -> (SharedString, SharedString) {
    let (title, consequence) = match titles {
        [name] => (
            format!("Delete “{}”?", display_title(name)),
            "Its script, notes and edit history are deleted. Files in its media folder stay on disk. \
             This can’t be undone.",
        ),
        titles => (
            format!("Delete {} videos?", titles.len()),
            "Their scripts, notes and edit history are deleted. Files in their media folders stay on disk. \
             This can’t be undone.",
        ),
    };
    let description = match upload_notice {
        Some(notice) => format!("{notice} {consequence}"),
        None => consequence.to_owned(),
    };
    (title.into(), description.into())
}

/// A video title for confirmation copy, with a placeholder for untitled videos.
fn display_title(title: &str) -> &str {
    let title = title.trim();
    if title.is_empty() { "Untitled" } else { title }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deleting_videos_says_what_goes_and_what_stays() {
        let (title, description) = delete_videos_copy(&["Rust ownership"], None);
        assert_eq!(title, "Delete “Rust ownership”?");
        assert_eq!(
            description,
            "Its script, notes and edit history are deleted. Files in its media folder stay on disk. \
             This can’t be undone."
        );
        assert_eq!(delete_videos_copy(&["  "], None).0, "Delete “Untitled”?");

        let (title, description) = delete_videos_copy(&["A", "B"], Some("Some of them are uploading."));
        assert_eq!(title, "Delete 2 videos?");
        assert!(description.starts_with("Some of them are uploading. Their scripts, notes and edit history"));
        assert!(description.ends_with("This can’t be undone."));
    }
}
