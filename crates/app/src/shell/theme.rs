//! Appearance: the embedded UI font, the light / dark / system theme mode and
//! the few colours VideoCRM sets on top of the kit's theme.

use std::borrow::Cow;

use gpui_kit::component::{Colorize as _, Theme, ThemeMode as KitThemeMode};
use gpui_kit::{App, Window};
use videocrm_core::settings::ThemeMode;

use crate::ui::contrast::readable_on;

/// Family of the embedded UI font.
const UI_FONT: &str = "Inter";

/// Inter (SIL Open Font License, `assets/fonts/OFL.txt`) in the weights the UI
/// uses. Embedded because the platform UI font may be a variable font whose
/// bold weights GPUI renders as regular.
const UI_FONT_FILES: [&[u8]; 5] = [
    include_bytes!("../../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../../assets/fonts/Inter-Medium.ttf"),
    include_bytes!("../../assets/fonts/Inter-SemiBold.ttf"),
    include_bytes!("../../assets/fonts/Inter-Bold.ttf"),
    include_bytes!("../../assets/fonts/Inter-Italic.ttf"),
];

/// Registers the embedded UI font with the text system. On failure the theme
/// keeps the platform font.
pub fn register_fonts(cx: &mut App) {
    let fonts = UI_FONT_FILES.iter().map(|bytes| Cow::Borrowed(*bytes)).collect();
    if let Err(error) = cx.text_system().add_fonts(fonts) {
        eprintln!("videocrm: cannot load the embedded UI font: {error}");
    }
}

/// Applies `mode` to the global theme and keeps the UI font on it. `System`
/// follows the window's appearance (the platform's when there is no window).
pub fn apply_theme(mode: ThemeMode, window: Option<&mut Window>, cx: &mut App) {
    match mode {
        ThemeMode::Light => Theme::change(KitThemeMode::Light, None, cx),
        ThemeMode::Dark => Theme::change(KitThemeMode::Dark, None, cx),
        ThemeMode::System => Theme::sync_system_appearance(window, cx),
    }
    Theme::update(cx, solid_danger_buttons);
    let has_ui_font = cx.text_system().all_font_names().iter().any(|name| name == UI_FONT);
    if has_ui_font && Theme::global(cx).font_family != UI_FONT {
        Theme::update(cx, |theme| theme.font_family = UI_FONT.into());
    }
}

/// Danger buttons commit destructive confirmations (Delete, Delete property…).
/// The kit draws them as a pale tint with danger-coloured text, which reads as
/// disabled in the light theme (about 2.9:1); VideoCRM draws them solid, with
/// light text on a danger fill dark enough to read.
fn solid_danger_buttons(theme: &mut Theme) {
    // Light text in both themes: the background of a light theme, the text of a dark one.
    let light = if theme.background.l > theme.foreground.l { theme.background } else { theme.foreground };
    let fill = readable_on(theme.danger, light);
    theme.colors.button_danger = fill;
    theme.colors.button_danger_foreground = light;
    theme.colors.button_danger_hover = fill.darken(0.1);
    theme.colors.button_danger_active = fill.darken(0.2);
}

#[cfg(test)]
mod tests {
    use gpui_kit::TestAppContext;
    use gpui_kit::component::ActiveTheme as _;

    use super::*;
    use crate::ui::contrast::{TEXT_CONTRAST, contrast_ratio};

    #[gpui_kit::test]
    fn danger_buttons_read_in_both_themes(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            cx.update(|cx| {
                apply_theme(mode, None, cx);
                let theme = cx.theme();
                let text = theme.button_danger_foreground;
                for fill in [theme.button_danger, theme.button_danger_hover, theme.button_danger_active] {
                    assert!(contrast_ratio(text, fill) >= TEXT_CONTRAST, "{mode:?}: {fill:?} under {text:?}");
                    assert_eq!(fill.a, 1., "{mode:?}: a solid fill, not a tint");
                }
                // The token the button paints its fill from follows the colour.
                assert_eq!(theme.tokens.button_danger.color, theme.button_danger);
            });
        }
    }
}
