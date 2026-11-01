//! Page icons: a video's emoji drawn as a bundled colour image.
//!
//! The text system picks an emoji's font by coverage and weight, so the same
//! emoji came out in colour in one place and as a thin outline glyph in another.
//! Every emoji the app offers (the icon picker, the former script templates, the sample
//! videos) therefore ships as a Noto Emoji image (`assets/emoji`, Apache-2.0)
//! and renders the same everywhere. Any other emoji, typed into older data, is
//! drawn as text.

use std::borrow::Cow;

use gpui_kit::assets::{AllAssets, IconName};
use gpui_kit::component::{ActiveTheme as _, Icon};
use gpui_kit::{
    App, AssetSource, Div, IntoElement, ParentElement as _, Rems, RenderOnce, Result, SharedString, Styled as _,
    StyledImage as _, Window, div, img,
};

/// Every bundled image: the emoji without U+FE0F, its file stem (code points in
/// lower-case hex joined by `_`) and the PNG.
macro_rules! page_icon_images {
    ($(($emoji:literal, $file:literal)),* $(,)?) => {
        const IMAGES: &[(&str, &str, &[u8])] = &[
            $(($emoji, $file, include_bytes!(concat!("../../assets/emoji/", $file, ".png")))),*
        ];
    };
}

page_icon_images!(
    ("🎬", "1f3ac"),
    ("🎥", "1f3a5"),
    ("📹", "1f4f9"),
    ("📸", "1f4f8"),
    ("🎤", "1f3a4"),
    ("🎧", "1f3a7"),
    ("🎵", "1f3b5"),
    ("📺", "1f4fa"),
    ("💻", "1f4bb"),
    ("📱", "1f4f1"),
    ("🎮", "1f3ae"),
    ("🔧", "1f527"),
    ("🔨", "1f528"),
    ("🧪", "1f9ea"),
    ("🔬", "1f52c"),
    ("💡", "1f4a1"),
    ("🧠", "1f9e0"),
    ("📚", "1f4da"),
    ("📝", "1f4dd"),
    ("📊", "1f4ca"),
    ("📈", "1f4c8"),
    ("💰", "1f4b0"),
    ("💸", "1f4b8"),
    ("💎", "1f48e"),
    ("🚀", "1f680"),
    ("🌟", "1f31f"),
    ("🔥", "1f525"),
    ("💥", "1f4a5"),
    ("🎯", "1f3af"),
    ("🏆", "1f3c6"),
    ("🎉", "1f389"),
    ("🎁", "1f381"),
    ("👀", "1f440"),
    ("🤔", "1f914"),
    ("😂", "1f602"),
    ("😎", "1f60e"),
    ("🙌", "1f64c"),
    ("👋", "1f44b"),
    ("🤝", "1f91d"),
    ("💬", "1f4ac"),
    ("🌍", "1f30d"),
    ("🧭", "1f9ed"),
    ("🛫", "1f6eb"),
    ("🗻", "1f5fb"),
    ("🌊", "1f30a"),
    ("🌴", "1f334"),
    ("🏃", "1f3c3"),
    ("🚗", "1f697"),
    ("🍳", "1f373"),
    ("🍕", "1f355"),
    ("🍔", "1f354"),
    ("🍿", "1f37f"),
    ("🐶", "1f436"),
    ("🐱", "1f431"),
    ("🌱", "1f331"),
    ("🌸", "1f338"),
    ("📦", "1f4e6"),
    ("🛒", "1f6d2"),
    ("🔴", "1f534"),
    ("📣", "1f4e3"),
    ("📅", "1f4c5"),
    ("📌", "1f4cc"),
    ("✅", "2705"),
    ("❓", "2753"),
    ("🛠", "1f6e0"),
    ("⭐", "2b50"),
    ("⚡", "26a1"),
    ("📄", "1f4c4"),
    ("🖥", "1f5a5"),
    ("🦀", "1f980"),
    ("🐍", "1f40d"),
    ("📼", "1f4fc"),
);

/// Asset folder of the page icon images.
const IMAGE_DIR: &str = "emoji/";

/// Size of a page icon in a row of text: table, board, sidebar, breadcrumb,
/// palette, Pipelines and Uploads.
const INLINE_SIZE: Rems = Rems(1.);

/// The bundled image of `emoji`, ignoring variation selectors.
fn image_file(emoji: &str) -> Option<&'static str> {
    let key: String = emoji.chars().filter(|c| *c != '\u{fe0f}').collect();
    IMAGES.iter().find(|(known, _, _)| *known == key).map(|(_, file, _)| *file)
}

