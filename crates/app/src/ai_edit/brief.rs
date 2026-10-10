//! The edit brief: what the creator asks the agent to do with the footage.

use std::ops::Range;

use serde::{Deserialize, Serialize};
use videocrm_core::document::teleprompter_text;
use videocrm_core::settings::EditPreferences;
use videocrm_core::{Block, Video, VideoFormat};

/// The state of a video's brief form, kept while its tab is closed and across restarts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct BriefDraft {
    pub brief: String,
    pub preferences: EditPreferences,
    pub auto_approve: bool,
    /// The creator changed the brief by hand; it no longer follows the video.
    pub edited: bool,
    /// What the brief was drafted from; see [`brief_source`].
    pub source: String,
}

/// What a default brief depends on besides the edit preferences (the video's title,
/// hook and format, its script, the standing instructions), as one comparable value:
/// the brief changes when this does.
pub fn brief_source(video: &Video, script: &[Block], extra_instructions: &str) -> String {
    default_brief(video, script, &EditPreferences::default(), extra_instructions)
}

/// video-use grade presets, as stored in [`EditPreferences::color_grade`], with their labels.
pub const COLOR_GRADES: [(&str, &str); 4] =
    [("none", "None"), ("subtle", "Subtle"), ("neutral_punch", "Neutral punch"), ("warm_cinematic", "Warm cinematic")];

/// Longest script excerpt put into a brief, in characters (about ten minutes of speech).
const SCRIPT_CHARS: usize = 10_000;
const PREFERENCES_HEADING: &str = "Edit preferences:";

const NATURAL_LENGTH: &str = "- Keep the natural length; tighten the pacing only.";
const TARGET_LENGTH: &str = "- Target length: ";
const REMOVE_FILLERS: &str = "- Remove filler words (“um”, “uh”), false starts, retakes and dead air.";
const KEEP_FILLERS: &str = "- Keep filler words and natural pauses.";
const SUBTITLES: &str = "- Burn subtitles into the render.";
const NO_SUBTITLES: &str = "- No subtitles.";
const NO_GRADE: &str = "- No colour grade: keep the original look.";
const GRADE: &str = "- Colour grade: ";

/// Label of a grade preset, or the free description itself.
pub fn color_grade_label(grade: &str) -> &str {
    COLOR_GRADES.iter().find(|(value, _)| *value == grade).map_or(grade, |(_, label)| label)
}

/// The brief the edit form starts from: the video's premise, the edit preferences, the
/// spoken script (so the agent can pick the takes that match it) and the creator's
/// standing instructions.
pub fn default_brief(
    video: &Video,
    script: &[Block],
    preferences: &EditPreferences,
    extra_instructions: &str,
) -> String {
    let title = video.title.trim();
    let mut brief = if title.is_empty() {
        format!("Edit this footage into {}.\n", format_phrase(video.format))
    } else {
        format!("Edit “{title}” into {}.\n", format_phrase(video.format))
    };
    if !video.hook.trim().is_empty() {
        brief.push_str(&format!("Hook: {}\n", video.hook.trim()));
    }

    brief.push('\n');
    brief.push_str(PREFERENCES_HEADING);
    brief.push('\n');
    brief.push_str(&preference_lines(preferences));

    let spoken = teleprompter_text(script);
    if !spoken.is_empty() {
        brief
            .push_str("\nScript (what the creator meant to say; use it to choose the best takes and their order):\n\n");
        brief.push_str(&cap(&spoken, SCRIPT_CHARS));
        brief.push('\n');
    }

    let extra = extra_instructions.trim();
    if !extra.is_empty() {
        brief.push_str("\nHouse style:\n");
        brief.push_str(extra);
        brief.push('\n');
    }
    brief
}

