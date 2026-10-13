//! Icons the AI edit views draw beyond GPUI Kit's default component set, and the
//! asset source that embeds them.

use std::borrow::Cow;

use gpui_kit::assets::{IconName, icon_assets};
use gpui_kit::{AssetSource, Result, SharedString};
use rushes_core::media::MediaKind;

icon_assets!(
    pub AiEditIcons,
    [
        AudioLines,
        Brain,
        CircleStop,
        Clapperboard,
        Clock,
        FilePen,
        FilePlus,
        Film,
        Image,
        Info,
        Layers,
        ListTodo,
        MessageSquare,
        RotateCcw,
        ShieldAlert,
        Sparkles,
        Undo2,
        Upload,
        WandSparkles,
        Wrench,
    ]
);

/// An asset source that serves [`AiEditIcons`] and falls back to `inner`.
///
/// Not needed when the application registers the complete Lucide set
/// (`gpui_kit::assets::AllAssets`).
#[derive(Clone, Copy, Debug, Default)]
pub struct WithAiEditIcons<A>(pub A);

impl<A: AssetSource> AssetSource for WithAiEditIcons<A> {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match AiEditIcons.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => self.0.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = self.0.list(path)?;
        paths.extend(AiEditIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

pub(crate) fn media_icon(kind: MediaKind) -> IconName {
    match kind {
        MediaKind::Video => IconName::Film,
        MediaKind::Audio => IconName::AudioLines,
        MediaKind::Image => IconName::Image,
    }
}

/// The icon of an agent tool, by the name Claude Code reports.
pub(crate) fn tool_icon(name: &str) -> IconName {
    match name {
        "Bash" => IconName::SquareTerminal,
        "Read" => IconName::FileText,
        "Write" => IconName::FilePlus,
        "Edit" | "MultiEdit" | "NotebookEdit" => IconName::FilePen,
        "Glob" | "Grep" => IconName::Search,
        "Skill" => IconName::Sparkles,
        "Agent" | "Task" => IconName::Bot,
        "TodoWrite" => IconName::ListTodo,
        "WebFetch" | "WebSearch" => IconName::Globe,
        _ => IconName::Wrench,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_and_media_icons_resolve_over_the_default_set() {
        let assets = WithAiEditIcons(gpui_kit::assets::Assets);
        let tools = ["Bash", "Read", "Write", "Edit", "Glob", "Skill", "Agent", "TodoWrite", "WebFetch", "Mystery"];
        let kinds = [MediaKind::Video, MediaKind::Audio, MediaKind::Image];
        let icons = tools.map(tool_icon).into_iter().chain(kinds.map(media_icon));
        for icon in icons {
            let path = icon.path();
            assert!(assets.load(&path).unwrap().is_some(), "{path} is not served");
        }
    }
}
