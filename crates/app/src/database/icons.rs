//! Icons used by the database views and the asset source that embeds them.

use std::borrow::Cow;

use gpui_kit::assets::{IconName, icon_assets};
use gpui_kit::{AssetSource, Result, SharedString};
use videocrm_core::{BuiltinField, Column, PropertyDef, PropertyKind, ViewKind};

// The Lucide icons the database views draw, beyond the default component set.
icon_assets!(
    pub DatabaseIcons,
    [
        ALargeSmall,
        ArrowDown,
        ArrowUp,
        ArrowUpDown,
        Calendar,
        Check,
        ChevronDown,
        CircleChevronDown,
        CircleDot,
        CirclePlay,
        Clapperboard,
        Clock,
        Copy,
        Ellipsis,
        ExternalLink,
        Eye,
        EyeOff,
        FileText,
        Hash,
        Link,
        List,
        ListFilter,
        PanelRightOpen,
        Pencil,
        Plus,
        Search,
        SearchX,
        Settings2,
        Sheet,
        SlidersHorizontal,
        SquareCheck,
        SquareKanban,
        Tags,
        TextAlignStart,
        Trash,
        WandSparkles,
        X,
    ]
);

/// An asset source that serves [`DatabaseIcons`] and falls back to `inner`.
///
/// Register it once at startup, wrapping the application's other sources:
/// `application().with_assets(WithDatabaseIcons(gpui_kit::assets::Assets))`.
#[derive(Clone, Copy, Debug, Default)]
pub struct WithDatabaseIcons<A>(pub A);

impl<A: AssetSource> AssetSource for WithDatabaseIcons<A> {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match DatabaseIcons.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => self.0.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = self.0.list(path)?;
        paths.extend(DatabaseIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

pub(crate) fn view_icon(kind: ViewKind) -> IconName {
    match kind {
        ViewKind::Table => IconName::Sheet,
        ViewKind::Board => IconName::SquareKanban,
    }
}

pub(crate) fn kind_icon(kind: &PropertyKind) -> IconName {
    match kind {
        PropertyKind::Text => IconName::TextAlignStart,
        PropertyKind::Number => IconName::Hash,
        PropertyKind::Select { .. } => IconName::CircleChevronDown,
        PropertyKind::MultiSelect { .. } => IconName::List,
        PropertyKind::Date => IconName::Calendar,
        PropertyKind::Checkbox => IconName::SquareCheck,
        PropertyKind::Url => IconName::Link,
    }
}

pub(crate) fn column_icon(column: Column, properties: &[PropertyDef]) -> IconName {
    match column {
        Column::Builtin(field) => match field {
            BuiltinField::Title => IconName::ALargeSmall,
            BuiltinField::Status => IconName::CircleDot,
            BuiltinField::Format => IconName::CircleChevronDown,
            BuiltinField::PublishDate => IconName::Calendar,
            BuiltinField::Tags => IconName::Tags,
            BuiltinField::Script => IconName::FileText,
            BuiltinField::Edit => IconName::WandSparkles,
            BuiltinField::Youtube => IconName::CirclePlay,
            BuiltinField::CreatedAt | BuiltinField::UpdatedAt => IconName::Clock,
        },
        Column::Custom(id) => properties
            .iter()
            .find(|property| property.id == id)
            .map_or(IconName::TextAlignStart, |property| kind_icon(&property.kind)),
    }
}
