//! Inline Markdown inside a block's text.
//!
//! A block stores `**bold**`, `*italic*`, `` `code` `` and `[label](url)` as
//! plain text. [`RichText::parse`] turns it into the text a reader sees plus
//! styled spans that remember where each piece came from, so a click on the
//! rendered text can place the caret at the matching source offset.
//! [`toggle_marker`] implements the formatting shortcuts on the raw text.

use std::ops::Range;

/// Character styles a span can carry. Links are kept on the span itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InlineStyle {
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
}

/// A run of rendered text with one style, mapped back to the source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineSpan {
    /// Byte range in [`RichText::text`].
    pub range: Range<usize>,
    /// Byte offset of the span's first character in the source text. A span
    /// never crosses a Markdown marker, so the mapping inside it is linear.
    pub source_start: usize,
    pub style: InlineStyle,
    /// Destination of the link this span belongs to.
    pub link: Option<String>,
}

/// Block text with inline Markdown resolved.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RichText {
    text: String,
    spans: Vec<InlineSpan>,
    source_len: usize,
}

impl RichText {
    pub fn parse(source: &str) -> Self {
        let mut parser = Parser {
            source,
            rich: RichText { source_len: source.len(), ..Self::default() },
        };
        parser.parse(0..source.len(), InlineStyle::default(), None);
        parser.rich
    }

    /// The text without Markdown markers.
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn spans(&self) -> &[InlineSpan] {
        &self.spans
    }

    /// Rendered ranges of links with their destinations, in reading order.
    /// Adjacent spans of one link are reported separately.
    pub fn links(&self) -> impl Iterator<Item = (Range<usize>, &str)> {
        self.spans
            .iter()
            .filter_map(|span| span.link.as_deref().map(|url| (span.range.clone(), url)))
    }

    /// Maps a byte offset in the rendered text to the source text.
    pub fn source_offset(&self, rendered: usize) -> usize {
        // Prefer the span that starts at the offset so a caret between two
        // spans lands after the marker that opens the second one.
        let span = self
            .spans
            .iter()
            .find(|span| span.range.start <= rendered && rendered < span.range.end)
            .or_else(|| self.spans.iter().rev().find(|span| span.range.end <= rendered));
        match span {
            Some(span) => span.source_start + rendered.min(span.range.end) - span.range.start,
            None if rendered == 0 => 0,
            None => self.source_len,
        }
    }
}

struct Parser<'a> {
    source: &'a str,
    rich: RichText,
}

impl Parser<'_> {
    fn parse(&mut self, range: Range<usize>, style: InlineStyle, link: Option<&str>) {
        let source = self.source;
        let end = range.end;
        let mut ix = range.start;
        while ix < end {
            let rest = &source[ix..end];
            let next = rest.chars().next().unwrap_or_default();

            if next == '\\'
                && let Some(escaped) = rest[1..].chars().next().filter(|c| is_escapable(*c))
            {
                let start = ix + 1;
                self.push(start..start + escaped.len_utf8(), style, link);
                ix = start + escaped.len_utf8();
                continue;
            }

            if next == '`'
                && let Some(close) = rest[1..].find('`').filter(|close| *close > 0)
            {
                let inner = ix + 1..ix + 1 + close;
                self.push(inner, InlineStyle { code: true, ..style }, link);
                ix += close + 2;
                continue;
            }

            if rest.starts_with("**")
                && let Some(close) = closing_double_star(&rest[2..])
            {
                let inner = ix + 2..ix + 2 + close;
                self.parse(inner, InlineStyle { bold: true, ..style }, link);
                ix += close + 4;
                continue;
            }

            if next == '*'
                && !rest.starts_with("**")
                && let Some(close) = closing_single_star(&rest[1..])
            {
                let inner = ix + 1..ix + 1 + close;
                self.parse(inner, InlineStyle { italic: true, ..style }, link);
                ix += close + 2;
                continue;
            }

            if next == '['
                && link.is_none()
                && let Some(parsed) = parse_link(rest)
            {
                let label = ix + 1..ix + 1 + parsed.label_len;
                let url = &rest[parsed.url.clone()];
                self.parse(label, style, Some(url));
                ix += parsed.len;
                continue;
            }

            self.push(ix..ix + next.len_utf8(), style, link);
            ix += next.len_utf8();
        }
    }

    /// Appends source text, extending the previous span when it continues it.
    fn push(&mut self, source: Range<usize>, style: InlineStyle, link: Option<&str>) {
        let piece = &self.source[source.clone()];
        let start = self.rich.text.len();
        self.rich.text.push_str(piece);
        if let Some(last) = self.rich.spans.last_mut()
            && last.style == style
            && last.link.as_deref() == link
            && last.range.end == start
            && last.source_start + last.range.len() == source.start
        {
            last.range.end += piece.len();
            return;
        }
        self.rich.spans.push(InlineSpan {
            range: start..start + piece.len(),
            source_start: source.start,
            style,
            link: link.map(str::to_owned),
        });
    }
}

