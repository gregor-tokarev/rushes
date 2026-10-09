//! The edit brief: what the creator asks the agent to do with the footage.

use videocrm_core::document::teleprompter_text;
use videocrm_core::settings::EditPreferences;
use videocrm_core::{Block, Video, VideoFormat};

/// The state of a video's brief form.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BriefDraft {
    pub brief: String,
    pub preferences: EditPreferences,
    pub auto_approve: bool,
}

/// video-use grade presets, as stored in [`EditPreferences::color_grade`], with their labels.
pub const COLOR_GRADES: [(&str, &str); 4] =
    [("none", "None"), ("subtle", "Subtle"), ("neutral_punch", "Neutral punch"), ("warm_cinematic", "Warm cinematic")];

/// Longest script excerpt put into a brief, in characters (about ten minutes of speech).
const SCRIPT_CHARS: usize = 10_000;
const PREFERENCES_HEADING: &str = "Edit preferences:";

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
        format!("Edit \"{title}\" into {}.\n", format_phrase(video.format))
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

/// `brief` with its "Edit preferences" list rewritten for `preferences`, keeping every
/// other edit the creator made. `None` when the brief no longer has that list.
pub fn with_preferences(brief: &str, preferences: &EditPreferences) -> Option<String> {
    let lines: Vec<&str> = brief.lines().collect();
    let heading = lines.iter().position(|line| line.trim() == PREFERENCES_HEADING)?;
    let end = lines[heading + 1..]
        .iter()
        .position(|line| !line.trim_start().starts_with("- "))
        .map_or(lines.len(), |offset| heading + 1 + offset);

    let mut rewritten = lines[..=heading].join("\n");
    rewritten.push('\n');
    rewritten.push_str(&preference_lines(preferences));
    if end < lines.len() {
        rewritten.push_str(&lines[end..].join("\n"));
        if brief.ends_with('\n') {
            rewritten.push('\n');
        }
    }
    Some(rewritten)
}

fn format_phrase(format: VideoFormat) -> &'static str {
    match format {
        VideoFormat::LongForm => "a long-form YouTube video",
        VideoFormat::Short => "a YouTube Short (vertical, under 60 seconds)",
        VideoFormat::Livestream => "a cut-down of a livestream recording",
    }
}

fn preference_lines(preferences: &EditPreferences) -> String {
    let target = preferences.target_length.trim();
    let grade = preferences.color_grade.trim();
    [
        if target.is_empty() {
            "- Keep the natural length; tighten the pacing only.".to_owned()
        } else {
            format!("- Target length: {target}.")
        },
        if preferences.remove_fillers {
            "- Remove filler words (\"um\", \"uh\"), false starts, retakes and dead air.".to_owned()
        } else {
            "- Keep filler words and natural pauses.".to_owned()
        },
        if preferences.subtitles { "- Burn subtitles into the render." } else { "- No subtitles." }.to_owned(),
        match grade {
            "" | "none" => "- No colour grade: keep the original look.".to_owned(),
            grade => format!("- Colour grade: {}.", color_grade_label(grade).to_lowercase()),
        },
    ]
    .iter()
    .map(|line| format!("{line}\n"))
    .collect()
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
            "Edit \"Desk setup 2026\" into a long-form YouTube video.\n\
             Hook: Everything on my desk, and what I regret buying.\n\
             \n\
             Edit preferences:\n\
             - Target length: 8-10 minutes.\n\
             - Remove filler words (\"um\", \"uh\"), false starts, retakes and dead air.\n\
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
             - Remove filler words (\"um\", \"uh\"), false starts, retakes and dead air.\n\
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
        let original = default_brief(&video(), &[Block::paragraph("Hello.")], &EditPreferences::default(), "");
        let edited = original.replace("Hook:", "Hook (keep it):");
        let rewritten = with_preferences(&edited, &preferences()).expect("preferences list");
        assert_eq!(
            rewritten,
            default_brief(&video(), &[Block::paragraph("Hello.")], &preferences(), "")
                .replace("Hook:", "Hook (keep it):")
        );

        // A list at the very end of the brief is replaced too.
        let tail = "Cut it.\n\nEdit preferences:\n- No subtitles.";
        assert_eq!(
            with_preferences(tail, &EditPreferences::default()).as_deref(),
            Some(
                "Cut it.\n\nEdit preferences:\n\
                 - Keep the natural length; tighten the pacing only.\n\
                 - Remove filler words (\"um\", \"uh\"), false starts, retakes and dead air.\n\
                 - No subtitles.\n\
                 - No colour grade: keep the original look.\n"
            )
        );
        assert_eq!(with_preferences("Just cut it.", &preferences()), None);
    }
}
