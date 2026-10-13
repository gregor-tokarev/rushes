//! Appearance: the embedded UI font, the light / dark / system theme mode and
//! the few colours Rushes sets on top of the kit's theme.

use std::borrow::Cow;

use gpui_kit::component::{Colorize as _, Theme, ThemeMode as KitThemeMode};
use gpui_kit::{App, Window};
use rushes_core::settings::ThemeMode;

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
        log_line!("rushes: cannot load the embedded UI font: {error}");
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
    Theme::update(cx, |theme| {
        readable_danger_text(theme);
        solid_danger_buttons(theme);
    });
    let has_ui_font = cx.text_system().all_font_names().iter().any(|name| name == UI_FONT);
    if has_ui_font && Theme::global(cx).font_family != UI_FONT {
        Theme::update(cx, |theme| theme.font_family = UI_FONT.into());
    }
}

/// The danger colour is made for fills and icons, yet it is also the colour of
/// inline errors and of a destructive outline button's label (Replace in the
/// Markdown import). It is made dark enough (light enough in the dark theme)
/// to read on the window, on dialogs and menus, and on the tint of a hovered
/// outline button.
fn readable_danger_text(theme: &mut Theme) {
    let mut danger = theme.danger;
    // The tint is the colour itself: once it changes, the tint does too.
    for _ in 0..10 {
        let before = danger;
        for surface in [theme.background, theme.popover] {
            for behind in [surface, surface.blend(danger.opacity(0.2))] {
                danger = readable_on(danger, behind);
            }
        }
        if danger == before {
            break;
        }
    }
    theme.colors.danger = danger;
}

/// Danger buttons commit destructive confirmations (Delete, Delete property…).
/// The kit draws them as a pale tint with danger-coloured text, which reads as
/// disabled in the light theme (about 2.9:1); Rushes draws them solid, with
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
    fn danger_text_reads_in_both_themes(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            cx.update(|cx| {
                apply_theme(mode, None, cx);
                let theme = cx.theme();
                let text = theme.danger;
                // Inline errors on the window and in dialogs and menus; an outline
                // button's label on its tint, at rest and hovered.
                for surface in [theme.background, theme.popover] {
                    for tint in [0., 0.1, 0.2] {
                        let behind = surface.blend(text.opacity(tint));
                        assert!(contrast_ratio(text, behind) >= TEXT_CONTRAST, "{mode:?}: {text:?} on {behind:?}");
                    }
                }
                assert_eq!(theme.tokens.danger.background, text.into(), "{mode:?}: the token follows the colour");
            });
        }
        // Applying the theme again changes nothing.
        cx.update(|cx| {
            let before = cx.theme().danger;
            apply_theme(ThemeMode::Dark, None, cx);
            assert_eq!(cx.theme().danger, before);
        });
    }

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