/// Whether `emoji` is drawn from a bundled image rather than as text.
pub fn has_page_icon_image(emoji: &str) -> bool {
    image_file(emoji).is_some()
}

/// The application's asset source: the page icon images, then the complete
/// Lucide catalog the feature modules draw their icons from.
#[derive(Clone, Copy, Debug, Default)]
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let image = path
            .strip_prefix(IMAGE_DIR)
            .and_then(|name| name.strip_suffix(".png"))
            .and_then(|stem| IMAGES.iter().find(|(_, file, _)| *file == stem));
        match image {
            Some((_, _, bytes)) => Ok(Some(Cow::Borrowed(bytes))),
            None => AllAssets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = AllAssets.list(path)?;
        if IMAGE_DIR.starts_with(path) || path.starts_with(IMAGE_DIR) {
            paths.extend(
                IMAGES
                    .iter()
                    .map(|(_, file, _)| SharedString::from(format!("{IMAGE_DIR}{file}.png")))
                    .filter(|asset| asset.starts_with(path)),
            );
        }
        Ok(paths)
    }
}

/// A video's page icon in a square slot: its emoji as a colour image, or as
/// text when the emoji isn't bundled; a video without an icon gets the neutral
/// page glyph, so every surface shows "no icon" the same way.
#[derive(IntoElement)]
pub struct PageIcon {
    icon: Option<SharedString>,
    size: Rems,
}

impl PageIcon {
    /// The icon of a video (`None` or blank: it has none), at the inline size.
    pub fn new(icon: Option<&str>) -> Self {
        let icon = icon.map(str::trim).filter(|icon| !icon.is_empty()).map(|icon| SharedString::from(icon.to_owned()));
        Self { icon, size: INLINE_SIZE }
    }

    /// The side of the square slot; the image fills it.
    pub fn size(mut self, size: Rems) -> Self {
        self.size = size;
        self
    }
}

impl RenderOnce for PageIcon {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let size = self.size;
        let slot = div().flex().flex_none().items_center().justify_center().size(size);
        let Some(emoji) = self.icon else {
            // Lucide draws inside a padded box; at 7/8 of the slot the glyph
            // matches an emoji's visual size.
            return slot.child(
                Icon::new(IconName::FileText).size(Rems(size.0 * 0.875)).text_color(cx.theme().muted_foreground),
            );
        };
        match image_file(&emoji) {
            Some(file) => slot.child(
                img(SharedString::from(format!("{IMAGE_DIR}{file}.png")))
                    .size(size)
                    .with_fallback(move || emoji_text(emoji.clone(), size).into_any_element()),
            ),
            None => slot.child(emoji_text(emoji, size)),
        }
    }
}

/// A video's page icon at the start of a row (table, board card, Pipelines,
/// Uploads), centred in a fixed lane so the titles after it line up.
pub fn video_icon(icon: Option<&str>) -> Div {
    div().flex().flex_none().w_5().h_5().items_center().justify_center().child(PageIcon::new(icon))
}

/// An emoji without an image, drawn as text that fills the slot.
fn emoji_text(emoji: SharedString, size: Rems) -> impl IntoElement {
    div().text_size(Rems(size.0 * 0.8)).line_height(size).child(emoji)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offered_emoji_have_images() {
        for emoji in crate::video_page::ICON_CHOICES.iter().copied() {
            assert!(has_page_icon_image(emoji), "{emoji} has no bundled image");
        }
        // With or without the emoji presentation selector.
        assert!(has_page_icon_image("🛠️") && has_page_icon_image("🛠"));
        assert!(!has_page_icon_image("🦄"), "other emoji are drawn as text");
    }

    #[test]
    fn a_blank_icon_is_no_icon() {
        // Every surface shows a video without an icon with the same page glyph.
        assert!(PageIcon::new(None).icon.is_none());
        assert!(PageIcon::new(Some("  ")).icon.is_none());
        assert_eq!(PageIcon::new(Some(" 🦀 ")).icon.as_deref(), Some("🦀"));
    }

    #[test]
    fn images_are_served_by_their_asset_path() {
        let image = Assets.load("emoji/1f3ac.png").unwrap().expect("the clapper image");
        assert!(image.starts_with(b"\x89PNG"));
        assert!(Assets.load("emoji/ffff.png").map_or(true, |found| found.is_none()));
        assert!(Assets.list("emoji/").unwrap().iter().any(|path| path == "emoji/2b50.png"));
        assert!(Assets.load("icons/file-text.svg").unwrap().is_some(), "Lucide icons still resolve");
    }
}
