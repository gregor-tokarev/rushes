//! Document logic: script statistics, outline, Markdown import/export,
//! teleprompter text and inline Markdown parsing.
//!
//! Markdown mapping (export and import are inverses for every [`BlockKind`]):
//!
//! | Block       | Markdown                                    |
//! |-------------|---------------------------------------------|
//! | Heading 1–3 | `# `, `## `, `### `                         |
//! | Bulleted    | `- ` (two spaces of indent per level)       |
//! | Numbered    | `1. ` (numbered per list, indented likewise) |
//! | Todo        | `- [ ] ` / `- [x] `                         |
//! | Quote       | `> `                                        |
//! | Callout     | `> [!note] `                                |
//! | B-roll cue  | `> [!broll] `                               |
//! | On-screen   | `> [!onscreen] `                            |
//! | Code        | fenced with backticks, no language          |
//! | Divider     | `---`                                       |
//! | Paragraph   | plain text, blocks separated by blank lines |
//!
//! Text lines that would otherwise read as markup are escaped with a leading
//! backslash. Normalisations: empty paragraphs are dropped, newlines in
//! headings become spaces, and blank lines inside a paragraph or list item
//! split it into separate blocks.

use std::borrow::Cow;

use crate::model::{Block, BlockId, BlockKind};

/// Speaking rate used for runtime estimates, in words per minute.
pub const DEFAULT_WPM: u32 = 150;

/// Words read aloud: spoken block kinds only, inline Markdown stripped,
/// punctuation-only tokens ignored.
pub fn spoken_word_count(blocks: &[Block]) -> usize {
    blocks
        .iter()
        .filter(|block| block.kind.is_spoken())
        .map(|block| word_count(&strip_inline_markdown(&block.text)))
        .sum()
}

fn word_count(text: &str) -> usize {
    text.split_whitespace().filter(|word| word.chars().any(char::is_alphanumeric)).count()
}

/// Estimated spoken runtime of the blocks in seconds at `wpm` words per minute.
pub fn estimated_runtime_secs(blocks: &[Block], wpm: u32) -> u32 {
    runtime_secs_for_words(spoken_word_count(blocks), wpm)
}

/// Seconds needed to speak `words` at `wpm`, rounded to the nearest second.
pub fn runtime_secs_for_words(words: usize, wpm: u32) -> u32 {
    if wpm == 0 {
        return 0;
    }
    let secs = (words as u64 * 60 + u64::from(wpm) / 2) / u64::from(wpm);
    u32::try_from(secs).unwrap_or(u32::MAX)
}

