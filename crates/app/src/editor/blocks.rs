//! Structural edits on a document's block list.
//!
//! Pure functions over `Vec<Block>` so the editing rules are testable without
//! a window. Offsets are UTF-8 byte offsets into `Block::text`.

use std::ops::Range;

use rushes_core::{Block, BlockId, BlockKind};

use super::markdown::is_list;

/// Deepest nesting level of a list item.
pub const MAX_INDENT: u8 = 3;

pub fn position(blocks: &[Block], id: BlockId) -> Option<usize> {
    blocks.iter().position(|block| block.id == id)
}

/// Whether Enter on an empty block of this kind turns it into a paragraph
/// instead of adding a new block.
pub fn exits_when_empty(kind: BlockKind) -> bool {
    matches!(
        kind,
        BlockKind::Bulleted
            | BlockKind::Numbered
            | BlockKind::Todo
            | BlockKind::Quote
            | BlockKind::Callout
            | BlockKind::BRoll
            | BlockKind::OnScreen
    )
}

/// The block Enter creates after `block`: list items continue the list,
/// everything else is followed by a paragraph.
pub fn continuation(block: &Block, text: impl Into<String>) -> Block {
    let mut next = Block::paragraph(text);
    if is_list(block.kind) {
        next.kind = block.kind;
        next.indent = block.indent;
    }
    next
}

/// Splits the block at `ix` at byte `at`; the text after `at` moves to a new
/// block inserted after it. Returns the new block's index.
pub fn split(blocks: &mut Vec<Block>, ix: usize, at: usize) -> usize {
    let tail = blocks[ix].text.split_off(at);
    let next = continuation(&blocks[ix], tail);
    blocks.insert(ix + 1, next);
    ix + 1
}

/// Pastes `pasted` (at least one block) over the byte range `selection` of
/// the text block at `ix`, like a split at the caret with the pasted blocks
/// in between:
///
/// - an empty block, or a paragraph pasted into at its start, becomes the
///   first pasted block;
/// - otherwise a pasted paragraph joins the text before the caret, while any
///   other first block (a cue, callout, heading, list item, divider or code)
///   stays a block of its own below that text;
/// - the text after the caret joins the last pasted block when it is of the
///   same kind as the block pasted into, and otherwise stays in a block of
///   that kind, so a cue's text is never read out as a paragraph.
///
/// Returns the index of the block the caret goes to and its byte offset there:
/// the end of the pasted text.
pub fn paste(blocks: &mut Vec<Block>, ix: usize, selection: Range<usize>, mut pasted: Vec<Block>) -> (usize, usize) {
    let target = blocks[ix].clone();
    let (before, after) = (&target.text[..selection.start], &target.text[selection.end..]);
    let mut kept = Vec::with_capacity(pasted.len() + 2);
    if before.is_empty() && (target.kind == BlockKind::Paragraph || after.is_empty()) {
        let first = pasted.remove(0);
        kept.push(Block { id: target.id, ..first });
    } else if pasted[0].kind == BlockKind::Paragraph {
        let first = pasted.remove(0);
        kept.push(Block { text: format!("{before}{}", first.text), ..target.clone() });
    } else if !before.is_empty() {
        kept.push(Block { text: before.to_owned(), ..target.clone() });
    }
    // An empty start of the target is dropped: the pasted blocks take its place.
    let dropped_target = kept.is_empty();
    kept.extend(pasted);

    let last = kept.last_mut().expect("at least one pasted block");
    let caret = if after.is_empty() {
        // The caret needs a text block after a divider.
        if !last.kind.has_text() {
            kept.push(Block::paragraph(""));
        }
        (kept.len() - 1, kept.last().map_or(0, |block| block.text.len()))
    } else if last.kind == target.kind {
        let offset = last.text.len();
        last.text.push_str(after);
        (kept.len() - 1, offset)
    } else {
        let id = if dropped_target { target.id } else { BlockId::new() };
        let checked = dropped_target && target.checked;
        kept.push(Block { id, text: after.to_owned(), checked, ..target });
        (kept.len() - 1, 0)
    };
    blocks.splice(ix..=ix, kept);
    (ix + caret.0, caret.1)
}

