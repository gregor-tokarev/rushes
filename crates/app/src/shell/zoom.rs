//! Zooming from the keyboard: Ctrl (⌘ on macOS) with + or - changes the zoom
//! setting a level at a time and Ctrl+0 resets it, wherever focus is. The
//! workspace draws the new level when the setting changes; a notification
//! names it, since nothing on screen does.

use gpui_kit::component::kbd::Kbd;
use gpui_kit::{App, Keystroke};
use rushes_core::settings::Zoom;

use crate::state::AppState;
use crate::ui;

/// The zoom-in key as shortcut hints show it: the key with = and + on it is
/// bound with and without Shift.
pub(super) const ZOOM_IN_KEY: &str = "secondary-+";
pub(super) const ZOOM_OUT_KEY: &str = "secondary--";
pub(super) const RESET_ZOOM_KEY: &str = "secondary-0";

/// Sets the zoom to `change` of the current one and says what it is now.
pub(super) fn change_zoom(change: impl FnOnce(Zoom) -> Zoom, cx: &mut App) {
    // The window saying the app can't open its data has no settings to zoom.
    let Some(state) = AppState::try_global(cx) else { return };
    let before = state.read(cx).settings().zoom;
    let zoom = change(before);
    state.update(cx, |state, cx| state.update_settings(cx, |settings| settings.zoom = zoom));
    // What was saved: a setting that can't be saved stays as it was, with its own notification.
    let now = state.read(cx).settings().zoom;
    if now != zoom {
        return;
    }
    let message = zoom_message(before, now);
    // The window is busy dispatching the key that got here.
    cx.defer(move |cx| {
        ui::push_notification(cx, |cx| ui::notification::info(message, cx).id::<Zoom>());
    });
}

/// `Zoom 125%. Ctrl+0 resets it.`
fn zoom_message(before: Zoom, now: Zoom) -> String {
    let label = now.label();
    if now == Zoom::DEFAULT {
        format!("Zoom {label}.")
    } else if now == before && now.zoom_in() == now {
        format!("Zoom {label} is the largest.")
    } else if now == before && now.zoom_out() == now {
        format!("Zoom {label} is the smallest.")
    } else {
        format!("Zoom {label}. {} resets it.", shortcut(RESET_ZOOM_KEY))
    }
}

/// How the zoom shortcuts read on this platform, for Settings: `Ctrl++ and
/// Ctrl+- zoom in and out; Ctrl+0 resets.`
pub fn zoom_shortcuts() -> String {
    format!(
        "{} and {} zoom in and out; {} resets.",
        shortcut(ZOOM_IN_KEY),
        shortcut(ZOOM_OUT_KEY),
        shortcut(RESET_ZOOM_KEY)
    )
}

fn shortcut(key: &str) -> String {
    Keystroke::parse(key).map(|keystroke| Kbd::format(&keystroke)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_name_the_level() {
        let reset = shortcut(RESET_ZOOM_KEY);
        assert_eq!(zoom_message(Zoom::DEFAULT, Zoom::new(110)), format!("Zoom 110%. {reset} resets it."));
        assert_eq!(zoom_message(Zoom::new(110), Zoom::DEFAULT), "Zoom 100%.");
        assert_eq!(zoom_message(Zoom::new(150), Zoom::new(175)), format!("Zoom 175%. {reset} resets it."));
        assert_eq!(zoom_message(Zoom::new(250), Zoom::new(250)), "Zoom 250% is the largest.");
        assert_eq!(zoom_message(Zoom::new(75), Zoom::new(75)), "Zoom 75% is the smallest.");
    }

    #[test]
    fn shortcuts_read_as_the_platform_writes_them() {
        if cfg!(target_os = "macos") {
            assert_eq!(zoom_shortcuts(), "⌘+ and ⌘- zoom in and out; ⌘0 resets.");
        } else {
            assert_eq!(zoom_shortcuts(), "Ctrl++ and Ctrl+- zoom in and out; Ctrl+0 resets.");
        }
    }
}