/// Compact duration: `45s`, `8m 16s`, `8m`, `1h 5m`.
pub fn format_runtime(secs: u32) -> String {
    let (hours, minutes, seconds) = (secs / 3600, secs % 3600 / 60, secs % 60);
    match (hours, minutes, seconds) {
        (0, 0, s) => format!("{s}s"),
        (0, m, 0) => format!("{m}m"),
        (0, m, s) => format!("{m}m {s}s"),
        (h, m, _) => format!("{h}h {m}m"),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutlineItem {
    pub block_id: BlockId,
    /// 1–3, from the heading kind.
    pub level: u8,
    /// Heading text with inline Markdown stripped.
    pub text: String,
}

/// Non-empty headings in document order.
pub fn outline(blocks: &[Block]) -> Vec<OutlineItem> {
    blocks
        .iter()
        .filter_map(|block| {
            let level = match block.kind {
                BlockKind::Heading1 => 1,
                BlockKind::Heading2 => 2,
                BlockKind::Heading3 => 3,
                _ => return None,
            };
            let text = strip_inline_markdown(&block.text).trim().to_string();
            (!text.is_empty()).then_some(OutlineItem { block_id: block.id, level, text })
        })
        .collect()
}

/// What the presenter reads: spoken blocks only, inline Markdown stripped, one
/// blank line between blocks.
pub fn teleprompter_text(blocks: &[Block]) -> String {
    blocks
        .iter()
        .filter(|block| block.kind.is_spoken())
        .map(|block| strip_inline_markdown(&block.text).trim().to_string())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

// ---------------------------------------------------------------------------
// Markdown export
// ---------------------------------------------------------------------------

/// Serialises blocks to Markdown. Pass `&document.blocks` for a [`crate::Document`].
pub fn to_markdown(blocks: &[Block]) -> String {
    let mut out = String::new();
    // Running number per indent level for numbered lists.
    let mut numbering: Vec<u32> = Vec::new();
    let mut previous_was_list = false;

    for block in blocks {
        if block.kind == BlockKind::Paragraph && block.text.trim().is_empty() {
            continue;
        }
        let is_list = is_list_kind(block.kind);
        if !out.is_empty() {
            out.push_str(if is_list && previous_was_list { "\n" } else { "\n\n" });
        }
        previous_was_list = is_list;

        let number = if is_list {
            let level = usize::from(block.indent);
            numbering.resize(level + 1, 0);
            numbering[level] = if block.kind == BlockKind::Numbered { numbering[level] + 1 } else { 0 };
            numbering[level]
        } else {
            numbering.clear();
            0
        };
        write_block(&mut out, block, number);
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

fn is_list_kind(kind: BlockKind) -> bool {
    matches!(kind, BlockKind::Bulleted | BlockKind::Numbered | BlockKind::Todo)
}

fn write_block(out: &mut String, block: &Block, number: u32) {
    let pad = "  ".repeat(usize::from(block.indent));
    let text = block.text.as_str();
    match block.kind {
        BlockKind::Paragraph => {
            let lines: Vec<String> = text.lines().map(|line| escape_text_line(line).into_owned()).collect();
            out.push_str(&lines.join("\n"));
        }
        BlockKind::Heading1 | BlockKind::Heading2 | BlockKind::Heading3 => {
            let hashes = match block.kind {
                BlockKind::Heading1 => "#",
                BlockKind::Heading2 => "##",
                _ => "###",
            };
            let single_line = text.split('\n').map(str::trim).collect::<Vec<_>>().join(" ");
            push_line(out, hashes, &escape_leading_backslash(&single_line));
        }
        BlockKind::Bulleted => {
            let first = first_line(text);
            // `- [ ] x` would read as a todo and `- - -` as a divider.
            let first = if task_marker(first).is_some() || is_divider(&format!("- {first}")) {
                Cow::Owned(format!("\\{first}"))
            } else {
                escape_leading_backslash(first)
            };
            write_list_item(out, &format!("{pad}-"), &first, text, &format!("{pad}  "));
        }
        BlockKind::Numbered => {
            let marker = format!("{pad}{number}.");
            let continuation = " ".repeat(marker.len() + 1);
            write_list_item(out, &marker, &escape_leading_backslash(first_line(text)), text, &continuation);
        }
        BlockKind::Todo => {
            let marker = format!("{pad}- [{}]", if block.checked { 'x' } else { ' ' });
            write_list_item(out, &marker, &escape_leading_backslash(first_line(text)), text, &format!("{pad}  "));
        }
        BlockKind::Quote | BlockKind::Callout | BlockKind::BRoll | BlockKind::OnScreen => {
            let marker = match block.kind {
                BlockKind::Callout => "> [!note]",
                BlockKind::BRoll => "> [!broll]",
                BlockKind::OnScreen => "> [!onscreen]",
                _ => ">",
            };
            for (index, line) in text.split('\n').enumerate() {
                if index > 0 {
                    out.push('\n');
                }
                let escaped = if index == 0 && block.kind == BlockKind::Quote && line.starts_with("[!") {
                    Cow::Owned(format!("\\{line}"))
                } else {
                    escape_leading_backslash(line)
                };
                push_line(out, if index == 0 { marker } else { ">" }, &escaped);
            }
        }
        BlockKind::Code => {
            let longest_run = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
            let fence = "`".repeat(longest_run.max(2) + 1);
            out.push_str(&fence);
            out.push('\n');
            if !text.is_empty() {
                out.push_str(text);
                out.push('\n');
            }
            out.push_str(&fence);
        }
        BlockKind::Divider => out.push_str("---"),
    }
}

fn first_line(text: &str) -> &str {
    text.split('\n').next().unwrap_or_default()
}

/// `marker first-line`, then the remaining lines indented by `continuation`.
fn write_list_item(out: &mut String, marker: &str, first: &str, text: &str, continuation: &str) {
    push_line(out, marker, first);
    for line in text.split('\n').skip(1) {
        out.push('\n');
        if !line.is_empty() {
            out.push_str(continuation);
            out.push_str(&escape_text_line(line));
        }
    }
}

/// `prefix text`, without a trailing space when the text is empty.
fn push_line(out: &mut String, prefix: &str, text: &str) {
    out.push_str(prefix);
    if !text.is_empty() {
        if !prefix.is_empty() {
            out.push(' ');
        }
        out.push_str(text);
    }
}

/// Escapes a paragraph or continuation line that would parse as a block marker.
fn escape_text_line(line: &str) -> Cow<'_, str> {
    let content = line.trim_start();
    if content.starts_with('\\') || block_start(content).is_some() {
        let indent = &line[..line.len() - content.len()];
        Cow::Owned(format!("{indent}\\{content}"))
    } else {
        Cow::Borrowed(line)
    }
}

/// Text that already starts with a backslash gets a second one, so the
/// importer's unescape restores it.
fn escape_leading_backslash(text: &str) -> Cow<'_, str> {
    if text.starts_with('\\') { Cow::Owned(format!("\\{text}")) } else { Cow::Borrowed(text) }
}

// ---------------------------------------------------------------------------
// Markdown import
// ---------------------------------------------------------------------------

/// Characters a leading backslash escapes at the start of a text line.
const ESCAPABLE: &[char] = &['\\', '#', '-', '*', '+', '_', '>', '`', '['];

enum BlockStart<'a> {
    Heading(BlockKind, &'a str),
    Divider,
    Fence(usize),
    ListItem { kind: BlockKind, checked: bool, text: &'a str },
    Quote(&'a str),
}

/// Recognises a block marker at the start of `content` (leading whitespace already removed).
fn block_start(content: &str) -> Option<BlockStart<'_>> {
    if let Some((kind, text)) = heading(content) {
        return Some(BlockStart::Heading(kind, text));
    }
    if is_divider(content) {
        return Some(BlockStart::Divider);
    }
    if let Some(fence) = fence(content) {
        return Some(BlockStart::Fence(fence));
    }
    if let Some(rest) = content.strip_prefix('>') {
        return Some(BlockStart::Quote(rest.strip_prefix(' ').unwrap_or(rest)));
    }
    list_item(content)
}

fn heading(content: &str) -> Option<(BlockKind, &str)> {
    let hashes = content.bytes().take_while(|&b| b == b'#').count();
    let rest = &content[hashes..];
    if !(1..=6).contains(&hashes) || !(rest.is_empty() || rest.starts_with([' ', '\t'])) {
        return None;
    }
    let kind = match hashes {
        1 => BlockKind::Heading1,
        2 => BlockKind::Heading2,
        _ => BlockKind::Heading3,
    };
    Some((kind, rest.trim()))
}

fn is_divider(content: &str) -> bool {
    let Some(marker) = content.chars().next().filter(|c| matches!(c, '-' | '*' | '_')) else { return false };
    content.chars().all(|c| c == marker || c == ' ' || c == '\t')
        && content.chars().filter(|&c| c == marker).count() >= 3
}

fn fence(content: &str) -> Option<usize> {
    let ticks = content.bytes().take_while(|&b| b == b'`').count();
    (ticks >= 3 && !content[ticks..].contains('`')).then_some(ticks)
}

fn list_item(content: &str) -> Option<BlockStart<'_>> {
    if let Some(rest) = content.strip_prefix(['-', '*', '+']) {
        let text = item_text(rest)?;
        return Some(match task_marker(text) {
            Some((checked, text)) => BlockStart::ListItem { kind: BlockKind::Todo, checked, text },
            None => BlockStart::ListItem { kind: BlockKind::Bulleted, checked: false, text },
        });
    }
    let digits = content.bytes().take_while(u8::is_ascii_digit).count();
    if !(1..=9).contains(&digits) {
        return None;
    }
    let rest = content[digits..].strip_prefix(['.', ')'])?;
    Some(BlockStart::ListItem { kind: BlockKind::Numbered, checked: false, text: item_text(rest)? })
}

/// Text after a list marker, which must be followed by whitespace or the end of the line.
fn item_text(rest: &str) -> Option<&str> {
    (rest.is_empty() || rest.starts_with([' ', '\t'])).then(|| rest.trim_start())
}

/// `[ ] text` / `[x] text` at the start of a bullet's text.
fn task_marker(text: &str) -> Option<(bool, &str)> {
    let (checked, rest) = if let Some(rest) = text.strip_prefix("[ ]") {
        (false, rest)
    } else {
        (true, text.strip_prefix("[x]").or_else(|| text.strip_prefix("[X]"))?)
    };
    item_text(rest).map(|text| (checked, text))
}

fn unescape(text: &str) -> &str {
    match text.strip_prefix('\\') {
        Some(rest) if rest.starts_with(ESCAPABLE) || rest.starts_with(|c: char| c.is_ascii_digit()) => rest,
        _ => text,
    }
}

/// Callout-style marker (`[!broll] text`) on the first line of a quote. GitHub
/// alert names (`[!NOTE]`, `[!TIP]`, ...) import as callouts.
fn quote_kind(first_line: &str) -> (BlockKind, &str) {
    let marker = first_line.strip_prefix("[!").and_then(|rest| rest.split_once(']'));
    let Some((name, rest)) = marker else { return (BlockKind::Quote, unescape(first_line)) };
    let kind = match name.to_ascii_lowercase().as_str() {
        "broll" | "b-roll" => BlockKind::BRoll,
        "onscreen" | "on-screen" => BlockKind::OnScreen,
        "note" | "tip" | "info" | "important" | "warning" | "caution" => BlockKind::Callout,
        _ => return (BlockKind::Quote, first_line),
    };
    (kind, unescape(rest.trim_start()))
}

fn is_quote_like(kind: BlockKind) -> bool {
    matches!(kind, BlockKind::Quote | BlockKind::Callout | BlockKind::BRoll | BlockKind::OnScreen)
}

/// Visual width of leading whitespace (tabs advance to the next multiple of 4).
fn leading_width(line: &str) -> usize {
    line.chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .fold(0, |width, c| if c == '\t' { width / 4 * 4 + 4 } else { width + 1 })
}

/// Spaces per nesting level, inferred from the indented list items (2 for
/// this module's own output, 4 or 3 for many other editors).
fn list_indent_unit(lines: &[&str]) -> usize {
    fn gcd(a: usize, b: usize) -> usize {
        if b == 0 { a } else { gcd(b, a % b) }
    }
    let mut in_code: Option<usize> = None;
    let mut unit = 0;
    for line in lines {
        let content = line.trim_start();
        match in_code {
            Some(ticks) => {
                if closes_fence(content, ticks) {
                    in_code = None;
                }
            }
            None => match block_start(content) {
                Some(BlockStart::Fence(ticks)) => in_code = Some(ticks),
                Some(BlockStart::ListItem { .. }) if leading_width(line) > 0 => unit = gcd(unit, leading_width(line)),
                _ => {}
            },
        }
    }
    unit.max(2)
}

fn closes_fence(content: &str, ticks: usize) -> bool {
    let content = content.trim_end();
    content.len() >= ticks && content.bytes().all(|b| b == b'`')
}

/// Parses Markdown into blocks with fresh ids. Accepts this module's output
/// exactly and common Markdown (`*`/`+` bullets, `1)` lists, 4-space nesting,
/// GitHub alerts, fenced code with a language) leniently.
pub fn from_markdown(markdown: &str) -> Vec<Block> {
    let lines: Vec<&str> = markdown.lines().collect();
    let unit = list_indent_unit(&lines);
    let mut blocks = Vec::new();
    // Block that may still receive continuation lines.
    let mut open: Option<Block> = None;
    let mut index = 0;

    while index < lines.len() {
        let line = lines[index];
        index += 1;
        let content = line.trim();
        if content.is_empty() {
            blocks.extend(open.take());
            continue;
        }

        if let Some(block) = open.as_mut().filter(|block| is_quote_like(block.kind))
            && let Some(rest) = content.strip_prefix('>')
        {
            append_line(block, unescape(rest.strip_prefix(' ').unwrap_or(rest)));
            continue;
        }

        let Some(start) = block_start(content) else {
            let text = unescape(content);
            match open.as_mut() {
                Some(block) => append_line(block, text),
                None => open = Some(Block::paragraph(text)),
            }
            continue;
        };

        blocks.extend(open.take());
        match start {
            BlockStart::Heading(kind, text) => blocks.push(Block::new(kind, unescape(text))),
            BlockStart::Divider => blocks.push(Block::new(BlockKind::Divider, "")),
            BlockStart::Fence(ticks) => {
                let mut code = Vec::new();
                while index < lines.len() {
                    let line = lines[index];
                    index += 1;
                    if closes_fence(line.trim_start(), ticks) {
                        break;
                    }
                    code.push(line);
                }
                blocks.push(Block::new(BlockKind::Code, code.join("\n")));
            }
            BlockStart::ListItem { kind, checked, text } => {
                let indent = u8::try_from(leading_width(line) / unit).unwrap_or(u8::MAX);
                open = Some(Block { checked, indent, ..Block::new(kind, unescape(text)) });
            }
            BlockStart::Quote(rest) => {
                let (kind, text) = quote_kind(rest);
                open = Some(Block::new(kind, text));
            }
        }
    }
    blocks.extend(open);
    blocks
}

fn append_line(block: &mut Block, line: &str) {
    if !block.text.is_empty() {
        block.text.push('\n');
    }
    block.text.push_str(line);
}

// ---------------------------------------------------------------------------
// Inline Markdown
// ---------------------------------------------------------------------------

/// A run of text with uniform inline formatting.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct InlineSpan {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    /// Target URL when the span is part of a link.
    pub link: Option<String>,
}

impl InlineSpan {
    fn same_style(&self, other: &InlineSpan) -> bool {
        self.bold == other.bold && self.italic == other.italic && self.code == other.code && self.link == other.link
    }
}

/// Splits inline Markdown into formatted spans: `**bold**`, `*italic*` /
/// `_italic_` (nestable), `` `code` ``, `[text](url)` and backslash escapes.
/// Delimiters follow CommonMark's flanking rules, so `2 * 3 * 4` and
/// `snake_case_name` stay literal, and unmatched markers are kept as text.
pub fn parse_inline(text: &str) -> Vec<InlineSpan> {
    if !text.contains(['*', '_', '`', '[', '\\']) {
        return if text.is_empty() {
            Vec::new()
        } else {
            vec![InlineSpan { text: text.to_string(), ..InlineSpan::default() }]
        };
    }
    let chars: Vec<char> = text.chars().collect();
    let mut spans = Vec::new();
    flatten(&parse_nodes(&chars), &InlineSpan::default(), &mut spans);
    spans
}

/// Plain text of inline Markdown (the concatenated span texts).
pub fn strip_inline_markdown(text: &str) -> String {
    parse_inline(text).into_iter().map(|span| span.text).collect()
}

enum Node {
    Text(String),
    Code(String),
    Emphasis { strong: bool, children: Vec<Node> },
    Link { url: String, children: Vec<Node> },
}

/// A run of `*` or `_` that may open and/or close emphasis.
struct Delimiter {
    marker: char,
    /// Characters not yet consumed by matched emphasis.
    count: usize,
    original: usize,
    can_open: bool,
    can_close: bool,
}

enum Piece {
    Node(Node),
    Delimiter(Delimiter),
}

fn parse_nodes(chars: &[char]) -> Vec<Node> {
    process_emphasis(tokenize(chars))
}

fn tokenize(chars: &[char]) -> Vec<Piece> {
    let mut pieces = Vec::new();
    let mut text = String::new();
    let flush = |text: &mut String, pieces: &mut Vec<Piece>| {
        if !text.is_empty() {
            pieces.push(Piece::Node(Node::Text(std::mem::take(text))));
        }
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' if chars.get(i + 1).is_some_and(char::is_ascii_punctuation) => {
                text.push(chars[i + 1]);
                i += 2;
            }
            '`' => {
                let ticks = run_length(chars, i);
                match find_code_close(chars, i + ticks, ticks) {
                    Some(close) => {
                        flush(&mut text, &mut pieces);
                        pieces.push(Piece::Node(Node::Code(code_content(&chars[i + ticks..close]))));
                        i = close + ticks;
                    }
                    None => {
                        text.extend(&chars[i..i + ticks]);
                        i += ticks;
                    }
                }
            }
            '[' => match parse_link(chars, i) {
                Some((label_end, url, end)) => {
                    flush(&mut text, &mut pieces);
                    pieces.push(Piece::Node(Node::Link { url, children: parse_nodes(&chars[i + 1..label_end]) }));
                    i = end;
                }
                None => {
                    text.push('[');
                    i += 1;
                }
            },
            '*' | '_' => {
                let count = run_length(chars, i);
                let before = i.checked_sub(1).map(|j| chars[j]);
                let after = chars.get(i + count).copied();
                let (can_open, can_close) = flanking(c, before, after);
                flush(&mut text, &mut pieces);
                pieces.push(Piece::Delimiter(Delimiter { marker: c, count, original: count, can_open, can_close }));
                i += count;
            }
            _ => {
                text.push(c);
                i += 1;
            }
        }
    }
    flush(&mut text, &mut pieces);
    pieces
}