/// Where the caret goes after two blocks were merged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Merge {
    /// The text was joined into the block at `into`; the join is at `offset`.
    Joined { into: usize, offset: usize },
    /// A neighbouring divider was deleted; the edited block is now at `ix`.
    RemovedDivider { ix: usize },
}

/// Backspace at the start of the block at `ix`: deletes a divider before it,
/// or appends its text to the previous text block.
pub fn merge_into_previous(blocks: &mut Vec<Block>, ix: usize) -> Option<Merge> {
    let previous = ix.checked_sub(1)?;
    if blocks[previous].kind == BlockKind::Divider {
        blocks.remove(previous);
        return Some(Merge::RemovedDivider { ix: previous });
    }
    let removed = blocks.remove(ix);
    let offset = blocks[previous].text.len();
    blocks[previous].text.push_str(&removed.text);
    Some(Merge::Joined { into: previous, offset })
}

/// Delete at the end of the block at `ix`: deletes a divider after it, or
/// appends the next block's text to it.
pub fn merge_next_into(blocks: &mut Vec<Block>, ix: usize) -> Option<Merge> {
    let next = blocks.get(ix + 1)?;
    if next.kind == BlockKind::Divider {
        blocks.remove(ix + 1);
        return Some(Merge::RemovedDivider { ix });
    }
    let removed = blocks.remove(ix + 1);
    let offset = blocks[ix].text.len();
    blocks[ix].text.push_str(&removed.text);
    Some(Merge::Joined { into: ix, offset })
}

/// Moves the block at `from` so it sits before the block currently at
/// `before` (or at the end when `before == blocks.len()`). Returns its new
/// index.
pub fn move_block(blocks: &mut Vec<Block>, from: usize, before: usize) -> usize {
    let block = blocks.remove(from);
    let to = if before > from { before - 1 } else { before };
    blocks.insert(to, block);
    to
}

/// Inserts a copy of the block at `ix` after it, with a fresh id.
pub fn duplicate(blocks: &mut Vec<Block>, ix: usize) -> usize {
    let copy = Block { id: BlockId::new(), ..blocks[ix].clone() };
    blocks.insert(ix + 1, copy);
    ix + 1
}

/// Indents (`delta > 0`) or outdents a list item. An item can sit at most
/// one level deeper than the block above it. Returns whether it changed.
pub fn indent(blocks: &mut [Block], ix: usize, delta: i8) -> bool {
    if !is_list(blocks[ix].kind) {
        return false;
    }
    let limit = match ix.checked_sub(1).map(|previous| &blocks[previous]) {
        Some(previous) if is_list(previous.kind) => previous.indent.saturating_add(1).min(MAX_INDENT),
        _ => 0,
    };
    let current = blocks[ix].indent;
    let target = if delta > 0 { current.saturating_add(1).min(limit).max(current) } else { current.saturating_sub(1) };
    if target == current {
        return false;
    }
    blocks[ix].indent = target;
    true
}

/// Converts the block to `kind`, keeping its text. Leaving a list resets the
/// indent; dividers drop their text.
pub fn convert(block: &mut Block, kind: BlockKind) {
    block.kind = kind;
    if !is_list(kind) {
        block.indent = 0;
    }
    if kind != BlockKind::Todo {
        block.checked = false;
    }
    if !kind.has_text() {
        block.text.clear();
    }
}

/// Number shown before each numbered list item (0 for other blocks).
///
/// Consecutive numbered items at one indent count up; nested items keep
/// their parent's sequence going, while any other block at the same or a
/// shallower level restarts it.
pub fn numbering(blocks: &[Block]) -> Vec<usize> {
    let mut counters = [0usize; MAX_INDENT as usize + 1];
    blocks
        .iter()
        .map(|block| {
            let depth = if is_list(block.kind) { block.indent.min(MAX_INDENT) as usize } else { 0 };
            for counter in &mut counters[depth + 1..] {
                *counter = 0;
            }
            if block.kind == BlockKind::Numbered {
                counters[depth] += 1;
                counters[depth]
            } else {
                counters[depth] = 0;
                0
            }
        })
        .collect()
}

