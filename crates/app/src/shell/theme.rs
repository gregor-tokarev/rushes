//! Appearance: the embedded UI font and the light / dark / system theme mode.

use std::borrow::Cow;

use gpui_kit::component::{Theme, ThemeMode as KitThemeMode};
use gpui_kit::{App, Window};
use videocrm_core::settings::ThemeMode;

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
    let has_ui_font = cx.text_system().all_font_names().iter().any(|name| name == UI_FONT);
    if has_ui_font && Theme::global(cx).font_family != UI_FONT {
        Theme::update(cx, |theme| theme.font_family = UI_FONT.into());
    }
}