fn run_length(chars: &[char], start: usize) -> usize {
    chars[start..].iter().take_while(|&&c| c == chars[start]).count()
}

/// Start of the next backtick run of exactly `ticks` characters.
fn find_code_close(chars: &[char], from: usize, ticks: usize) -> Option<usize> {
    let mut i = from;
    while i < chars.len() {
        if chars[i] == '`' {
            let run = run_length(chars, i);
            if run == ticks {
                return Some(i);
            }
            i += run;
        } else {
            i += 1;
        }
    }
    None
}

/// Code span content; one surrounding space is stripped so `` ` `` `` can be written.
fn code_content(chars: &[char]) -> String {
    let content: String = chars.iter().collect();
    match content.strip_prefix(' ').and_then(|c| c.strip_suffix(' ')) {
        Some(inner) if !inner.trim().is_empty() => inner.to_string(),
        _ => content,
    }
}

/// `[label](url)` starting at `open`: returns (index of `]`, url, index after `)`).
fn parse_link(chars: &[char], open: usize) -> Option<(usize, String, usize)> {
    let mut depth = 0usize;
    let mut i = open;
    let label_end = loop {
        match chars.get(i)? {
            '\\' => i += 1,
            '`' => {
                let ticks = run_length(chars, i);
                i = find_code_close(chars, i + ticks, ticks).map_or(i + ticks - 1, |close| close + ticks - 1);
            }
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    break i;
                }
            }
            _ => {}
        }
        i += 1;
    };
    if chars.get(label_end + 1) != Some(&'(') {
        return None;
    }
    let mut depth = 0usize;
    let mut i = label_end + 1;
    let url_end = loop {
        match chars.get(i)? {
            '\\' => i += 1,
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    break i;
                }
            }
            _ => {}
        }
        i += 1;
    };
    let destination: String = chars[label_end + 2..url_end].iter().collect();
    // A link title (`[a](url "title")`) is accepted and ignored.
    let url = destination.split_whitespace().next()?;
    let url = url.strip_prefix('<').and_then(|u| u.strip_suffix('>')).unwrap_or(url);
    Some((label_end, url.to_string(), url_end + 1))
}

