//! The interface zoom. GPUI Kit makes the theme's base font the window's
//! `rem`, so the zoom sets that font to the design's size times the creator's
//! [`Zoom`], and everything drawn in rems scales with it. Geometry the kit
//! takes in pixels (table rows and columns, the title bar, the sidebar,
//! notifications) goes through [`zoomed`]; tab bars take a [`tab_size`].

use gpui_kit::component::notification::NotificationSettings;
use gpui_kit::component::tab::TabVariant;
use gpui_kit::component::{ActiveTheme as _, Size, TITLE_BAR_HEIGHT, Theme};
use gpui_kit::{App, Pixels, px};
use rushes_core::settings::Zoom;

/// The base font the interface is designed at: the `rem` at 100%.
pub const BASE_FONT_SIZE: Pixels = px(16.);
/// The monospace font at 100%.
const BASE_MONO_FONT_SIZE: Pixels = px(13.);

/// Draws every window at `zoom`.
pub fn apply_zoom(zoom: Zoom, cx: &mut App) {
    let scale = zoom.factor();
    let (font_size, mono_font_size) = (BASE_FONT_SIZE * scale, BASE_MONO_FONT_SIZE * scale);
    if cx.theme().font_size == font_size && cx.theme().mono_font_size == mono_font_size {
        return;
    }
    Theme::update(cx, |theme| {
        theme.font_size = font_size;
        theme.mono_font_size = mono_font_size;
        // Notifications keep room for their larger text and stay below the taller title bar.
        let notifications = NotificationSettings::default();
        let margin = notifications.margins.right * scale;
        theme.notification.width = notifications.width * scale;
        theme.notification.margins.top = title_bar_height_at(scale) + margin;
        theme.notification.margins.right = margin;
        theme.notification.margins.bottom = margin;
        theme.notification.margins.left = margin;
    });
}

/// The window's title bar height. Never shorter than the kit's: macOS places
/// the window buttons for that height.
pub fn title_bar_height(cx: &App) -> Pixels {
    title_bar_height_at(scale(cx))
}

fn title_bar_height_at(scale: f32) -> Pixels {
    TITLE_BAR_HEIGHT * scale.max(1.)
}

/// How much larger than designed the interface is drawn: 1.0 at 100%.
pub fn scale(cx: &App) -> f32 {
    cx.theme().font_size / BASE_FONT_SIZE
}

/// `pixels` of the design at the current zoom.
pub fn zoomed(pixels: Pixels, cx: &App) -> Pixels {
    pixels * scale(cx)
}

/// The size for a bar of `variant` tabs designed at `size`. The kit draws a
/// tab's label in rems inside a frame of fixed pixels, so a zoomed label
/// outgrows its frame: this is the first size from `size` up whose frame holds it.
pub fn tab_size(variant: TabVariant, size: Size, cx: &App) -> Size {
    let rem = cx.theme().font_size;
    let sizes = [Size::XSmall, Size::Small, Size::Medium, Size::Large];
    let start = sizes.iter().position(|known| *known == size).unwrap_or(1);
    sizes[start..].iter().copied().find(|size| label_fits(variant, *size, rem)).unwrap_or(Size::Large)
}

/// Whether a tab's label at `rem` fits the frame the kit gives it at `size`.
fn label_fits(variant: TabVariant, size: Size, rem: Pixels) -> bool {
    // The kit's label sizes and inner frame heights (`TabVariant::inner_height`).
    let (label, frame) = match (size, variant) {
        (Size::XSmall, TabVariant::Segmented) => (0.75, 16.),
        (Size::XSmall, TabVariant::Underline) => (0.75, 20.),
        (Size::XSmall, _) => (0.75, 18.),
        (Size::Small, TabVariant::Segmented) => (0.875, 18.),
        (Size::Small, _) => (0.875, 22.),
        (Size::Large, TabVariant::Segmented) => (1., 28.),
        (Size::Large, TabVariant::Underline) => (1., 32.),
        (Size::Large, _) => (1., 36.),
        (_, TabVariant::Segmented) => (0.875, 24.),
        (_, TabVariant::Underline) => (0.875, 26.),
        (_, TabVariant::Tab) => (0.875, 30.),
        (_, _) => (0.875, 26.),
    };
    // Inter's ink, ascenders to descenders, stays within 9/8 of its size.
    rem * label * 1.125 <= px(frame)
}

#[cfg(test)]
mod tests {
    use gpui_kit::TestAppContext;

    use super::*;

    #[gpui_kit::test]
    fn zoom_scales_the_base_fonts(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            assert_eq!(cx.theme().font_size, BASE_FONT_SIZE, "the kit's default is the design's size");
            apply_zoom(Zoom::new(150), cx);
            assert_eq!(cx.theme().font_size, px(24.));
            assert_eq!(cx.theme().mono_font_size, px(19.5));
            assert_eq!(scale(cx), 1.5);
            assert_eq!(zoomed(px(32.), cx), px(48.));
            assert_eq!(title_bar_height(cx), px(51.));
            assert_eq!(cx.theme().notification.width, px(573.));
            assert_eq!(cx.theme().notification.margins.top, px(51. + 24.), "below the title bar");
            apply_zoom(Zoom::new(75), cx);
            assert_eq!(title_bar_height(cx), TITLE_BAR_HEIGHT, "never shorter than the kit's");
            apply_zoom(Zoom::DEFAULT, cx);
            assert_eq!(cx.theme().font_size, BASE_FONT_SIZE);
            assert_eq!(scale(cx), 1.);
            let kit = NotificationSettings::default();
            assert_eq!(tab_size(TabVariant::Segmented, Size::Small, cx), Size::Small);
            assert_eq!(cx.theme().notification.width, kit.width, "100% is the kit's own");
            assert_eq!(cx.theme().notification.margins, kit.margins);
        });
    }

    #[gpui_kit::test]
    fn tabs_step_up_until_their_label_fits(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            let sizes = |cx: &App| {
                [
                    tab_size(TabVariant::Segmented, Size::Small, cx),
                    tab_size(TabVariant::Underline, Size::Small, cx),
                    tab_size(TabVariant::Underline, Size::Medium, cx),
                ]
            };
            for (percent, expected) in [
                (75, [Size::Small, Size::Small, Size::Medium]),
                (100, [Size::Small, Size::Small, Size::Medium]),
                (110, [Size::Small, Size::Small, Size::Medium]),
                (125, [Size::Medium, Size::Small, Size::Medium]),
                (150, [Size::Medium, Size::Medium, Size::Medium]),
            ] {
                apply_zoom(Zoom::new(percent), cx);
                assert_eq!(sizes(cx), expected, "{percent}%");
            }
        });
    }
}
