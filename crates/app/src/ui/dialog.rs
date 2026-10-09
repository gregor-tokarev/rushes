//! Dialog footers and the confirmations the app shares.

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::dialog::{Cancel, Confirm, DialogFooter};
use gpui_kit::{App, ElementId, ParentElement as _, SharedString, Window};
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
        .child(
            Button::new(cancel_id)
                .label("Cancel")
                .on_click(|_, window, cx| window.dispatch_action(Box::new(Cancel), cx)),
        )
        .child(
            Button::new(commit_id)
                .primary()
                .label(commit)
                .on_click(|_, window, cx| window.dispatch_action(Box::new(Confirm { secondary: false }), cx)),
        )
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
    let (title, consequence): (SharedString, &str) = match videos.as_slice() {
        [(_, name)] => (
            format!("Delete “{}”?", display_title(name)).into(),
            "Its script, notes and edit history are deleted. Files in its media folder stay on disk. \
             This can’t be undone.",
        ),
        videos => (
            format!("Delete {} videos?", videos.len()).into(),
            "Their scripts, notes and edit history are deleted. Files in their media folders stay on disk. \
             This can’t be undone.",
        ),
    };
    let description: SharedString = match crate::publish::delete_upload_notice(&ids, cx) {
        Some(notice) => format!("{notice} {consequence}").into(),
        None => consequence.into(),
    };
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

/// A video title for confirmation copy, with a placeholder for untitled videos.
fn display_title(title: &str) -> &str {
    let title = title.trim();
    if title.is_empty() { "Untitled" } else { title }
}