/// CommonMark left/right-flanking rules: whether a delimiter run can open / close emphasis.
fn flanking(marker: char, before: Option<char>, after: Option<char>) -> (bool, bool) {
    let whitespace = |c: Option<char>| c.is_none_or(char::is_whitespace);
    let punctuation = |c: Option<char>| c.is_some_and(|c| !c.is_alphanumeric() && !c.is_whitespace());
    let left = !whitespace(after) && (!punctuation(after) || whitespace(before) || punctuation(before));
    let right = !whitespace(before) && (!punctuation(before) || whitespace(after) || punctuation(after));
    if marker == '*' {
        (left, right)
    } else {
        // `_` never opens or closes inside a word.
        (left && (!right || punctuation(before)), right && (!left || punctuation(after)))
    }
}

/// Matches delimiter runs into emphasis nodes (CommonMark "process emphasis").
fn process_emphasis(mut pieces: Vec<Piece>) -> Vec<Node> {
    let mut i = 0;
    while i < pieces.len() {
        let Piece::Delimiter(closer) = &pieces[i] else {
            i += 1;
            continue;
        };
        if !closer.can_close || closer.count == 0 {
            i += 1;
            continue;
        }
        let opener = (0..i).rev().find(|&j| match &pieces[j] {
            Piece::Delimiter(opener) => {
                opener.marker == closer.marker && opener.can_open && opener.count > 0 && !rule_of_three(opener, closer)
            }
            Piece::Node(_) => false,
        });
        let Some(j) = opener else {
            i += 1;
            continue;
        };

        let used = match (&pieces[j], &pieces[i]) {
            (Piece::Delimiter(o), Piece::Delimiter(c)) if o.count >= 2 && c.count >= 2 => 2,
            _ => 1,
        };
        let inner = pieces.drain(j + 1..i).collect();
        pieces.insert(j + 1, Piece::Node(Node::Emphasis { strong: used == 2, children: into_nodes(inner) }));
        let mut closer_index = j + 2;
        for index in [j, closer_index] {
            if let Piece::Delimiter(delimiter) = &mut pieces[index] {
                delimiter.count -= used;
            }
        }
        if matches!(&pieces[j], Piece::Delimiter(d) if d.count == 0) {
            pieces.remove(j);
            closer_index -= 1;
        }
        if matches!(&pieces[closer_index], Piece::Delimiter(d) if d.count == 0) {
            pieces.remove(closer_index);
        }
        i = closer_index;
    }
    into_nodes(pieces)
}

