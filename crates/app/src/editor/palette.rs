//! Colors of editor surfaces, resolved by role from the active theme.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{App, Hsla};

use crate::ui::contrast::readable_on;

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
    pub talking_point: CueColors,
    pub broll: CueColors,
    pub onscreen: CueColors,
}

/// Colors of a boxed script block (talking point, B-roll or on-screen cue),
/// from one accent.
#[derive(Clone, Copy, Debug)]
pub struct CueColors {
    /// The box: a tint of the accent over the page.
    pub fill: Hsla,
    /// The label's text and icon: the accent, made readable on the fill.
    pub label: Hsla,
}

impl CueColors {
    fn new(accent: Hsla, page: Hsla) -> Self {
        let fill = accent.opacity(0.1);
        Self { fill, label: readable_on(accent, page.blend(fill)) }
    }
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
            talking_point: CueColors::new(theme.green, theme.background),
            broll: CueColors::new(theme.blue, theme.background),
            onscreen: CueColors::new(theme.magenta, theme.background),
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::TestAppContext;
    use gpui_kit::component::{Theme, ThemeMode};

    use super::*;
    use crate::ui::contrast::{TEXT_CONTRAST, contrast_ratio};

    #[gpui_kit::test]
    fn cue_labels_read_on_their_fill_in_both_themes(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            cx.update(|cx| {
                Theme::change(mode, None, cx);
                let palette = EditorPalette::new(cx);
                for cue in [palette.talking_point, palette.broll, palette.onscreen] {
                    let behind = cx.theme().background.blend(cue.fill);
                    assert!(contrast_ratio(cue.label, behind) >= TEXT_CONTRAST, "{mode:?} {cue:?}");
                }
            });
        }
    }
}
