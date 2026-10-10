//! Keyboard focus on the app's own focusable rows, cards and controls.

use gpui_kit::component::ThemeStyled as _;
use gpui_kit::{App, FocusHandle, InteractiveElement, MouseButton, ParentElement, Styled, Window};

/// The one focus rule for every row, card and control the app makes focusable
/// itself, as the kit's buttons, checkboxes and radios behave: a click doesn't
/// take the keyboard, and the focus ring shows only focus the keyboard gave,
/// as CSS `:focus-visible` does. A ring left by a click makes a row or card
/// look selected until something else takes focus, and a dialog it opened
/// hands focus back to it.
pub trait KeyboardFocus: InteractiveElement + Styled + ParentElement + Sized {
    /// Tracks `focus` on this element under that rule.
    fn keyboard_focus(self, focus: &FocusHandle, window: &Window, cx: &App) -> Self {
        let ring = shows_focus_ring(focus, window);
        let this = self.track_focus(focus).on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default());
        if ring { this.focus_ring_style(window, cx) } else { this }
    }
}

impl<E: InteractiveElement + Styled + ParentElement> KeyboardFocus for E {}

/// Whether an element with `focus` draws its focus ring: it has the focus,
/// and the last input was a key.
fn shows_focus_ring(focus: &FocusHandle, window: &Window) -> bool {
    focus.is_focused(window) && window.last_input_was_keyboard()
}