/// The edits that rewrite the preference lines of `brief` from `old` to `new`, as byte
/// ranges of `brief` with their replacements, in order. Only the line the app wrote
/// for a preference that changed is replaced; every line the creator wrote, bullets
/// included, stays. A preference whose line the creator removed is added at the end
/// of the "Edit preferences" list, or in a new list at the end of the brief.
pub fn preference_edits(brief: &str, old: &EditPreferences, new: &EditPreferences) -> Vec<(Range<usize>, String)> {
    let lines = line_spans(brief);
    let heading = lines.iter().position(|span| brief[span.clone()].trim() == PREFERENCES_HEADING);
    // The list: the lines after the heading, up to the first blank one.
    let list = heading.map(|heading| {
        let end = lines[heading + 1..]
            .iter()
            .position(|span| brief[span.clone()].trim().is_empty())
            .map_or(lines.len(), |offset| heading + 1 + offset);
        heading + 1..end
    });
    let searched = heading.map_or(0, |heading| heading + 1);

    let (old_lines, new_lines) = (preference_line_array(old), preference_line_array(new));
    let mut used = Vec::new();
    let mut edits = Vec::new();
    let mut missing = Vec::new();
    for (slot, (old_line, new_line)) in old_lines.iter().zip(&new_lines).enumerate() {
        if old_line == new_line {
            continue;
        }
        let free = |ix: &usize| !used.contains(ix);
        // The line as the app wrote it; else one the app writes for this preference.
        let found = (searched..lines.len())
            .filter(free)
            .find(|ix| brief[lines[*ix].clone()].trim() == old_line)
            .or_else(|| list.clone()?.filter(free).find(|ix| is_slot_line(slot, &brief[lines[*ix].clone()])));
        match found {
            Some(ix) => {
                used.push(ix);
                let span = &lines[ix];
                let text = &brief[span.clone()];
                let start = span.start + (text.len() - text.trim_start().len());
                if text.trim() != new_line {
                    edits.push((start..span.start + text.trim_end().len(), new_line.clone()));
                }
            }
            None => missing.push(new_line.clone()),
        }
    }
    if !missing.is_empty() {
        let added: String = missing.iter().map(|line| format!("\n{line}")).collect();
        match list {
            Some(list) => {
                // After the list's last line, or the heading when the list is empty.
                let end = lines[list.end.max(list.start + 1) - 1].end;
                edits.push((end..end, added));
            }
            None => {
                let separator = match brief {
                    "" => "",
                    brief if brief.ends_with("\n\n") => "",
                    brief if brief.ends_with('\n') => "\n",
                    _ => "\n\n",
                };
                let end = brief.len();
                edits.push((end..end, format!("{separator}{PREFERENCES_HEADING}{added}\n")));
            }
        }
    }
    edits.sort_by_key(|(range, _)| range.start);
    edits
}

/// The preferences the brief's own preference lines state, starting from `current`:
/// when the creator edits or undoes one of those lines, the form follows the brief. A
/// preference without such a line keeps its value.
pub fn preferences_in(brief: &str, current: &EditPreferences) -> EditPreferences {
    let mut preferences = current.clone();
    let lines = line_spans(brief);
    let Some(heading) = lines.iter().position(|span| brief[span.clone()].trim() == PREFERENCES_HEADING) else {
        return preferences;
    };
    let list = lines[heading + 1..].iter().map(|span| brief[span.clone()].trim()).take_while(|line| !line.is_empty());
    let valued = |line: &str, prefix: &str| line.strip_prefix(prefix)?.strip_suffix('.').map(str::to_owned);
    for line in list {
        match line {
            NATURAL_LENGTH => preferences.target_length = String::new(),
            REMOVE_FILLERS => preferences.remove_fillers = true,
            KEEP_FILLERS => preferences.remove_fillers = false,
            SUBTITLES => preferences.subtitles = true,
            NO_SUBTITLES => preferences.subtitles = false,
            NO_GRADE => preferences.color_grade = "none".to_owned(),
            line => {
                if let Some(target) = valued(line, TARGET_LENGTH) {
                    preferences.target_length = target;
                } else if let Some(grade) = valued(line, GRADE) {
                    preferences.color_grade = COLOR_GRADES
                        .iter()
                        .find(|(_, label)| label.to_lowercase() == grade)
                        .map_or(grade, |(value, _)| (*value).to_owned());
                }
            }
        }
    }
    // The same preference either way: the form keeps how it was set.
    if preferences.color_grade == "none" && matches!(current.color_grade.trim(), "" | "none") {
        preferences.color_grade = current.color_grade.clone();
    }
    preferences
}

