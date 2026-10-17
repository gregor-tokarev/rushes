//! Icons the AI edit views draw beyond GPUI Kit's default component set, and the
//! asset source that embeds them.

use std::borrow::Cow;

use gpui_kit::assets::{IconName, icon_assets};
use gpui_kit::{AssetSource, Result, SharedString};
use rushes_core::media::MediaKind;
use rushes_pipeline::RUSHES_TOOL_PREFIX;

icon_assets!(
    pub AiEditIcons,
    [
        AudioLines,
        Brain,
        Captions,
        CircleStop,
        Clapperboard,
        Clock,
        Eye,
        FilePen,
        FilePlus,
        Film,
        Image,
        Info,
        Layers,
        ListTodo,
        ListVideo,
        MessageSquare,
        RotateCcw,
        Scissors,
        ShieldAlert,
        ShieldCheck,
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

/// How the activity log names an agent tool: Rushes' editing tools in words, the
/// agent's own tools by their names.
pub(crate) fn tool_label(name: &str) -> &str {
    match name.strip_prefix(RUSHES_TOOL_PREFIX) {
        Some("inventory") => "Footage",
        Some("transcribe") => "Transcribe",
        Some("read_transcript") => "Read transcript",
        Some("phrase_words") => "Read words",
        Some("silences") => "Find pauses",
        Some("look") => "Look",
        Some("plan_cut") => "Plan cut",
        Some("render") => "Render",
        Some("check") => "Check",
        _ => name,
    }
}

/// The icon of an agent tool, by the name Claude Code reports.
pub(crate) fn tool_icon(name: &str) -> IconName {
    if let Some(tool) = name.strip_prefix(RUSHES_TOOL_PREFIX) {
        return match tool {
            "inventory" => IconName::ListVideo,
            "transcribe" => IconName::Captions,
            "read_transcript" | "phrase_words" => IconName::FileText,
            "silences" => IconName::AudioLines,
            "look" => IconName::Eye,
            "plan_cut" => IconName::Scissors,
            "render" => IconName::Clapperboard,
            "check" => IconName::ShieldCheck,
            _ => IconName::Wrench,
        };
    }
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
        let tools = [
            "Bash",
            "Read",
            "Write",
            "Edit",
            "Glob",
            "Skill",
            "Agent",
            "TodoWrite",
            "WebFetch",
            "Mystery",
            "mcp__rushes__inventory",
            "mcp__rushes__transcribe",
            "mcp__rushes__read_transcript",
            "mcp__rushes__silences",
            "mcp__rushes__look",
            "mcp__rushes__plan_cut",
            "mcp__rushes__render",
            "mcp__rushes__check",
            "mcp__rushes__new_tool",
        ];
        let kinds = [MediaKind::Video, MediaKind::Audio, MediaKind::Image];
        let icons = tools.map(tool_icon).into_iter().chain(kinds.map(media_icon));
        for icon in icons {
            let path = icon.path();
            assert!(assets.load(&path).unwrap().is_some(), "{path} is not served");
        }
    }
}