/// "Rule of 3": a run that can both open and close only pairs with another run
/// when their combined length is not a multiple of 3 (so `*a**b*` stays sane).
fn rule_of_three(opener: &Delimiter, closer: &Delimiter) -> bool {
    (opener.can_close || closer.can_open)
        && (opener.original + closer.original).is_multiple_of(3)
        && !(opener.original.is_multiple_of(3) && closer.original.is_multiple_of(3))
}

/// Unmatched delimiters become literal text.
fn into_nodes(pieces: Vec<Piece>) -> Vec<Node> {
    pieces
        .into_iter()
        .filter_map(|piece| match piece {
            Piece::Node(node) => Some(node),
            Piece::Delimiter(d) => (d.count > 0).then(|| Node::Text(d.marker.to_string().repeat(d.count))),
        })
        .collect()
}

fn flatten(nodes: &[Node], style: &InlineSpan, out: &mut Vec<InlineSpan>) {
    for node in nodes {
        match node {
            Node::Text(text) => push_span(out, style, text),
            Node::Code(text) => push_span(out, &InlineSpan { code: true, ..style.clone() }, text),
            Node::Emphasis { strong, children } => {
                let nested = if *strong {
                    InlineSpan { bold: true, ..style.clone() }
                } else {
                    InlineSpan { italic: true, ..style.clone() }
                };
                flatten(children, &nested, out);
            }
            Node::Link { url, children } => {
                flatten(children, &InlineSpan { link: Some(url.clone()), ..style.clone() }, out);
            }
        }
    }
}