fn is_escapable(c: char) -> bool {
    matches!(c, '\\' | '*' | '`' | '[' | ']' | '(' | ')' | '_' | '#')
}

/// Offset of the `**` that closes a bold run opened just before `rest`.
///
/// The content must not start or end with whitespace. In a longer run of
/// stars (`***`) the last two close the bold, leaving the first one to an
/// italic run nested inside.
fn closing_double_star(rest: &str) -> Option<usize> {
    if rest.starts_with(char::is_whitespace) || rest.starts_with("**") {
        return None;
    }
    let bytes = rest.as_bytes();
    let mut ix = 1;
    while ix < bytes.len() {
        if bytes[ix] == b'*' {
            let run = bytes[ix..].iter().take_while(|b| **b == b'*').count();
            if run >= 2 && !rest[..ix].ends_with(char::is_whitespace) {
                return Some(ix + run - 2);
            }
            ix += run;
        } else {
            ix += 1;
        }
    }
    None
}

/// Offset of the single `*` that closes an italic run opened just before
/// `rest`. Runs of two stars inside belong to bold text and are skipped.
fn closing_single_star(rest: &str) -> Option<usize> {
    if rest.starts_with(char::is_whitespace) {
        return None;
    }
    let bytes = rest.as_bytes();
    let mut ix = 0;
    while ix < bytes.len() {
        if bytes[ix] == b'*' {
            let run = bytes[ix..].iter().take_while(|b| **b == b'*').count();
            if ix > 0 && run % 2 == 1 && !rest[..ix].ends_with(char::is_whitespace) {
                return Some(ix + run - 1);
            }
            ix += run;
        } else {
            ix += 1;
        }
    }
    None
}

struct ParsedLink {
    label_len: usize,
    /// Range of the destination inside the parsed text.
    url: Range<usize>,
    /// Bytes consumed, from `[` through `)`.
    len: usize,
}

/// Parses `[label](url)` at the start of `rest`.
fn parse_link(rest: &str) -> Option<ParsedLink> {
    let mut depth = 0usize;
    let mut label_end = None;
    for (ix, c) in rest.char_indices().skip(1) {
        match c {
            '[' => depth += 1,
            ']' if depth == 0 => {
                label_end = Some(ix);
                break;
            }
            ']' => depth -= 1,
            '\n' => return None,
            _ => {}
        }
    }
    let label_end = label_end.filter(|end| *end > 1)?;
    let after = &rest[label_end + 1..];
    if !after.starts_with('(') {
        return None;
    }
    let close = after.find(')')?;
    let url_start = label_end + 2;
    let url = url_start..label_end + 1 + close;
    if url.is_empty() || rest[url.clone()].contains(char::is_whitespace) {
        return None;
    }
    Some(ParsedLink { label_len: label_end - 1, url, len: label_end + close + 2 })
}

/// An inline formatting marker toggled by a keyboard shortcut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Marker {
    Bold,
    Italic,
    Code,
}

