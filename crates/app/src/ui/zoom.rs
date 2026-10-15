//! The interface zoom. GPUI Kit makes the theme's base font the window's
//! `rem`, so the zoom sets that font to the design's size times the creator's
//! [`Zoom`], and everything drawn in rems scales with it. Geometry the kit
//! takes in pixels (table rows and columns, the title bar, the sidebar,
//! notifications) goes through [`zoomed`]; tab bars through [`zoomed_tabs`].

use std::panic::Location;

use gpui_kit::component::notification::NotificationSettings;
use gpui_kit::component::tab::{TabBar, TabVariant};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, Size, TITLE_BAR_HEIGHT, Theme};
use gpui_kit::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement, LayoutId, Pixels,
    Window, px,
};
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

/// `tab_bar` as a bar of `variant` tabs designed at `size`, at the current
/// zoom. The kit draws a tab's label in rems inside a frame of fixed pixels,
/// so a zoomed label outgrows its frame: the bar takes the first size from
/// `size` up whose frame holds it. Past the zoom the largest frame holds, the
/// bar, suffix and the menus it opens included, is drawn at the largest rem
/// that frame holds, so its labels stop growing instead of being cut.
pub fn zoomed_tabs(tab_bar: TabBar, variant: TabVariant, size: Size, cx: &App) -> AnyElement {
    let size = tab_size(variant, size, cx);
    let tab_bar = tab_bar.with_variant(variant).with_size(size);
    match tab_rem(variant, size, cx) {
        Some(rem) => AtRem { rem, child: tab_bar.into_any_element() }.into_any_element(),
        None => tab_bar.into_any_element(),
    }
}

/// The first size from `size` up whose frame holds a `variant` tab's label.
fn tab_size(variant: TabVariant, size: Size, cx: &App) -> Size {
    let rem = cx.theme().font_size;
    let sizes = [Size::XSmall, Size::Small, Size::Medium, Size::Large];
    let start = sizes.iter().position(|known| *known == size).unwrap_or(1);
    sizes[start..].iter().copied().find(|size| label_fits(variant, *size, rem)).unwrap_or(Size::Large)
}

/// The rem a bar of `variant` tabs at `size` is drawn at when the zoom's is
/// larger than its frames hold.
fn tab_rem(variant: TabVariant, size: Size, cx: &App) -> Option<Pixels> {
    let (label, frame) = tab_frame(variant, size);
    let largest = px(frame / (label * LABEL_INK));
    (cx.theme().font_size > largest).then_some(largest)
}

/// Whether a tab's label at `rem` fits the frame the kit gives it at `size`.
fn label_fits(variant: TabVariant, size: Size, rem: Pixels) -> bool {
    let (label, frame) = tab_frame(variant, size);
    rem * label * LABEL_INK <= px(frame)
}

/// Inter's ink, ascenders to descenders, stays within 9/8 of its size.
const LABEL_INK: f32 = 1.125;

/// A tab's label size, in rems, and its inner frame height in pixels.
fn tab_frame(variant: TabVariant, size: Size) -> (f32, f32) {
    // The kit's label sizes and inner frame heights (`TabVariant::inner_height`).
    match (size, variant) {
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
    }
}

/// Draws its child at `rem`.
struct AtRem {
    rem: Pixels,
    child: AnyElement,
}

impl IntoElement for AtRem {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for AtRem {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        (window.with_rem_size(Some(self.rem), |window| self.child.request_layout(window, cx)), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        window.with_rem_size(Some(self.rem), |window| {
            self.child.prepaint(window, cx);
        });
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.with_rem_size(Some(self.rem), |window| self.child.paint(window, cx));
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::component::tab::Tab;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        AppContext as _, Context, InteractiveElement as _, ParentElement as _, Render, Styled as _, TestAppContext,
        TestSupportExt as _, WindowBounds, WindowOptions, div, rems, size,
    };

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
                (175, [Size::Large, Size::Large, Size::Large]),
                (250, [Size::Large, Size::Large, Size::Large]),
            ] {
                apply_zoom(Zoom::new(percent), cx);
                assert_eq!(sizes(cx), expected, "{percent}%");
            }
        });
    }

    /// A tab bar with a probe a rem high in its tab and in its suffix, and one
    /// beside it.
    struct ProbedTabs(TabVariant);

    impl Render for ProbedTabs {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let probe = |id: &'static str| div().id(id).test_support().size(rems(1.));
            let tabs = TabBar::new("tabs")
                .child(Tab::new().label("Footage").child(probe("in-tab")))
                .suffix(probe("in-suffix"));
            div().child(zoomed_tabs(tabs, self.0, Size::Small, cx)).child(probe("beside"))
        }
    }

    #[gpui_kit::test]
    fn tab_labels_stop_growing_at_the_largest_their_frames_hold(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        for (variant, largest) in [(TabVariant::Underline, 32. / 1.125), (TabVariant::Segmented, 28. / 1.125)] {
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(Default::default(), size(px(900.), px(300.))))),
                ..WindowOptions::default()
            };
            let window = cx
                .update(|cx| gpui_kit::open_window(options, cx, |_, cx| cx.new(|_| ProbedTabs(variant))))
                .expect("open test window")
                .0;
            for percent in Zoom::LEVELS {
                cx.update(|cx| apply_zoom(Zoom::new(percent), cx));
                let heights = cx
                    .update_window(window, |_, window, cx| {
                        window.render_frame(cx);
                        ["in-tab", "in-suffix", "beside"].map(|id| window.find(id).bounds().size.height)
                    })
                    .expect("window is open");
                let rem = BASE_FONT_SIZE * Zoom::new(percent).factor();
                let label = rem.min(px(largest));
                let expected = [label, label, rem];
                // Layout snaps to the device's pixels.
                let snapped =
                    heights.iter().zip(expected).all(|(height, expected)| (*height - expected).abs() < px(0.5));
                assert!(snapped, "{variant:?} at {percent}%: {heights:?}, not {expected:?}");
            }
        }
    }
}
