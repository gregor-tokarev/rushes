//! Small presentation pieces of the shell.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::kbd::Kbd;
use gpui_kit::{Action, AnyElement, App, IntoElement, ParentElement as _, Styled as _, Window, div};

/// The key binding of `action` as muted text, for controls that show their shortcut.
pub(crate) fn shortcut_hint(action: &dyn Action, window: &Window, cx: &App) -> Option<AnyElement> {
    let kbd = Kbd::binding_for_action(action, None, window)?;
    Some(div().text_xs().text_color(cx.theme().muted_foreground).child(kbd.appearance(false)).into_any_element())
}