impl Marker {
    fn text(self) -> &'static str {
        match self {
            Marker::Bold => "**",
            Marker::Italic => "*",
            Marker::Code => "`",
        }
    }

    /// Whether runs of `before` and `after` marker characters around a
    /// selection already apply this marker.
    fn is_applied(self, before: usize, after: usize) -> bool {
        match self {
            Marker::Bold => before >= 2 && after >= 2,
            Marker::Italic => before % 2 == 1 && after % 2 == 1,
            Marker::Code => before >= 1 && after >= 1,
        }
    }
}

/// Wraps `selection` of `text` in `marker`, or unwraps it when the marker is
/// already applied around or inside the selection. Returns the new text and
/// the selection over the same characters. An empty selection inserts an
/// empty pair with the caret between the markers.
pub fn toggle_marker(text: &str, selection: Range<usize>, marker: Marker) -> (String, Range<usize>) {
    let mark = marker.text();
    let mark_char = mark.as_bytes()[0];
    let len = mark.len();
    let Range { start, end } = selection;

    let before = text.as_bytes()[..start].iter().rev().take_while(|b| **b == mark_char).count();
    let after = text.as_bytes()[end..].iter().take_while(|b| **b == mark_char).count();
    if marker.is_applied(before, after) {
        let unwrapped = [&text[..start - len], &text[start..end], &text[end + len..]].concat();
        return (unwrapped, start - len..end - len);
    }

    let selected = &text[start..end];
    let inner_before = selected.bytes().take_while(|b| *b == mark_char).count();
    let inner_after = selected.bytes().rev().take_while(|b| *b == mark_char).count();
    if selected.len() >= 2 * len && inner_before < selected.len() && marker.is_applied(inner_before, inner_after) {
        let unwrapped = [&text[..start], &selected[len..selected.len() - len], &text[end..]].concat();
        return (unwrapped, start..end - 2 * len);
    }

    let wrapped = [&text[..start], mark, selected, mark, &text[end..]].concat();
    (wrapped, start + len..end + len)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn styled(rich: &RichText) -> Vec<(&str, InlineStyle, Option<&str>)> {
        rich.spans()
            .iter()
            .map(|span| (&rich.text()[span.range.clone()], span.style, span.link.as_deref()))
            .collect()
    }

    const PLAIN: InlineStyle = InlineStyle { bold: false, italic: false, code: false };
    const BOLD: InlineStyle = InlineStyle { bold: true, italic: false, code: false };
    const ITALIC: InlineStyle = InlineStyle { bold: false, italic: true, code: false };
    const CODE: InlineStyle = InlineStyle { bold: false, italic: false, code: true };
    const BOLD_ITALIC: InlineStyle = InlineStyle { bold: true, italic: true, code: false };

    #[test]
    fn plain_text_is_one_span() {
        let rich = RichText::parse("Hello world");
        assert_eq!(rich.text(), "Hello world");
        assert_eq!(styled(&rich), vec![("Hello world", PLAIN, None)]);
    }

    #[test]
    fn parses_bold_italic_and_code() {
        let rich = RichText::parse("a **b** *c* `d`");
        assert_eq!(rich.text(), "a b c d");
        assert_eq!(
            styled(&rich),
            vec![
                ("a ", PLAIN, None),
                ("b", BOLD, None),
                (" ", PLAIN, None),
                ("c", ITALIC, None),
                (" ", PLAIN, None),
                ("d", CODE, None),
            ]
        );
    }

    #[test]
    fn nests_italic_inside_bold_and_triple_stars() {
        let rich = RichText::parse("**x *y* z**");
        assert_eq!(rich.text(), "x y z");
        assert_eq!(
            styled(&rich),
            vec![("x ", BOLD, None), ("y", BOLD_ITALIC, None), (" z", BOLD, None)]
        );

        let rich = RichText::parse("***both***");
        assert_eq!(rich.text(), "both");
        assert_eq!(styled(&rich), vec![("both", BOLD_ITALIC, None)]);

        let rich = RichText::parse("*a **b** c*");
        assert_eq!(rich.text(), "a b c");
        assert_eq!(
            styled(&rich),
            vec![("a ", ITALIC, None), ("b", BOLD_ITALIC, None), (" c", ITALIC, None)]
        );
    }

    #[test]
    fn code_spans_are_literal() {
        let rich = RichText::parse("`**not bold**`");
        assert_eq!(rich.text(), "**not bold**");
        assert_eq!(styled(&rich), vec![("**not bold**", CODE, None)]);
    }

    #[test]
    fn parses_links_with_formatted_labels() {
        let rich = RichText::parse("see [the **docs**](https://example.com) now");
        assert_eq!(rich.text(), "see the docs now");
        assert_eq!(
            styled(&rich),
            vec![
                ("see ", PLAIN, None),
                ("the ", PLAIN, Some("https://example.com")),
                ("docs", BOLD, Some("https://example.com")),
                (" now", PLAIN, None),
            ]
        );
        let links: Vec<_> = rich.links().collect();
        assert_eq!(links, vec![(4..8, "https://example.com"), (8..12, "https://example.com")]);
    }

    #[test]
    fn unmatched_markers_stay_literal() {
        for source in ["2 * 3 = 6", "**open", "a * b *", "`tick", "[label]", "[label](no url)", "[](x)", "** spaced**"] {
            let rich = RichText::parse(source);
            assert_eq!(rich.text(), source, "{source}");
            assert_eq!(styled(&rich), vec![(source, PLAIN, None)], "{source}");
        }
    }

    #[test]
    fn escapes_render_the_marker() {
        let rich = RichText::parse(r"\*not italic\*");
        assert_eq!(rich.text(), "*not italic*");
        assert!(rich.spans().iter().all(|span| span.style == PLAIN));
    }

    #[test]
    fn handles_multibyte_text() {
        let rich = RichText::parse("中 **文** 🦀");
        assert_eq!(rich.text(), "中 文 🦀");
        assert_eq!(styled(&rich)[1], ("文", BOLD, None));
    }

    #[test]
    fn maps_rendered_offsets_to_source() {
        let source = "ab **cd** [ef](u) g";
        let rich = RichText::parse(source);
        assert_eq!(rich.text(), "ab cd ef g");
        // Before "c" -> after the opening marker.
        assert_eq!(rich.source_offset(3), 5);
        // Inside "cd".
        assert_eq!(rich.source_offset(4), 6);
        // Before "e" in the link label.
        assert_eq!(rich.source_offset(6), 11);
        // End of text.
        assert_eq!(rich.source_offset(rich.text().len()), source.len());
        assert_eq!(rich.source_offset(0), 0);
        assert_eq!(RichText::parse("").source_offset(0), 0);
    }

    #[test]
    fn toggles_bold_around_a_selection() {
        let (text, selection) = toggle_marker("say hello now", 4..9, Marker::Bold);
        assert_eq!(text, "say **hello** now");
        assert_eq!(&text[selection.clone()], "hello");

        let (text, selection) = toggle_marker(&text, selection, Marker::Bold);
        assert_eq!(text, "say hello now");
        assert_eq!(selection, 4..9);
    }

    #[test]
    fn unwraps_markers_inside_the_selection() {
        let (text, selection) = toggle_marker("x `code` y", 2..8, Marker::Code);
        assert_eq!(text, "x code y");
        assert_eq!(&text[selection], "code");
    }

    #[test]
    fn italic_and_bold_combine() {
        let (text, selection) = toggle_marker("**word**", 2..6, Marker::Italic);
        assert_eq!(text, "***word***");
        assert_eq!(&text[selection.clone()], "word");
        let (text, selection) = toggle_marker(&text, selection, Marker::Italic);
        assert_eq!(text, "**word**");
        assert_eq!(selection, 2..6);
        let (text, _) = toggle_marker("*word*", 1..5, Marker::Bold);
        assert_eq!(text, "***word***");
    }

    #[test]
    fn empty_selection_inserts_a_pair() {
        let (text, selection) = toggle_marker("ab", 1..1, Marker::Italic);
        assert_eq!(text, "a**b");
        assert_eq!(selection, 2..2);
    }
}