fn push_span(out: &mut Vec<InlineSpan>, style: &InlineSpan, text: &str) {
    if text.is_empty() {
        return;
    }
    match out.last_mut() {
        Some(last) if last.same_style(style) => last.text.push_str(text),
        _ => out.push(InlineSpan { text: text.to_string(), ..style.clone() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(kind: BlockKind, text: &str) -> Block {
        Block::new(kind, text)
    }

    fn indented(kind: BlockKind, text: &str, indent: u8) -> Block {
        Block { indent, ..Block::new(kind, text) }
    }

    /// Block content without ids, for comparing round trips.
    fn shape(blocks: &[Block]) -> Vec<(BlockKind, String, bool, u8)> {
        blocks.iter().map(|b| (b.kind, b.text.clone(), b.checked, b.indent)).collect()
    }

    fn assert_round_trip(blocks: &[Block]) {
        let markdown = to_markdown(blocks);
        let parsed = from_markdown(&markdown);
        assert_eq!(shape(&parsed), shape(blocks), "markdown was:\n{markdown}");
        assert_eq!(to_markdown(&parsed), markdown);
    }

    fn span(text: &str) -> InlineSpan {
        InlineSpan { text: text.into(), ..InlineSpan::default() }
    }

    fn bold(text: &str) -> InlineSpan {
        InlineSpan { bold: true, ..span(text) }
    }

    fn italic(text: &str) -> InlineSpan {
        InlineSpan { italic: true, ..span(text) }
    }

    fn code(text: &str) -> InlineSpan {
        InlineSpan { code: true, ..span(text) }
    }

    // --- statistics -------------------------------------------------------

    #[test]
    fn counts_only_spoken_words_without_markup() {
        let blocks = vec![
            block(BlockKind::Heading1, "Intro heading words"),
            block(BlockKind::Paragraph, "Hello **there**, *dear* [viewer](https://x.dev) — welcome!"),
            block(BlockKind::Bulleted, "one two"),
            block(BlockKind::Numbered, "three"),
            block(BlockKind::Quote, "four five"),
            block(BlockKind::Todo, "not spoken"),
            block(BlockKind::BRoll, "drone shot"),
            block(BlockKind::OnScreen, "Title"),
            block(BlockKind::Callout, "note"),
            block(BlockKind::Code, "let x = 1;"),
            block(BlockKind::Divider, ""),
        ];
        // "Hello there, dear viewer welcome!" = 5, bullets 2 + 1, quote 2.
        assert_eq!(spoken_word_count(&blocks), 10);
    }

    #[test]
    fn runtime_uses_words_per_minute() {
        let words = vec!["word"; 150].join(" ");
        let blocks = vec![block(BlockKind::Paragraph, &words)];
        assert_eq!(estimated_runtime_secs(&blocks, DEFAULT_WPM), 60);
        assert_eq!(estimated_runtime_secs(&blocks, 300), 30);
        assert_eq!(estimated_runtime_secs(&blocks, 0), 0);
        assert_eq!(runtime_secs_for_words(1240, 150), 496);
        assert_eq!(runtime_secs_for_words(1, 150), 0);
        assert_eq!(runtime_secs_for_words(2, 150), 1);
    }

    #[test]
    fn formats_runtime() {
        assert_eq!(format_runtime(0), "0s");
        assert_eq!(format_runtime(45), "45s");
        assert_eq!(format_runtime(496), "8m 16s");
        assert_eq!(format_runtime(480), "8m");
        assert_eq!(format_runtime(3_900), "1h 5m");
    }

    #[test]
    fn outline_lists_non_empty_headings() {
        let blocks = vec![
            block(BlockKind::Heading1, "Hook"),
            block(BlockKind::Paragraph, "text"),
            block(BlockKind::Heading2, "Step **1**"),
            block(BlockKind::Heading3, "  "),
            block(BlockKind::Heading3, "Detail"),
        ];
        let items = outline(&blocks);
        let flat: Vec<_> = items.iter().map(|i| (i.level, i.text.as_str())).collect();
        assert_eq!(flat, [(1, "Hook"), (2, "Step 1"), (3, "Detail")]);
        assert_eq!(items[1].block_id, blocks[2].id);
    }

    #[test]
    fn teleprompter_has_spoken_blocks_only() {
        let blocks = vec![
            block(BlockKind::Heading2, "Hook"),
            block(BlockKind::Paragraph, "Stop using **print** to debug."),
            block(BlockKind::BRoll, "screen recording"),
            block(BlockKind::Paragraph, "   "),
            block(BlockKind::Bulleted, "Use `breakpoint()` instead"),
        ];
        assert_eq!(teleprompter_text(&blocks), "Stop using print to debug.\n\nUse breakpoint() instead");
    }

    // --- markdown ---------------------------------------------------------

    #[test]
    fn exports_every_kind() {
        let blocks = vec![
            block(BlockKind::Heading1, "Title"),
            block(BlockKind::Heading2, "Section"),
            block(BlockKind::Heading3, "Sub"),
            block(BlockKind::Paragraph, "Some **bold** text."),
            block(BlockKind::Bulleted, "bullet"),
            indented(BlockKind::Bulleted, "nested", 1),
            block(BlockKind::Numbered, "first"),
            block(BlockKind::Numbered, "second"),
            Block { checked: true, ..block(BlockKind::Todo, "done") },
            block(BlockKind::Todo, "open"),
            block(BlockKind::Quote, "quoted"),
            block(BlockKind::Callout, "heads up"),
            block(BlockKind::BRoll, "drone shot"),
            block(BlockKind::OnScreen, "Subscribe"),
            block(BlockKind::Code, "fn main() {}"),
            block(BlockKind::Divider, ""),
        ];
        let expected = "# Title\n\n## Section\n\n### Sub\n\nSome **bold** text.\n\n- bullet\n  - nested\n1. first\n\
                        2. second\n- [x] done\n- [ ] open\n\n> quoted\n\n> [!note] heads up\n\n> [!broll] drone shot\n\n\
                        > [!onscreen] Subscribe\n\n```\nfn main() {}\n```\n\n---\n";
        assert_eq!(to_markdown(&blocks), expected);
        assert_round_trip(&blocks);
    }

    #[test]
    fn round_trips_every_kind_with_indent_and_checked_todos() {
        let mut blocks: Vec<Block> =
            BlockKind::ALL.iter().map(|&kind| block(kind, if kind.has_text() { kind.label() } else { "" })).collect();
        blocks.extend([
            indented(BlockKind::Bulleted, "level 0", 0),
            indented(BlockKind::Bulleted, "level 1", 1),
            indented(BlockKind::Numbered, "level 2 numbered", 2),
            indented(BlockKind::Numbered, "level 2 again", 2),
            Block { checked: true, ..indented(BlockKind::Todo, "level 3 done", 3) },
            indented(BlockKind::Todo, "level 1 open", 1),
            indented(BlockKind::Numbered, "back to the top", 0),
        ]);
        assert_round_trip(&blocks);
    }

    #[test]
    fn numbers_restart_per_list_and_level() {
        let blocks = vec![
            block(BlockKind::Numbered, "a"),
            indented(BlockKind::Numbered, "a.1", 1),
            indented(BlockKind::Numbered, "a.2", 1),
            block(BlockKind::Numbered, "b"),
            block(BlockKind::Paragraph, "break"),
            block(BlockKind::Numbered, "c"),
        ];
        assert_eq!(to_markdown(&blocks), "1. a\n  1. a.1\n  2. a.2\n2. b\n\nbreak\n\n1. c\n");
        assert_round_trip(&blocks);
    }

    #[test]
    fn round_trips_multiline_text() {
        assert_round_trip(&[
            block(BlockKind::Paragraph, "line one\nline two"),
            block(BlockKind::Bulleted, "item\ncontinued"),
            block(BlockKind::Numbered, "step\n- continued"),
            indented(BlockKind::Todo, "nested todo\nmore", 1),
            block(BlockKind::Quote, "quote\n\nwith a gap"),
            block(BlockKind::BRoll, "cue\nsecond line"),
            block(BlockKind::Code, "fn main() {\n\n    println!(\"hi\");\n}\n"),
        ]);
    }

    #[test]
    fn escapes_text_that_looks_like_markup() {
        let blocks = vec![
            block(BlockKind::Paragraph, "# not a heading"),
            block(BlockKind::Paragraph, "- not a bullet\n1. not numbered\n> not a quote"),
            block(BlockKind::Paragraph, "---"),
            block(BlockKind::Paragraph, "```"),
            block(BlockKind::Paragraph, "\\ starts with a backslash"),
            block(BlockKind::Paragraph, "2024. A year"),
            block(BlockKind::Bulleted, "[ ] not a todo"),
            block(BlockKind::Bulleted, "- dash text"),
            block(BlockKind::Bulleted, "- -"),
            block(BlockKind::Quote, "[!broll] not a cue"),
            block(BlockKind::Heading2, "\\escaped"),
            block(BlockKind::Code, "```\nnested fence\n```"),
            block(BlockKind::Paragraph, "#hashtag and 3.14 stay plain"),
        ];
        let markdown = to_markdown(&blocks);
        assert!(markdown.starts_with("\\# not a heading\n\n\\- not a bullet\n\\1. not numbered\n\\> not a quote"));
        assert!(markdown.contains("\n````\n```\nnested fence\n```\n````\n"), "{markdown}");
        assert!(markdown.contains("\n#hashtag and 3.14 stay plain\n"), "{markdown}");
        assert_round_trip(&blocks);
    }

    #[test]
    fn round_trips_empty_blocks_except_paragraphs() {
        let blocks = vec![
            block(BlockKind::Heading1, ""),
            block(BlockKind::Paragraph, ""),
            block(BlockKind::Bulleted, ""),
            block(BlockKind::Todo, ""),
            block(BlockKind::Numbered, ""),
            block(BlockKind::Quote, ""),
            block(BlockKind::OnScreen, ""),
            block(BlockKind::Code, ""),
        ];
        let parsed = from_markdown(&to_markdown(&blocks));
        let expected: Vec<_> = blocks.into_iter().filter(|b| b.kind != BlockKind::Paragraph).collect();
        assert_eq!(shape(&parsed), shape(&expected));
    }

    #[test]
    fn imports_common_markdown_dialects() {
        let markdown = "Intro line\r\nwraps here\r\n\r\n* star bullet\r\n    + four-space nested\r\n        \
                        - deeper\r\n1) paren list\r\n\r\n> [!NOTE]\r\n> GitHub alert\r\n\r\n```rust\r\nlet a = 1;\r\n\
                        ```\r\n\r\n***\r\n#### Deep heading\r\n- [X] Done item\r\n";
        let parsed = from_markdown(markdown);
        assert_eq!(
            shape(&parsed),
            vec![
                (BlockKind::Paragraph, "Intro line\nwraps here".into(), false, 0),
                (BlockKind::Bulleted, "star bullet".into(), false, 0),
                (BlockKind::Bulleted, "four-space nested".into(), false, 1),
                (BlockKind::Bulleted, "deeper".into(), false, 2),
                (BlockKind::Numbered, "paren list".into(), false, 0),
                (BlockKind::Callout, "GitHub alert".into(), false, 0),
                (BlockKind::Code, "let a = 1;".into(), false, 0),
                (BlockKind::Divider, String::new(), false, 0),
                (BlockKind::Heading3, "Deep heading".into(), false, 0),
                (BlockKind::Todo, "Done item".into(), true, 0),
            ]
        );
    }

    #[test]
    fn imports_lazy_continuations_and_unclosed_fences() {
        let parsed = from_markdown("> quote\nlazy line\n\n- item\nlazy item line\n\n```\nnever closed\n");
        assert_eq!(
            shape(&parsed),
            vec![
                (BlockKind::Quote, "quote\nlazy line".into(), false, 0),
                (BlockKind::Bulleted, "item\nlazy item line".into(), false, 0),
                (BlockKind::Code, "never closed".into(), false, 0),
            ]
        );
        assert!(from_markdown("").is_empty());
        assert!(from_markdown("\n \n\t\n").is_empty());
    }

    #[test]
    fn imported_blocks_get_fresh_ids() {
        let parsed = from_markdown("one\n\ntwo\n");
        assert_eq!(parsed.len(), 2);
        assert_ne!(parsed[0].id, parsed[1].id);
    }

    // --- inline -----------------------------------------------------------

    #[test]
    fn plain_text_is_one_span() {
        assert_eq!(parse_inline("just text"), vec![span("just text")]);
        assert!(parse_inline("").is_empty());
    }

    #[test]
    fn parses_basic_inline_styles() {
        assert_eq!(
            parse_inline("a **b** *c* _d_ `e` [f](https://g.dev)"),
            vec![
                span("a "),
                bold("b"),
                span(" "),
                italic("c"),
                span(" "),
                italic("d"),
                span(" "),
                code("e"),
                span(" "),
                InlineSpan { link: Some("https://g.dev".into()), ..span("f") },
            ]
        );
    }

    #[test]
    fn parses_nested_bold_and_italic() {
        let both = InlineSpan { bold: true, ..italic("both") };
        assert_eq!(parse_inline("***both***"), vec![both.clone()]);
        assert_eq!(parse_inline("**bold _both_ bold**"), vec![bold("bold "), both.clone(), bold(" bold")]);
        assert_eq!(parse_inline("*it **both** it*"), vec![italic("it "), both.clone(), italic(" it")]);
        assert_eq!(parse_inline("***both** it*"), vec![both, italic(" it")]);
    }

    #[test]
    fn formatting_inside_links_and_code() {
        let url = Some("https://x.dev/a_(b)".to_string());
        assert_eq!(
            parse_inline("see [**bold** link](https://x.dev/a_(b) \"title\")!"),
            vec![
                span("see "),
                InlineSpan { bold: true, link: url.clone(), ..span("bold") },
                InlineSpan { link: url, ..span(" link") },
                span("!"),
            ]
        );
        assert_eq!(parse_inline("`**not bold**`"), vec![code("**not bold**")]);
        assert_eq!(parse_inline("``a ` b``"), vec![code("a ` b")]);
        assert_eq!(
            parse_inline("[`code` link](<u>)"),
            vec![
                InlineSpan { code: true, link: Some("u".into()), ..span("code") },
                InlineSpan { link: Some("u".into()), ..span(" link") }
            ]
        );
    }

    #[test]
    fn unmatched_markers_stay_literal() {
        assert_eq!(parse_inline("**unclosed"), vec![span("**unclosed")]);
        assert_eq!(parse_inline("2 * 3 * 4"), vec![span("2 * 3 * 4")]);
        assert_eq!(parse_inline("snake_case_name"), vec![span("snake_case_name")]);
        assert_eq!(parse_inline("`open code"), vec![span("`open code")]);
        assert_eq!(parse_inline("[no link] here"), vec![span("[no link] here")]);
        assert_eq!(parse_inline("[text](no close"), vec![span("[text](no close")]);
        assert_eq!(parse_inline("** spaced **"), vec![span("** spaced **")]);
        assert_eq!(parse_inline("*a **b*"), vec![span("*a *"), italic("b")]);
    }

    #[test]
    fn backslash_escapes_markers() {
        assert_eq!(parse_inline(r"\*not italic\*"), vec![span("*not italic*")]);
        assert_eq!(parse_inline(r"a \[b](c)"), vec![span("a [b](c)")]);
        assert_eq!(parse_inline(r"path\to"), vec![span(r"path\to")]);
    }

    #[test]
    fn intraword_star_emphasis_is_allowed() {
        assert_eq!(parse_inline("un*frigging*believable"), vec![span("un"), italic("frigging"), span("believable")]);
    }

    #[test]
    fn strip_returns_plain_text() {
        assert_eq!(
            strip_inline_markdown("**Bold** and [link](u) and `code` and *it*"),
            "Bold and link and code and it"
        );
        assert_eq!(strip_inline_markdown("2 * 3"), "2 * 3");
    }
}
