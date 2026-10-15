//! Notifications in the style of their kind: an info, success, warning or
//! error icon beside the title and message.
//!
//! The kit hangs a notification's icon 18 pixels down while its text grows
//! with the zoom, so a zoomed notification's icon rides above its first line.
//! These hang it in rems instead, on the first line's middle at any zoom, even
//! one that changes while the notification is up.

use gpui_kit::assets::IconName;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, Icon};
use gpui_kit::{App, Hsla, SharedString, Styled as _, px, rems};

/// Where the kit hangs a notification's icon, from the top of its padding.
const KIT_ICON_TOP: f32 = 18.;
/// The notification's top padding and the line height of its `text_sm` first
/// line, a golden ratio of its size, in rems.
const FIRST_LINE_TOP: f32 = 0.875;
const FIRST_LINE_HEIGHT: f32 = 0.875 * 1.618_034;
/// The icon's size in rems: the size of the notification's text.
const ICON_SIZE: f32 = 1.;

pub fn info(message: impl Into<SharedString>, cx: &App) -> Notification {
    with_icon(message, IconName::Info, cx.theme().info)
}

pub fn success(message: impl Into<SharedString>, cx: &App) -> Notification {
    with_icon(message, IconName::CircleCheck, cx.theme().success)
}

pub fn warning(message: impl Into<SharedString>, cx: &App) -> Notification {
    with_icon(message, IconName::TriangleAlert, cx.theme().warning)
}

pub fn error(message: impl Into<SharedString>, cx: &App) -> Notification {
    with_icon(message, IconName::CircleX, cx.theme().danger)
}

/// A notification of `message` with `icon` in `color`, the kit's for its kind.
fn with_icon(message: impl Into<SharedString>, icon: IconName, color: Hsla) -> Notification {
    let top = FIRST_LINE_TOP + (FIRST_LINE_HEIGHT - ICON_SIZE) / 2.;
    let icon = Icon::new(icon)
        .text_color(color)
        .size(rems(ICON_SIZE))
        // Take back the kit's pixels and hang the icon in rems.
        .mt(px(-KIT_ICON_TOP))
        .relative()
        .top(rems(top));
    Notification::new().message(message).icon(icon)
}