/// `brief` with [`preference_edits`] applied.
pub fn with_preferences(brief: &str, old: &EditPreferences, new: &EditPreferences) -> String {
    apply_edits(brief, &preference_edits(brief, old, new))
}

/// `text` with `edits` (in order, not overlapping) applied.
pub(crate) fn apply_edits(text: &str, edits: &[(Range<usize>, String)]) -> String {
    let mut result = String::with_capacity(text.len());
    let mut copied = 0;
    for (range, replacement) in edits {
        result.push_str(&text[copied..range.start]);
        result.push_str(replacement);
        copied = range.end;
    }
    result.push_str(&text[copied..]);
    result
}

/// Where `offset` of a text ends up after `edits`: shifted by the edits before it,
/// at the end of the replacement when an edit replaced it.
pub(crate) fn shift_offset(offset: usize, edits: &[(Range<usize>, String)]) -> usize {
    let mut shifted = offset;
    for (range, replacement) in edits {
        if offset >= range.end {
            shifted = shifted + replacement.len() - range.len();
        } else if offset > range.start {
            shifted = shifted - (offset - range.start) + replacement.len();
        }
    }
    shifted
}

/// Byte ranges of the lines of `text`, without their line breaks.
fn line_spans(text: &str) -> Vec<Range<usize>> {
    let mut spans = Vec::new();
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        let content = line.strip_suffix('\n').unwrap_or(line);
        let content = content.strip_suffix('\r').unwrap_or(content);
        spans.push(start..start + content.len());
        start += line.len();
    }
    spans
}

/// Whether `line` is one the app writes for the preference at `slot` of
/// [`preference_line_array`], whatever its value.
fn is_slot_line(slot: usize, line: &str) -> bool {
    let line = line.trim();
    let valued = |prefix: &str| line.starts_with(prefix) && line.ends_with('.');
    match slot {
        0 => line == NATURAL_LENGTH || valued(TARGET_LENGTH),
        1 => line == REMOVE_FILLERS || line == KEEP_FILLERS,
        2 => line == SUBTITLES || line == NO_SUBTITLES,
        _ => line == NO_GRADE || valued(GRADE),
    }
}

fn format_phrase(format: VideoFormat) -> &'static str {
    match format {
        VideoFormat::LongForm => "a long-form YouTube video",
        VideoFormat::Short => "a YouTube Short (vertical, under 60 seconds)",
        VideoFormat::Livestream => "a cut-down of a livestream recording",
    }
}

fn preference_lines(preferences: &EditPreferences) -> String {
    preference_line_array(preferences).iter().map(|line| format!("{line}\n")).collect()
}

/// The preference lines: length, fillers, subtitles, grade.
fn preference_line_array(preferences: &EditPreferences) -> [String; 4] {
    let target = preferences.target_length.trim();
    let grade = preferences.color_grade.trim();
    [
        if target.is_empty() { NATURAL_LENGTH.to_owned() } else { format!("{TARGET_LENGTH}{target}.") },
        if preferences.remove_fillers { REMOVE_FILLERS } else { KEEP_FILLERS }.to_owned(),
        if preferences.subtitles { SUBTITLES } else { NO_SUBTITLES }.to_owned(),
        match grade {
            "" | "none" => NO_GRADE.to_owned(),
            grade => format!("{GRADE}{}.", color_grade_label(grade).to_lowercase()),
        },
    ]
}