/// Label of a numbered item: `1.` at the top level, `a.` and `i.` below.
pub fn number_label(number: usize, indent: u8) -> String {
    match indent % 3 {
        0 => format!("{number}."),
        1 => format!("{}.", alphabetic(number)),
        _ => format!("{}.", roman(number)),
    }
}

/// Bullet glyph for a bulleted item at `indent`.
pub fn bullet(indent: u8) -> &'static str {
    ["•", "◦", "▪"][indent as usize % 3]
}

fn alphabetic(mut number: usize) -> String {
    let mut letters = Vec::new();
    while number > 0 {
        number -= 1;
        letters.push(char::from(b'a' + (number % 26) as u8));
        number /= 26;
    }
    letters.iter().rev().collect()
}

fn roman(mut number: usize) -> String {
    const NUMERALS: [(usize, &str); 13] = [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ];
    let mut out = String::new();
    for (value, numeral) in NUMERALS {
        while number >= value {
            out.push_str(numeral);
            number -= value;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(kind: BlockKind, text: &str) -> Block {
        Block::new(kind, text)
    }

    fn nested(kind: BlockKind, text: &str, indent: u8) -> Block {
        Block { indent, ..Block::new(kind, text) }
    }

    fn texts(blocks: &[Block]) -> Vec<&str> {
        blocks.iter().map(|b| b.text.as_str()).collect()
    }

    fn kinds_and_texts(blocks: &[Block]) -> Vec<(BlockKind, &str)> {
        blocks.iter().map(|b| (b.kind, b.text.as_str())).collect()
    }

    #[test]
    fn paste_keeps_pasted_blocks_whole_unless_they_are_paragraphs() {
        use BlockKind::*;
        let paste_into = |target: Block, at: Range<usize>, pasted: Vec<Block>| {
            let mut blocks = vec![block(Paragraph, "above"), target, block(Paragraph, "below")];
            let caret = paste(&mut blocks, 1, at, pasted);
            (blocks, caret)
        };

        // A cue after text is its own block; the text after the caret joins the
        // last pasted paragraph.
        let (blocks, caret) =
            paste_into(block(Paragraph, "line Crest"), 6..6, vec![block(BRoll, "Drone"), block(Paragraph, "Next ")]);
        assert_eq!(
            kinds_and_texts(&blocks),
            [
                (Paragraph, "above"),
                (Paragraph, "line C"),
                (BRoll, "Drone"),
                (Paragraph, "Next rest"),
                (Paragraph, "below")
            ]
        );
        assert_eq!(caret, (3, 5));

        // A paragraph joins the text before the caret; a heading at the end
        // leaves the rest of the line in a block of its own kind.
        let (blocks, caret) =
            paste_into(block(BRoll, "Drone shot"), 6..6, vec![block(Paragraph, "over"), block(Heading2, "Section")]);
        assert_eq!(&kinds_and_texts(&blocks)[1..4], [(BRoll, "Drone over"), (Heading2, "Section"), (BRoll, "shot")]);
        assert_eq!(caret, (3, 0));

        // An empty block, or a paragraph pasted into at its start, becomes the
        // first pasted block, keeping its id.
        let target = block(Bulleted, "");
        let id = target.id;
        let (blocks, caret) = paste_into(target, 0..0, vec![block(Callout, "Note"), block(Paragraph, "x")]);
        assert_eq!(&kinds_and_texts(&blocks)[1..3], [(Callout, "Note"), (Paragraph, "x")]);
        assert_eq!((blocks[1].id, caret), (id, (2, 1)));
        let (blocks, _) = paste_into(block(Paragraph, "world"), 0..0, vec![block(OnScreen, "cue")]);
        assert_eq!(&kinds_and_texts(&blocks)[1..3], [(OnScreen, "cue"), (Paragraph, "world")]);

        // At the start of another kind of block, the pasted blocks go above it.
        let target = block(Heading2, "Intro");
        let id = target.id;
        let (blocks, caret) = paste_into(target, 0..0, vec![block(BRoll, "cue"), block(Paragraph, "said")]);
        assert_eq!(&kinds_and_texts(&blocks)[1..4], [(BRoll, "cue"), (Paragraph, "said"), (Heading2, "Intro")]);
        assert_eq!((blocks[3].id, caret), (id, (3, 0)));

        // The selection is replaced, and a trailing divider gets a line after it.
        let (blocks, caret) =
            paste_into(block(Paragraph, "a[sel]"), 1..6, vec![block(Paragraph, "b"), block(Divider, "")]);
        assert_eq!(&kinds_and_texts(&blocks)[1..4], [(Paragraph, "ab"), (Divider, ""), (Paragraph, "")]);
        assert_eq!(caret, (3, 0));
    }

    #[test]
    fn split_moves_the_tail_and_continues_lists() {
        let mut blocks = vec![nested(BlockKind::Bulleted, "onetwo", 1), block(BlockKind::Paragraph, "end")];
        let new = split(&mut blocks, 0, 3);
        assert_eq!(new, 1);
        assert_eq!(texts(&blocks), vec!["one", "two", "end"]);
        assert_eq!((blocks[1].kind, blocks[1].indent), (BlockKind::Bulleted, 1));
        assert_ne!(blocks[0].id, blocks[1].id);

        let mut blocks = vec![block(BlockKind::Heading1, "Title")];
        split(&mut blocks, 0, 5);
        assert_eq!(blocks[1].kind, BlockKind::Paragraph);
        assert_eq!(blocks[1].text, "");

        let mut blocks = vec![Block { checked: true, ..block(BlockKind::Todo, "ab") }];
        split(&mut blocks, 0, 1);
        assert_eq!((blocks[1].kind, blocks[1].checked), (BlockKind::Todo, false));
    }

    #[test]
    fn backspace_merges_or_removes_a_divider() {
        let mut blocks = vec![block(BlockKind::Heading2, "Hello"), block(BlockKind::Paragraph, " world")];
        assert_eq!(merge_into_previous(&mut blocks, 1), Some(Merge::Joined { into: 0, offset: 5 }));
        assert_eq!(texts(&blocks), vec!["Hello world"]);
        assert_eq!(blocks[0].kind, BlockKind::Heading2);

        let mut blocks = vec![block(BlockKind::Divider, ""), block(BlockKind::Paragraph, "x")];
        assert_eq!(merge_into_previous(&mut blocks, 1), Some(Merge::RemovedDivider { ix: 0 }));
        assert_eq!(texts(&blocks), vec!["x"]);

        assert_eq!(merge_into_previous(&mut blocks, 0), None);
    }

    #[test]
    fn delete_pulls_the_next_block_in() {
        let mut blocks = vec![block(BlockKind::Paragraph, "a"), block(BlockKind::Quote, "b")];
        assert_eq!(merge_next_into(&mut blocks, 0), Some(Merge::Joined { into: 0, offset: 1 }));
        assert_eq!(texts(&blocks), vec!["ab"]);
        assert_eq!(merge_next_into(&mut blocks, 0), None);

        let mut blocks = vec![block(BlockKind::Paragraph, "a"), block(BlockKind::Divider, "")];
        assert_eq!(merge_next_into(&mut blocks, 0), Some(Merge::RemovedDivider { ix: 0 }));
        assert_eq!(blocks.len(), 1);
    }

    #[test]
    fn moves_blocks_in_both_directions() {
        let mut blocks: Vec<_> = ["a", "b", "c", "d"].map(|t| block(BlockKind::Paragraph, t)).into();
        assert_eq!(move_block(&mut blocks, 0, 3), 2);
        assert_eq!(texts(&blocks), vec!["b", "c", "a", "d"]);
        assert_eq!(move_block(&mut blocks, 3, 0), 0);
        assert_eq!(texts(&blocks), vec!["d", "b", "c", "a"]);
        assert_eq!(move_block(&mut blocks, 1, 4), 3);
        assert_eq!(texts(&blocks), vec!["d", "c", "a", "b"]);
    }

    #[test]
    fn indents_list_items_one_level_below_their_parent() {
        let mut blocks =
            vec![block(BlockKind::Bulleted, "a"), block(BlockKind::Bulleted, "b"), block(BlockKind::Paragraph, "p")];
        assert!(!indent(&mut blocks, 0, 1), "the first item has no parent");
        assert!(indent(&mut blocks, 1, 1));
        assert!(!indent(&mut blocks, 1, 1), "at most one level deeper than its parent");
        assert_eq!(blocks[1].indent, 1);
        assert!(!indent(&mut blocks, 2, 1), "paragraphs do not indent");
        assert!(indent(&mut blocks, 1, -1));
        assert!(!indent(&mut blocks, 1, -1));

        let mut deep: Vec<_> = (0..6).map(|i| nested(BlockKind::Todo, "t", i.min(3))).collect();
        assert!(!indent(&mut deep, 4, 1), "max indent");
        assert_eq!(deep[4].indent, MAX_INDENT);

        // Stored documents may carry any indent; arithmetic must not overflow.
        let mut corrupt = vec![nested(BlockKind::Bulleted, "a", u8::MAX), nested(BlockKind::Bulleted, "b", 9)];
        assert!(!indent(&mut corrupt, 1, 1));
        assert!(indent(&mut corrupt, 1, -1));
    }

    #[test]
    fn numbers_consecutive_items_per_level() {
        let blocks = vec![
            block(BlockKind::Numbered, "1"),
            block(BlockKind::Numbered, "2"),
            nested(BlockKind::Numbered, "2a", 1),
            nested(BlockKind::Numbered, "2b", 1),
            nested(BlockKind::Bulleted, "bullet", 2),
            block(BlockKind::Numbered, "3"),
            nested(BlockKind::Numbered, "3a", 1),
            block(BlockKind::Paragraph, "break"),
            block(BlockKind::Numbered, "1 again"),
            block(BlockKind::Bulleted, "b"),
            block(BlockKind::Numbered, "1 after bullet"),
        ];
        assert_eq!(numbering(&blocks), vec![1, 2, 1, 2, 0, 3, 1, 0, 1, 0, 1]);
    }

    #[test]
    fn labels_numbers_by_depth() {
        assert_eq!(number_label(3, 0), "3.");
        assert_eq!(number_label(2, 1), "b.");
        assert_eq!(number_label(27, 1), "aa.");
        assert_eq!(number_label(4, 2), "iv.");
        assert_eq!(number_label(14, 2), "xiv.");
        assert_eq!(bullet(0), "•");
        assert_eq!(bullet(1), "◦");
        assert_eq!(bullet(3), "•");
    }

    #[test]
    fn convert_resets_fields_that_no_longer_apply() {
        let mut todo = Block { checked: true, indent: 2, ..block(BlockKind::Todo, "x") };
        convert(&mut todo, BlockKind::Heading1);
        assert_eq!((todo.kind, todo.checked, todo.indent, todo.text.as_str()), (BlockKind::Heading1, false, 0, "x"));
        convert(&mut todo, BlockKind::Divider);
        assert_eq!(todo.text, "");
    }

    #[test]
    fn duplicate_gets_a_new_id() {
        let mut blocks = vec![block(BlockKind::Callout, "tip")];
        assert_eq!(duplicate(&mut blocks, 0), 1);
        assert_eq!(texts(&blocks), vec!["tip", "tip"]);
        assert_ne!(blocks[0].id, blocks[1].id);
    }
}
