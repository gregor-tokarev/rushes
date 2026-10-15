//! Block-level Markdown: shortcuts typed at the start of a block, and
//! multi-line text pasted into the editor, which is read like an imported
//! Markdown file ([`from_markdown`]).

use rushes_core::document::{from_markdown, quote_marker};
use rushes_core::{Block, BlockKind};

use super::blocks::MAX_INDENT;

/// A block kind requested by typing a Markdown prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shortcut {
    pub kind: BlockKind,
    pub checked: bool,
    /// Length of the typed marker, removed from the text on conversion.
    pub marker_len: usize,
}

/// Detects a shortcut whose marker starts the text and ends at the caret,
/// i.e. one the user has just finished typing.
pub fn detect_shortcut(text: &str, cursor: usize) -> Option<Shortcut> {
    let typed = text.get(..cursor)?;
    let shortcut = |kind, checked| Some(Shortcut { kind, checked, marker_len: cursor });
    match typed {
        "# " => shortcut(BlockKind::Heading1, false),
        "## " => shortcut(BlockKind::Heading2, false),
        "### " => shortcut(BlockKind::Heading3, false),
        "- " | "* " | "+ " => shortcut(BlockKind::Bulleted, false),
        "[] " | "[ ] " => shortcut(BlockKind::Todo, false),
        "[x] " | "[X] " => shortcut(BlockKind::Todo, true),
        "> " => shortcut(BlockKind::Quote, false),
        "!! " => shortcut(BlockKind::Callout, false),
        "```" => shortcut(BlockKind::Code, false),
        "---" if text == "---" => shortcut(BlockKind::Divider, false),
        _ if is_ordered_marker(typed) => shortcut(BlockKind::Numbered, false),
        _ => None,
    }
}

/// `1. `, `12. `: digits followed by a dot and a space.
fn is_ordered_marker(typed: &str) -> bool {
    typed
        .strip_suffix(". ")
        .is_some_and(|digits| !digits.is_empty() && digits.len() <= 3 && digits.bytes().all(|b| b.is_ascii_digit()))
}

/// Detects a callout-style marker (`[!note] `, `[!point] `, `[!broll] `, `[!onscreen] `)
/// just typed at the start of a quote: the syntax Markdown files use for
/// callouts and cues, typed after the `> ` shortcut.
pub fn detect_quote_marker(text: &str, cursor: usize) -> Option<Shortcut> {
    let marker = text.get(..cursor)?.strip_suffix(' ')?;
    let (kind, len) = quote_marker(marker)?;
    (len == marker.len()).then_some(Shortcut { kind, checked: false, marker_len: cursor })
}

/// Pasted text as blocks, read exactly like an imported Markdown file, so
/// the app's own export pastes back unchanged. List nesting is capped at the
/// editor's deepest level.
pub fn parse_pasted(text: &str) -> Vec<Block> {
    let mut blocks = from_markdown(text);
    for block in &mut blocks {
        block.indent = block.indent.min(MAX_INDENT);
    }
    blocks
}