/// `text` cut to at most `max` characters at a paragraph or word boundary.
fn cap(text: &str, max: usize) -> String {
    let Some((cut, _)) = text.char_indices().nth(max) else {
        return text.to_owned();
    };
    let head = &text[..cut];
    let boundary = head.rfind("\n\n").or_else(|| head.rfind(char::is_whitespace)).unwrap_or(cut);
    format!("{}\n\n[The script continues; the rest is omitted here.]", head[..boundary].trim_end())
}

#[cfg(test)]
mod tests {
    use videocrm_core::BlockKind;

    use super::*;

    fn video() -> Video {
        let mut video = Video::new("Desk setup 2026");
        video.hook = "Everything on my desk, and what I regret buying.".into();
        video
    }

    fn preferences() -> EditPreferences {
        EditPreferences {
            remove_fillers: true,
            subtitles: true,
            color_grade: "neutral_punch".into(),
            target_length: "8-10 minutes".into(),
        }
    }

    #[test]
    fn brief_carries_premise_preferences_script_and_house_style() {
        let script = vec![
            Block::new(BlockKind::Heading1, "Intro"),
            Block::paragraph("Welcome back to the **channel**."),
            Block::new(BlockKind::BRoll, "Desk overview shot"),
            Block::paragraph("Today: my desk."),
        ];
        let brief = default_brief(&video(), &script, &preferences(), "  Keep my intro jingle.  ");
        assert_eq!(
            brief,
            "Edit “Desk setup 2026” into a long-form YouTube video.\n\
             Hook: Everything on my desk, and what I regret buying.\n\
             \n\
             Edit preferences:\n\
             - Target length: 8-10 minutes.\n\
             - Remove filler words (“um”, “uh”), false starts, retakes and dead air.\n\
             - Burn subtitles into the render.\n\
             - Colour grade: neutral punch.\n\
             \n\
             Script (what the creator meant to say; use it to choose the best takes and their order):\n\
             \n\
             Welcome back to the channel.\n\
             \n\
             Today: my desk.\n\
             \n\
             House style:\n\
             Keep my intro jingle.\n"
        );
    }

    #[test]
    fn brief_without_hook_script_or_house_style_stays_short() {
        let mut video = Video::new("");
        video.format = VideoFormat::Short;
        let brief = default_brief(&video, &[], &EditPreferences::default(), "");
        assert_eq!(
            brief,
            "Edit this footage into a YouTube Short (vertical, under 60 seconds).\n\
             \n\
             Edit preferences:\n\
             - Keep the natural length; tighten the pacing only.\n\
             - Remove filler words (“um”, “uh”), false starts, retakes and dead air.\n\
             - No subtitles.\n\
             - No colour grade: keep the original look.\n"
        );
    }

    #[test]
    fn long_scripts_are_capped_at_a_paragraph() {
        let paragraph = "word ".repeat(400);
        let script: Vec<Block> = (0..20).map(|_| Block::paragraph(paragraph.trim())).collect();
        let brief = default_brief(&video(), &script, &EditPreferences::default(), "");
        assert!(brief.chars().count() < SCRIPT_CHARS + 1_000, "{}", brief.len());
        assert!(brief.contains("word\n\n[The script continues; the rest is omitted here.]"));
    }

    #[test]
    fn free_text_grades_are_kept_verbatim() {
        let preferences = EditPreferences { color_grade: "teal and orange".into(), ..EditPreferences::default() };
        let brief = default_brief(&video(), &[], &preferences, "");
        assert!(brief.contains("- Colour grade: teal and orange.\n"), "{brief}");
        assert_eq!(color_grade_label("warm_cinematic"), "Warm cinematic");
    }

    #[test]
    fn preferences_are_rewritten_in_place() {
        let script = [Block::paragraph("Hello.")];
        let original = default_brief(&video(), &script, &EditPreferences::default(), "");
        let edited = original.replace("Hook:", "Hook (keep it):");
        let rewritten = with_preferences(&edited, &EditPreferences::default(), &preferences());
        assert_eq!(rewritten, default_brief(&video(), &script, &preferences(), "").replace("Hook:", "Hook (keep it):"));
    }

