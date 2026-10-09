//! Block-level Markdown: shortcuts typed at the start of a block and
//! multi-line plain text pasted into the editor.

use videocrm_core::{Block, BlockKind};

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

/// Splits pasted plain text into blocks, one per non-empty line, recognising
/// headings, lists, to-dos, quotes, B-roll / on-screen cues (`> [!broll]`),
/// dividers and fenced code. Leading indentation nests list items.
pub fn parse_blocks(text: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut code: Option<Vec<&str>> = None;
    for line in text.lines() {
        if let Some(lines) = code.as_mut() {
            if line.trim_start().starts_with("```") {
                blocks.push(Block::new(BlockKind::Code, lines.join("\n")));
                code = None;
            } else {
                lines.push(line);
            }
            continue;
        }
        if line.trim_start().starts_with("```") {
            code = Some(Vec::new());
            continue;
        }
        if !line.trim().is_empty() {
            blocks.push(parse_line(line));
        }
    }
    if let Some(lines) = code {
        blocks.push(Block::new(BlockKind::Code, lines.join("\n")));
    }
    blocks
}

fn parse_line(line: &str) -> Block {
    let content = line.trim_start();
    let leading = &line[..line.len() - content.len()];
    let width: usize = leading.chars().map(|c| if c == '\t' { 4 } else { 1 }).sum();
    let indent = (width / 2).min(MAX_INDENT as usize) as u8;
    let content = content.trim_end();

    let (kind, checked, text) = classify(content);
    let mut block = Block::new(kind, text);
    block.checked = checked;
    if is_list(kind) {
        block.indent = indent;
    }
    block
}

fn classify(content: &str) -> (BlockKind, bool, &str) {
    const PREFIXES: [(&str, BlockKind, bool); 15] = [
        ("### ", BlockKind::Heading3, false),
        ("## ", BlockKind::Heading2, false),
        ("# ", BlockKind::Heading1, false),
        ("- [ ] ", BlockKind::Todo, false),
        ("- [x] ", BlockKind::Todo, true),
        ("- [X] ", BlockKind::Todo, true),
        ("[ ] ", BlockKind::Todo, false),
        ("[x] ", BlockKind::Todo, true),
        ("> [!broll] ", BlockKind::BRoll, false),
        ("> [!onscreen] ", BlockKind::OnScreen, false),
        ("> ", BlockKind::Quote, false),
        ("- ", BlockKind::Bulleted, false),
        ("* ", BlockKind::Bulleted, false),
        ("+ ", BlockKind::Bulleted, false),
        ("!! ", BlockKind::Callout, false),
    ];
    if matches!(content, "---" | "***" | "___") {
        return (BlockKind::Divider, false, "");
    }
    for (prefix, kind, checked) in PREFIXES {
        if let Some(text) = content.strip_prefix(prefix) {
            return (kind, checked, text);
        }
    }
    if let Some((digits, text)) = content.split_once(". ")
        && is_ordered_marker(&content[..digits.len() + 2])
    {
        return (BlockKind::Numbered, false, text);
    }
    (BlockKind::Paragraph, false, content)
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
    fn parses_pasted_lines_into_blocks() {
        let blocks = parse_blocks(
            "# Title\n\nIntro with **bold**\n- one\n  - nested\n1. first\n- [x] done\n[ ] open\n> quoted\n> [!broll] Drone shot\n---\n",
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
                (BlockKind::Todo, "open", 0, false),
                (BlockKind::Quote, "quoted", 0, false),
                (BlockKind::BRoll, "Drone shot", 0, false),
                (BlockKind::Divider, "", 0, false),
            ]
        );
    }

    #[test]
    fn keeps_fenced_code_together() {
        let blocks = parse_blocks("before\n```rust\nfn main() {\n    run();\n}\n```\nafter");
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
        let blocks = parse_blocks("- a\r\n\t\t\t- deep\r\n");
        assert_eq!(blocks[0].text, "a");
        assert_eq!(blocks[1].text, "deep");
        assert_eq!(blocks[1].indent, MAX_INDENT);
    }
}
