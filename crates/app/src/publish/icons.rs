//! Icons the publish views draw beyond GPUI Kit's default component set.

use std::borrow::Cow;

use gpui_kit::assets::icon_assets;
use gpui_kit::{AssetSource, Result, SharedString};

icon_assets!(pub PublishIcons, [ArrowRight, Circle, Film, Image, ListOrdered, Upload]);

/// An asset source that serves [`PublishIcons`] and falls back to `inner`.
///
/// Not needed when the application registers the full Lucide catalog
/// (`gpui_kit::assets::AllAssets`).
#[derive(Clone, Copy, Debug, Default)]
pub struct WithPublishIcons<A>(pub A);

impl<A: AssetSource> AssetSource for WithPublishIcons<A> {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match PublishIcons.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => self.0.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = self.0.list(path)?;
        paths.extend(PublishIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}
