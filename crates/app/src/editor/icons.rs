//! Icons the editor draws beyond GPUI Kit's default component set.

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};

gpui_kit::assets::icon_assets!(
    EditorIcons,
    [
        ArrowDown,
        ArrowUp,
        Clapperboard,
        Code,
        Copy,
        GripVertical,
        Heading1,
        Heading2,
        Heading3,
        Lightbulb,
        List,
        ListOrdered,
        ListTodo,
        Pilcrow,
        Plus,
        Repeat2,
        SeparatorHorizontal,
        TextQuote,
        Trash,
        Type,
    ]
);

/// Asset source with the editor's icons layered over `gpui_kit::assets::Assets`.
///
/// Register it (or chain [`Assets::load`] from the application's own source)
/// so block icons resolve: `gpui_kit::application().with_assets(editor::Assets)`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match EditorIcons.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => gpui_kit::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = EditorIcons.list(path)?;
        paths.extend(gpui_kit::assets::Assets.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}
