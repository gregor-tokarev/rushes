//! The block catalog behind the slash menu and the block menu's "Turn into".

use gpui_kit::assets::IconName;
use rushes_core::BlockKind;

/// One block kind the user can insert or convert to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockType {
    pub kind: BlockKind,
    pub icon: IconName,
    pub description: &'static str,
    /// Extra search terms for the slash filter.
    aliases: &'static [&'static str],
}

impl BlockType {
    pub fn label(&self) -> &'static str {
        self.kind.label()
    }

    /// Whether the normalized query starts the label, an alias, or one of
    /// their words: `/head` finds headings, `/text` finds "On-screen text".
    fn matches(&self, query: &str) -> bool {
        std::iter::once(self.label()).chain(self.aliases.iter().copied()).any(|term| {
            normalize(term).starts_with(query)
                || term.split(|c: char| !c.is_alphanumeric()).any(|word| normalize(word).starts_with(query))
        })
    }
}

pub const BLOCK_TYPES: [BlockType; 13] = [
    BlockType {
        kind: BlockKind::Paragraph,
        icon: IconName::Pilcrow,
        description: "Plain text",
        aliases: &["paragraph", "plain"],
    },
    BlockType {
        kind: BlockKind::Heading1,
        icon: IconName::Heading1,
        description: "Large section heading",
        aliases: &["h1", "title"],
    },
    BlockType {
        kind: BlockKind::Heading2,
        icon: IconName::Heading2,
        description: "Medium section heading",
        aliases: &["h2", "subtitle"],
    },
    BlockType {
        kind: BlockKind::Heading3,
        icon: IconName::Heading3,
        description: "Small section heading",
        aliases: &["h3"],
    },
    BlockType {
        kind: BlockKind::Bulleted,
        icon: IconName::List,
        description: "Simple bulleted list",
        aliases: &["ul", "bullet", "unordered"],
    },
    BlockType {
        kind: BlockKind::Numbered,
        icon: IconName::ListOrdered,
        description: "List with numbering",
        aliases: &["ol", "ordered"],
    },
    BlockType {
        kind: BlockKind::Todo,
        icon: IconName::ListTodo,
        description: "Track tasks with a checkbox",
        aliases: &["todo", "task", "checkbox"],
    },
    BlockType {
        kind: BlockKind::Quote,
        icon: IconName::TextQuote,
        description: "Capture a quote",
        aliases: &["blockquote", "citation"],
    },
    BlockType {
        kind: BlockKind::Callout,
        icon: IconName::Lightbulb,
        description: "Make a note stand out",
        aliases: &["note", "tip", "info"],
    },
    BlockType {
        kind: BlockKind::Code,
        icon: IconName::Code,
        description: "Code snippet",
        aliases: &["snippet", "pre"],
    },
    BlockType {
        kind: BlockKind::Divider,
        icon: IconName::SeparatorHorizontal,
        description: "Separate sections",
        aliases: &["hr", "line", "separator", "rule"],
    },
    BlockType {
        kind: BlockKind::BRoll,
        icon: IconName::Clapperboard,
        description: "Footage to cut away to",
        aliases: &["broll", "footage", "cutaway", "shot"],
    },
    BlockType {
        kind: BlockKind::OnScreen,
        icon: IconName::Type,
        description: "Text overlay shown in the video",
        aliases: &["onscreen", "overlay", "lower third", "caption"],
    },
];

/// Block types matching the text typed after `/`. Matches on a label prefix
/// come first; otherwise catalog order is kept.
pub fn filter(query: &str) -> Vec<&'static BlockType> {
    let query = normalize(query);
    let (mut prefix, rest): (Vec<_>, Vec<_>) = BLOCK_TYPES
        .iter()
        .filter(|ty| ty.matches(&query))
        .partition(|ty| normalize(ty.label()).starts_with(&query));
    prefix.extend(rest);
    prefix
}

/// Offset of a `/` the last edit inserted right before the caret at the
/// start of a word, which opens the slash menu.
pub fn typed_slash(previous: &str, text: &str, cursor: usize) -> Option<usize> {
    let slash = cursor.checked_sub(1)?;
    if text.as_bytes().get(slash) != Some(&b'/')
        || !text[..slash].chars().next_back().is_none_or(char::is_whitespace)
    {
        return None;
    }
    let prefix: usize =
        previous.chars().zip(text.chars()).take_while(|(a, b)| a == b).map(|(c, _)| c.len_utf8()).sum();
    let suffix: usize = previous[prefix..]
        .chars()
        .rev()
        .zip(text[prefix..].chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(c, _)| c.len_utf8())
        .sum();
    (prefix <= slash && slash < text.len() - suffix).then_some(slash)
}

/// Lowercase alphanumerics only, so `/b-roll`, `/broll` and `/B roll` agree.
fn normalize(text: &str) -> String {
    text.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(query: &str) -> Vec<BlockKind> {
        filter(query).iter().map(|ty| ty.kind).collect()
    }

    #[test]
    fn empty_query_lists_every_kind_in_catalog_order() {
        assert_eq!(kinds(""), BlockKind::ALL.to_vec());
    }

    #[test]
    fn filters_by_label_and_alias() {
        assert_eq!(kinds("head"), vec![BlockKind::Heading1, BlockKind::Heading2, BlockKind::Heading3]);
        assert_eq!(kinds("broll"), vec![BlockKind::BRoll]);
        assert_eq!(kinds("B-Roll"), vec![BlockKind::BRoll]);
        assert_eq!(kinds("h2"), vec![BlockKind::Heading2]);
        assert_eq!(kinds("overlay"), vec![BlockKind::OnScreen]);
        assert!(kinds("zzz").is_empty());
    }

    #[test]
    fn detects_a_slash_typed_at_a_word_start() {
        assert_eq!(typed_slash("", "/", 1), Some(0));
        assert_eq!(typed_slash("Intro", "Intro /", 7), Some(6));
        assert_eq!(typed_slash("a b", "a /b", 3), Some(2));
        assert_eq!(typed_slash("and", "and/", 4), None, "inside a word");
        assert_eq!(typed_slash("a /x", "a /", 3), None, "deleting after an old slash");
        assert_eq!(typed_slash("/", "/x", 2), None);
    }

    #[test]
    fn single_letters_match_word_starts_only() {
        assert_eq!(kinds("b"), vec![BlockKind::Bulleted, BlockKind::BRoll, BlockKind::Quote]);
    }

    #[test]
    fn label_prefix_matches_rank_first() {
        let found = kinds("text");
        assert_eq!(found[0], BlockKind::Paragraph);
        assert!(found.contains(&BlockKind::OnScreen));
    }

    #[test]
    fn every_kind_has_one_block_type() {
        for kind in BlockKind::ALL {
            assert_eq!(BLOCK_TYPES.iter().filter(|ty| ty.kind == kind).count(), 1);
        }
    }
}
