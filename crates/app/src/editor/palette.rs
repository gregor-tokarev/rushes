//! Colors of editor surfaces, resolved by role from the active theme.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{App, Hsla};

/// The editor's product roles mapped onto theme tokens in one place, so
/// block renderers never pick palette entries themselves.
#[derive(Clone, Copy, Debug)]
pub struct EditorPalette {
    pub text: Hsla,
    pub muted: Hsla,
    pub placeholder: Hsla,
    pub link: Hsla,
    /// Background of a selected divider or a block whose menu is open.
    pub selected: Hsla,
    pub rule: Hsla,
    pub drop_indicator: Hsla,
    /// Fill behind code blocks, inline code and callouts.
    pub surface: Hsla,
    pub quote_bar: Hsla,
    /// Accent of B-roll cues; the cue's fill is a tint of it.
    pub broll: Hsla,
    /// Accent of on-screen text cues.
    pub onscreen: Hsla,
}

impl EditorPalette {
    pub fn new(cx: &App) -> Self {
        let theme = cx.theme();
        Self {
            text: theme.foreground,
            muted: theme.muted_foreground,
            placeholder: theme.muted_foreground.opacity(0.6),
            link: theme.link,
            selected: theme.selection,
            rule: theme.border,
            drop_indicator: theme.drag_border,
            surface: theme.muted,
            quote_bar: theme.foreground,
            broll: theme.blue,
            onscreen: theme.magenta,
        }
    }

    /// Fill for a cue box with the given accent.
    pub fn tint(accent: Hsla) -> Hsla {
        accent.opacity(0.1)
    }
}