    #[test]
    fn the_creators_own_lines_stay_when_a_preference_changes() {
        let before = EditPreferences::default();
        let brief = default_brief(&video(), &[], &before, "").replace(
            "- No colour grade: keep the original look.\n",
            "- No colour grade: keep the original look.\n- Mention the sponsor at the end.\n",
        );
        let after = EditPreferences { remove_fillers: false, subtitles: true, ..before.clone() };
        let rewritten = with_preferences(&brief, &before, &after);
        assert!(rewritten.contains("- Mention the sponsor at the end.\n"), "{rewritten}");
        assert!(rewritten.contains("- Keep filler words and natural pauses.\n- Burn subtitles into the render.\n"));
        assert_eq!(rewritten.lines().count(), brief.lines().count(), "one line for one line");

        // Only the changed line is touched, by an edit of that line.
        let edits = preference_edits(&brief, &before, &EditPreferences { subtitles: true, ..before.clone() });
        assert_eq!(edits.len(), 1);
        assert_eq!(&brief[edits[0].0.clone()], "- No subtitles.");
    }

    #[test]
    fn a_preference_line_the_creator_changed_is_still_found_or_added() {
        let before = preferences();
        let brief = "Cut it.\n\nEdit preferences:\n- Target length: about 8 minutes, strictly.\n- Mention the sponsor.\n\nMore.\n";
        // A line the app writes for the length, though not as it wrote it.
        let longer = EditPreferences { target_length: "10 minutes".into(), ..before.clone() };
        assert_eq!(
            with_preferences(brief, &before, &longer),
            "Cut it.\n\nEdit preferences:\n- Target length: 10 minutes.\n- Mention the sponsor.\n\nMore.\n"
        );
        // The subtitles line was deleted: the new one ends the list.
        let without = EditPreferences { subtitles: false, ..before.clone() };
        assert_eq!(
            with_preferences(brief, &before, &without),
            "Cut it.\n\nEdit preferences:\n- Target length: about 8 minutes, strictly.\n- Mention the sponsor.\n\
             - No subtitles.\n\nMore.\n"
        );
        // No list at all: one starts at the end.
        assert_eq!(
            with_preferences("Just cut it.", &before, &without),
            "Just cut it.\n\nEdit preferences:\n- No subtitles.\n"
        );
        // Undone by hand to the new value already: nothing to do.
        let undone = default_brief(&video(), &[], &without, "");
        assert!(preference_edits(&undone, &before, &without).is_empty());
    }

    #[test]
    fn the_form_follows_the_preference_lines_of_the_brief() {
        let current = EditPreferences::default();
        let brief = default_brief(&video(), &[], &preferences(), "");
        assert_eq!(preferences_in(&brief, &current), preferences());
        // The creator's own lines change nothing; a preference without a line keeps its value.
        let custom = "Edit preferences:\n- Colour grade: teal and orange.\n- Mention the sponsor.\n\n- No subtitles.\n";
        let followed = preferences_in(custom, &preferences());
        assert_eq!(followed, EditPreferences { color_grade: "teal and orange".into(), ..preferences() });
        assert_eq!(preferences_in("Just cut it.", &preferences()), preferences());
        let unchanged = default_brief(&video(), &[], &current, "");
        assert_eq!(preferences_in(&unchanged, &current), current, "no grade is still no grade");
    }

    #[test]
    fn offsets_follow_the_edits() {
        let edits = vec![(2..4, "abcd".to_owned()), (10..10, "xy".to_owned())];
        assert_eq!(apply_edits("0123456789!", &edits), "01abcd456789xy!");
        assert_eq!(shift_offset(1, &edits), 1);
        assert_eq!(shift_offset(3, &edits), 6, "inside a replaced range: its end");
        assert_eq!(shift_offset(5, &edits), 7);
        assert_eq!(shift_offset(11, &edits), 15);
    }
}
