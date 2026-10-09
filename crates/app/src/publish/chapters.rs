//! YouTube chapters estimated from the script: one per Heading 2, timed by the
//! spoken words before it. The estimate is explained next to the button that
//! inserts them; the description itself gets a plain list viewers can use.

use videocrm_core::document::{runtime_secs_for_words, spoken_word_count, strip_inline_markdown};
use videocrm_core::{Block, BlockKind};

/// First line of the chapter list. Also how a later insert finds the list to replace.
pub const CHAPTERS_HEADING: &str = "Chapters:";
/// YouTube ignores the whole list when a chapter is shorter than this.
const MIN_CHAPTER_SECS: u32 = 10;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chapter {
    pub start_secs: u32,
    pub title: String,
}

/// Chapters for the script `blocks` at `wpm` words per minute. YouTube requires the
/// first chapter at 0:00, so spoken words before the first Heading 2 become an "Intro",
/// and chapters of at least ten seconds, so a shorter section gives its slot to the
/// heading after it. Empty when the script has no Heading 2.
pub fn chapters(blocks: &[Block], wpm: u32) -> Vec<Chapter> {
    let mut chapters: Vec<Chapter> = Vec::new();
    let mut words = 0;
    for block in blocks {
        if block.kind == BlockKind::Heading2 {
            let title = strip_inline_markdown(&block.text).trim().to_owned();
            if title.is_empty() {
                continue;
            }
            let start_secs = runtime_secs_for_words(words, wpm);
            if chapters.is_empty() && start_secs > 0 {
                chapters.push(Chapter { start_secs: 0, title: "Intro".to_owned() });
            }
            match chapters.last_mut() {
                Some(last) if start_secs < last.start_secs + MIN_CHAPTER_SECS => last.title = title,
                _ => chapters.push(Chapter { start_secs, title }),
            }
        } else {
            words += spoken_word_count(std::slice::from_ref(block));
        }
    }
    chapters
}

/// `0:00 Intro` lines under [`CHAPTERS_HEADING`].
pub fn chapters_text(chapters: &[Chapter]) -> String {
    let mut text = CHAPTERS_HEADING.to_owned();
    for chapter in chapters {
        text.push('\n');
        text.push_str(&timestamp(chapter.start_secs));
        text.push(' ');
        text.push_str(&chapter.title);
    }
    text
}

/// Puts `chapters` into `description`, replacing a chapter list inserted earlier
/// or appending it after a blank line.
pub fn insert_chapters(description: &str, chapters: &str) -> String {
    if let Some(start) = description.find(CHAPTERS_HEADING) {
        // The list runs to the first blank line after its heading.
        let end = description[start..].find("\n\n").map_or(description.len(), |offset| start + offset);
        return format!("{}{chapters}{}", &description[..start], &description[end..]);
    }
    let body = description.trim_end();
    if body.is_empty() { chapters.to_owned() } else { format!("{body}\n\n{chapters}") }
}

/// `m:ss`, or `h:mm:ss` from one hour on, as YouTube writes chapter times.
pub fn timestamp(secs: u32) -> String {
    let (hours, minutes, seconds) = (secs / 3600, secs % 3600 / 60, secs % 60);
    if hours > 0 { format!("{hours}:{minutes:02}:{seconds:02}") } else { format!("{minutes}:{seconds:02}") }
}

#[cfg(test)]
mod tests {
    use videocrm_core::document::DEFAULT_WPM;

    use super::*;

    fn words(count: usize) -> Block {
        Block::paragraph(vec!["word"; count].join(" "))
    }

    fn heading(text: &str) -> Block {
        Block::new(BlockKind::Heading2, text)
    }

    #[test]
    fn times_each_heading_by_the_words_spoken_before_it() {
        let blocks = vec![
            heading("Why **Rust**"),
            words(150),
            Block::new(BlockKind::BRoll, "Crab walking on a beach, lots of unspoken words here"),
            heading("Ownership"),
            words(300),
            Block::new(BlockKind::Heading3, "Not a chapter"),
            words(75),
            heading("Wrap-up"),
        ];
        assert_eq!(
            chapters(&blocks, DEFAULT_WPM),
            vec![
                Chapter { start_secs: 0, title: "Why Rust".into() },
                Chapter { start_secs: 60, title: "Ownership".into() },
                Chapter { start_secs: 210, title: "Wrap-up".into() },
            ]
        );
    }

    #[test]
    fn spoken_intro_before_the_first_heading_starts_at_zero() {
        let blocks = vec![words(50), heading("Setup"), words(30), heading(""), heading("Demo")];
        assert_eq!(
            chapters(&blocks, DEFAULT_WPM),
            vec![
                Chapter { start_secs: 0, title: "Intro".into() },
                Chapter { start_secs: 20, title: "Setup".into() },
                Chapter { start_secs: 32, title: "Demo".into() },
            ]
        );
    }

    #[test]
    fn sections_shorter_than_ten_seconds_merge_into_the_next() {
        // 20 words are 8 seconds at 150 wpm.
        let blocks = vec![heading("Cold open"), words(20), heading("Hook"), words(25), heading("Main")];
        assert_eq!(
            chapters(&blocks, DEFAULT_WPM),
            vec![Chapter { start_secs: 0, title: "Hook".into() }, Chapter { start_secs: 18, title: "Main".into() }]
        );
    }

    #[test]
    fn empty_sections_do_not_produce_zero_length_chapters() {
        let blocks = vec![heading("Draft"), heading("Hook"), words(30), heading("Main")];
        let chapters = chapters(&blocks, DEFAULT_WPM);
        assert_eq!(chapters[0], Chapter { start_secs: 0, title: "Hook".into() });
        assert_eq!(chapters.len(), 2);
        assert!(super::chapters(&[words(100)], DEFAULT_WPM).is_empty());
    }

    #[test]
    fn formats_and_inserts_the_list() {
        let list = chapters_text(&[
            Chapter { start_secs: 0, title: "Intro".into() },
            Chapter { start_secs: 3725, title: "Q&A".into() },
        ]);
        assert_eq!(list, "Chapters:\n0:00 Intro\n1:02:05 Q&A");

        assert_eq!(insert_chapters("", &list), list);
        let described = insert_chapters("My video.\n\n", &list);
        assert_eq!(described, format!("My video.\n\n{list}"));

        // Inserting again replaces the earlier list and keeps what follows it.
        let edited = format!("{described}\n\nFollow me elsewhere.");
        let newer = chapters_text(&[Chapter { start_secs: 0, title: "Start".into() }]);
        assert_eq!(insert_chapters(&edited, &newer), format!("My video.\n\n{newer}\n\nFollow me elsewhere."));
        assert_eq!(timestamp(59), "0:59");
        assert_eq!(timestamp(600), "10:00");
    }
}