pub fn is_list(kind: BlockKind) -> bool {
    matches!(kind, BlockKind::Bulleted | BlockKind::Numbered | BlockKind::Todo)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_shortcuts_ending_at_the_caret() {
        let kind = |text: &str, cursor| detect_shortcut(text, cursor).map(|s| (s.kind, s.checked, s.marker_len));
        assert_eq!(kind("# ", 2), Some((BlockKind::Heading1, false, 2)));
        assert_eq!(kind("## Title", 3), Some((BlockKind::Heading2, false, 3)));
        assert_eq!(kind("### ", 4), Some((BlockKind::Heading3, false, 4)));
        assert_eq!(kind("- ", 2), Some((BlockKind::Bulleted, false, 2)));
        assert_eq!(kind("* ", 2), Some((BlockKind::Bulleted, false, 2)));
        assert_eq!(kind("1. ", 3), Some((BlockKind::Numbered, false, 3)));
        assert_eq!(kind("12. ", 4), Some((BlockKind::Numbered, false, 4)));
        assert_eq!(kind("[] ", 3), Some((BlockKind::Todo, false, 3)));
        assert_eq!(kind("[ ] ", 4), Some((BlockKind::Todo, false, 4)));
        assert_eq!(kind("[x] ", 4), Some((BlockKind::Todo, true, 4)));
        assert_eq!(kind("> ", 2), Some((BlockKind::Quote, false, 2)));
        assert_eq!(kind("!! ", 3), Some((BlockKind::Callout, false, 3)));
        assert_eq!(kind("```", 3), Some((BlockKind::Code, false, 3)));
        assert_eq!(kind("---", 3), Some((BlockKind::Divider, false, 3)));
    }

    #[test]
    fn ignores_markers_not_just_typed() {
        assert_eq!(detect_shortcut("# ", 1), None);
        assert_eq!(detect_shortcut("#", 1), None);
        assert_eq!(detect_shortcut("a # ", 4), None);
        assert_eq!(detect_shortcut("---x", 3), None);
        assert_eq!(detect_shortcut("1.5 ", 4), None);
        assert_eq!(detect_shortcut(". ", 2), None);
    }

    #[test]
    fn detects_callout_markers_typed_into_a_quote() {
        let kind = |text: &str, cursor| detect_quote_marker(text, cursor).map(|s| (s.kind, s.marker_len));
        assert_eq!(kind("[!broll] ", 9), Some((BlockKind::BRoll, 9)));
        assert_eq!(kind("[!onscreen] Subscribe", 12), Some((BlockKind::OnScreen, 12)));
        assert_eq!(kind("[!note] ", 8), Some((BlockKind::Callout, 8)));
        assert_eq!(kind("[!point] ", 9), Some((BlockKind::TalkingPoint, 9)));
        assert_eq!(kind("[!NOTE] ", 8), Some((BlockKind::Callout, 8)));
        assert_eq!(kind("[!broll]", 8), None, "not before the space");
        assert_eq!(kind("[!broll] Slow pan", 17), None);
        assert_eq!(kind("[!quote] ", 9), None);
    }

    #[test]
    fn pastes_markdown_like_an_import() {
        let blocks = parse_pasted(
            "# Title\n\nIntro with **bold**\n\n- one\n  - nested\n1. first\n- [x] done\n\n> quoted\n\n\
             > [!note] Pin the Rust version\n\n> [!NOTE]\n> GitHub style note that spans\n> two lines.\n\n\
             > [!broll] Drone shot\n\n> [!onscreen] Subscribe\n\n---\n",
        );
        let summary: Vec<_> = blocks.iter().map(|b| (b.kind, b.text.as_str(), b.indent, b.checked)).collect();
        assert_eq!(
            summary,
            vec![
                (BlockKind::Heading1, "Title", 0, false),
                (BlockKind::Paragraph, "Intro with **bold**", 0, false),
                (BlockKind::Bulleted, "one", 0, false),
                (BlockKind::Bulleted, "nested", 1, false),
                (BlockKind::Numbered, "first", 0, false),
                (BlockKind::Todo, "done", 0, true),
                (BlockKind::Quote, "quoted", 0, false),
                (BlockKind::Callout, "Pin the Rust version", 0, false),
                (BlockKind::Callout, "GitHub style note that spans\ntwo lines.", 0, false),
                (BlockKind::BRoll, "Drone shot", 0, false),
                (BlockKind::OnScreen, "Subscribe", 0, false),
                (BlockKind::Divider, "", 0, false),
            ]
        );
    }

    #[test]
    fn keeps_fenced_code_together() {
        let blocks = parse_pasted("before\n```rust\nfn main() {\n    run();\n}\n```\nafter");
        let summary: Vec<_> = blocks.iter().map(|b| (b.kind, b.text.as_str())).collect();
        assert_eq!(
            summary,
            vec![
                (BlockKind::Paragraph, "before"),
                (BlockKind::Code, "fn main() {\n    run();\n}"),
                (BlockKind::Paragraph, "after"),
            ]
        );
    }

    #[test]
    fn caps_indent_and_handles_crlf() {
        let blocks = parse_pasted("- a\r\n  - b\r\n    - c\r\n      - d\r\n        - deep\r\n");
        let indents: Vec<_> = blocks.iter().map(|b| (b.text.as_str(), b.indent)).collect();
        assert_eq!(indents, [("a", 0), ("b", 1), ("c", 2), ("d", 3), ("deep", MAX_INDENT)]);
    }
}
