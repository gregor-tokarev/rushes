//! Status colours that stay legible. The theme's status colours are made for
//! fills and icons; text drawn in one, or on one, needs [`TEXT_CONTRAST`]
//! against what is behind it, in light and dark themes alike.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{App, Hsla};

/// The contrast body text needs against its background (WCAG AA).
pub const TEXT_CONTRAST: f32 = 4.5;

/// Warning text on the window background, such as a hint that something
/// won't work as expected: the theme's warning colour, made dark enough (or
/// light enough) to read.
pub fn warning_text(cx: &App) -> Hsla {
    readable_on(cx.theme().warning, cx.theme().background)
}

/// Fill and text of a warning count on `surface`: a tint of the warning
/// colour and warning text that reads on it.
pub fn warning_badge(surface: Hsla, cx: &App) -> (Hsla, Hsla) {
    let fill = cx.theme().warning.opacity(0.2);
    (fill, readable_on(cx.theme().warning, surface.blend(fill)))
}

/// `color` with its lightness moved away from `background` until the two
/// reach [`TEXT_CONTRAST`]; `color` itself when they already do. Either may
/// be the text: a fill made readable under light text works the same way.
pub fn readable_on(color: Hsla, background: Hsla) -> Hsla {
    let darken = luminance(background) > 0.18;
    let mut readable = Hsla { a: 1., ..color };
    for _ in 0..100 {
        if contrast_ratio(readable, background) >= TEXT_CONTRAST {
            break;
        }
        readable.l = if darken { (readable.l - 0.01).max(0.) } else { (readable.l + 0.01).min(1.) };
    }
    readable
}

/// The WCAG contrast ratio of two opaque colours, from 1 to 21.
pub fn contrast_ratio(a: Hsla, b: Hsla) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// Relative luminance (sRGB).
fn luminance(color: Hsla) -> f32 {
    let rgb = color.to_rgb();
    let linear = |channel: f32| {
        if channel <= 0.040_45 { channel / 12.92 } else { ((channel + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * linear(rgb.r) + 0.7152 * linear(rgb.g) + 0.0722 * linear(rgb.b)
}

#[cfg(test)]
mod tests {
    use gpui_kit::component::{Theme, ThemeMode};
    use gpui_kit::{TestAppContext, rgb};

    use super::*;

    #[test]
    fn contrast_follows_wcag() {
        let (white, black) = (Hsla::from(rgb(0xffffff)), Hsla::from(rgb(0x000000)));
        assert!((contrast_ratio(white, black) - 21.).abs() < 0.01);
        assert!((contrast_ratio(white, white) - 1.).abs() < 0.01);
        // Yellow-500 on white: the hint the QA pass could hardly read.
        assert!(contrast_ratio(rgb(0xeab308).into(), white) < 2.5);
    }

    #[test]
    fn readable_colours_reach_the_text_contrast() {
        let white = Hsla::from(rgb(0xffffff));
        let near_black = Hsla::from(rgb(0x0a0a0a));
        for color in [0xeab308, 0xfacc15, 0xef4444, 0xf87171, 0x22c55e] {
            let color = Hsla::from(rgb(color));
            assert!(contrast_ratio(readable_on(color, white), white) >= TEXT_CONTRAST);
            assert!(contrast_ratio(readable_on(color, near_black), near_black) >= TEXT_CONTRAST);
        }
        // A colour that already reads is left alone.
        assert_eq!(readable_on(near_black, white), near_black);
    }

    #[gpui_kit::test]
    fn warning_text_and_badges_read_in_both_themes(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            cx.update(|cx| {
                Theme::change(mode, None, cx);
                let theme = cx.theme();
                let text = warning_text(cx);
                assert!(contrast_ratio(text, theme.background) >= TEXT_CONTRAST, "{mode:?} warning text");
                for surface in [theme.sidebar, theme.sidebar_accent] {
                    let (fill, text) = warning_badge(surface, cx);
                    let behind = surface.blend(fill);
                    assert!(contrast_ratio(text, behind) >= TEXT_CONTRAST, "{mode:?} warning badge");
                }
            });
        }
    }
}
