//! Writing a video's YouTube details (title, description, tags) with the agent,
//! from its script and the transcript video-use made of the footage.
//!
//! One headless agent turn without tools: everything the agent needs is in the
//! prompt, which goes on stdin since a transcript can outgrow an argument, and the
//! answer is JSON checked against [`DETAILS_SCHEMA`].

use std::fs;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use serde::Deserialize;

use crate::agent::{AgentConfig, Invocation, run_agent};
use crate::error::PipelineError;
use crate::event::AgentEvent;

/// Where video-use keeps its working state, inside the footage folder.
const EDIT_DIR: &str = "edit";
/// video-use's phrase-level transcript of every take.
const PACKED_TRANSCRIPT: &str = "takes_packed.md";
/// video-use's raw transcripts, one JSON file per take.
const TRANSCRIPTS_DIR: &str = "transcripts";
/// video-use's record of the edit decisions.
const PROJECT_NOTES: &str = "project.md";

/// Longest transcript sent, in characters: well inside the agent's context.
const TRANSCRIPT_MAX_CHARS: usize = 150_000;
/// Longest script sent, in characters.
const SCRIPT_MAX_CHARS: usize = 60_000;
/// Longest stretch of the edit notes sent, in characters: the latest sessions.
const NOTES_MAX_CHARS: usize = 12_000;

/// The answer's shape. YouTube's own limits are in the prompt; the app checks them
/// again, since a schema can't count bytes or the length of all tags together.
pub const DETAILS_SCHEMA: &str = r#"{"type":"object","properties":{"title":{"type":"string","maxLength":100},"description":{"type":"string"},"tags":{"type":"array","items":{"type":"string"}}},"required":["title","description","tags"],"additionalProperties":false}"#;

/// What the agent writes from.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DetailsRequest {
    /// The video's working title in Rushes.
    pub video_title: String,
    /// A YouTube Short rather than a long-form video.
    pub short: bool,
    /// The script as Markdown; may be empty.
    pub script: String,
    /// See [`read_transcript`].
    pub transcript: Option<String>,
    /// See [`read_edit_notes`].
    pub edit_notes: Option<String>,
    /// What the form holds now: links and boilerplate in the description stay.
    pub current: VideoDetails,
    /// The creator's own instructions for this request; may be empty.
    pub instructions: String,
}

/// A video's YouTube title, description and tags.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct VideoDetails {
    pub title: String,
    pub description: String,
    pub tags: Vec<String>,
}

/// What the agent wrote.
#[derive(Clone, Debug, PartialEq)]
pub struct WrittenDetails {
    pub details: VideoDetails,
    pub cost_usd: Option<f64>,
}

/// Writes the details for `request`, with `cwd` as the agent's working directory.
///
/// Blocks until the agent answers. Setting `cancel` stops it and returns
/// [`PipelineError::Cancelled`].
pub fn write_details(
    config: &AgentConfig,
    cwd: &Path,
    request: &DetailsRequest,
    on_event: &mut dyn FnMut(&AgentEvent),
    cancel: &AtomicBool,
) -> Result<WrittenDetails, PipelineError> {
    let program = config.resolve_command().ok_or_else(|| PipelineError::AgentNotFound(config.command.clone()))?;
    // No tools, so nothing to allow; the creator's own model and environment stay.
    let config = AgentConfig { allowed_tools: Vec::new(), max_turns: None, ..config.clone() };
    let invocation = Invocation {
        cwd: cwd.to_owned(),
        prompt: details_prompt(request),
        prompt_on_stdin: true,
        tools: Some(Vec::new()),
        json_schema: Some(DETAILS_SCHEMA.to_owned()),
        one_off: true,
        ..Invocation::default()
    };
    let (status, run) = run_agent(&config, &program, &invocation, on_event, cancel)?;
    let failed = |message: String, no_result: bool| PipelineError::AgentFailed {
        message,
        stderr_tail: run.stderr_tail.clone(),
        session_id: None,
        no_result,
    };
    let Some(AgentEvent::Result { is_error, result_text, cost_usd, .. }) = &run.result else {
        return Err(failed(format!("the agent stopped without reporting a result ({status})"), true));
    };
    if *is_error || !status.success() {
        let message = if *is_error && !result_text.trim().is_empty() {
            result_text.clone()
        } else {
            format!("the agent stopped unexpectedly ({status})")
        };
        return Err(failed(message, false));
    }
    let answer =
        if result_text.trim().is_empty() { run.last_text.clone().unwrap_or_default() } else { result_text.clone() };
    let details = parse_details(&answer)
        .ok_or_else(|| failed("the agent’s answer wasn’t a title, description and tags".to_owned(), false))?;
    Ok(WrittenDetails { details, cost_usd: *cost_usd })
}

/// The details in the agent's answer: JSON, possibly wrapped in prose or a code fence
/// by an agent that ignores the schema.
pub fn parse_details(answer: &str) -> Option<VideoDetails> {
    let answer = answer.trim();
    if let Ok(details) = serde_json::from_str(answer) {
        return Some(details);
    }
    let start = answer.find('{')?;
    let end = answer.rfind('}')?;
    serde_json::from_str(answer.get(start..=end)?).ok()
}

/// The transcript video-use made of the footage in `footage_dir`: its phrase-level
/// `edit/takes_packed.md`, or else the text of each raw transcript. `None` when the
/// footage was never transcribed. Every take is in it, retakes and slips included.
pub fn read_transcript(footage_dir: &Path) -> Option<String> {
    let edit_dir = footage_dir.join(EDIT_DIR);
    let packed = fs::read_to_string(edit_dir.join(PACKED_TRANSCRIPT)).ok().filter(|text| !text.trim().is_empty());
    let text = packed.or_else(|| raw_transcripts(&edit_dir.join(TRANSCRIPTS_DIR)))?;
    Some(truncate_start(&text, TRANSCRIPT_MAX_CHARS))
}

/// Whether video-use transcribed the footage in `footage_dir`; cheaper than
/// [`read_transcript`].
pub fn has_transcript(footage_dir: &Path) -> bool {
    let edit_dir = footage_dir.join(EDIT_DIR);
    edit_dir.join(PACKED_TRANSCRIPT).is_file()
        || fs::read_dir(edit_dir.join(TRANSCRIPTS_DIR)).is_ok_and(|mut entries| {
            entries.any(|entry| entry.is_ok_and(|entry| entry.path().extension().is_some_and(|ext| ext == "json")))
        })
}

/// `## <take>` and the transcript's text for each raw transcript in `dir`, by name.
fn raw_transcripts(dir: &Path) -> Option<String> {
    let mut files: Vec<_> = fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    let sections: Vec<String> = files
        .iter()
        .filter_map(|path| {
            let json: serde_json::Value = serde_json::from_str(&fs::read_to_string(path).ok()?).ok()?;
            let text = json.get("text")?.as_str()?.trim();
            let take = path.file_stem()?.to_string_lossy();
            (!text.is_empty()).then(|| format!("## {take}\n{text}"))
        })
        .collect();
    (!sections.is_empty()).then(|| sections.join("\n\n"))
}

/// The latest of video-use's notes on the edit (`edit/project.md`): the strategy and
/// what made the cut. `None` without notes.
pub fn read_edit_notes(footage_dir: &Path) -> Option<String> {
    let notes = fs::read_to_string(footage_dir.join(EDIT_DIR).join(PROJECT_NOTES)).ok()?;
    let notes = notes.trim();
    (!notes.is_empty()).then(|| truncate_end(notes, NOTES_MAX_CHARS))
}

/// The prompt for `request`.
pub fn details_prompt(request: &DetailsRequest) -> String {
    let kind = if request.short { "a YouTube Short" } else { "a long-form YouTube video" };
    let title = request.video_title.trim();
    let mut prompt = format!(
        "Write the YouTube title, description and tags for {kind}{about}.\n\n\
         You run headless inside the Rushes app: nobody can answer questions, so make the most \
         sensible choices yourself. Answer only with the structured output.\n\n\
         Guidelines:\n\
         - Write in the language the script and transcript are in.\n\
         - Say only what the video says: the script and transcript below are the source.\n\
         - Title: at most 100 characters, best 40 to 70. Specific and worth clicking; promise only \
         what the video delivers. No ALL CAPS words except acronyms, no hashtags.\n\
         - Description: start with one or two sentences that tell viewers what they get (search \
         results show them), then a short overview of what the video covers. Plain text in short \
         paragraphs, no Markdown. At most 4,000 characters. No timestamps or chapter list of \
         your own: Rushes adds the chapters.\n\
         - Keep the chapter list, links, sponsor mentions, social handles and other standing text \
         the current description has, word for word, after your text.\n\
         - Tags: 5 to 15, the most specific first, at most 400 characters together. Lowercase \
         unless a name, no # signs.\n\
         - Never use the characters < or >: YouTube rejects them.\n\n",
        about = if title.is_empty() { String::new() } else { format!(" titled “{title}” in the creator’s plan") },
    );
    let instructions = request.instructions.trim();
    if !instructions.is_empty() {
        prompt.push_str(&format!(
            "The creator’s instructions, which come before the guidelines:\n<instructions>\n{instructions}\n</instructions>\n\n"
        ));
    }
    let current = &request.current;
    if !current.title.trim().is_empty() || !current.description.trim().is_empty() || !current.tags.is_empty() {
        prompt.push_str(&format!(
            "What the form holds now; replace it, keeping what the guidelines say to keep:\n\
             <current_title>{}</current_title>\n<current_description>\n{}\n</current_description>\n\
             <current_tags>{}</current_tags>\n\n",
            current.title.trim(),
            current.description.trim(),
            current.tags.join(", "),
        ));
    }
    let script = request.script.trim();
    if script.is_empty() {
        prompt.push_str("There is no script.\n\n");
    } else {
        prompt.push_str(&format!(
            "The script (Markdown; talking points are ideas said in the creator’s own words, B-roll and \
             on-screen cues aren’t spoken):\n<script>\n{}\n</script>\n\n",
            truncate_start(script, SCRIPT_MAX_CHARS),
        ));
    }
    match &request.transcript {
        Some(transcript) => prompt.push_str(&format!(
            "The transcript of the recorded footage. It has every take, so retakes and slips repeat \
             lines; what was actually said wins over the script:\n<transcript>\n{}\n</transcript>\n\n",
            transcript.trim(),
        )),
        None => prompt.push_str("The footage hasn’t been transcribed: write from the script.\n\n"),
    }
    if let Some(notes) = &request.edit_notes {
        prompt.push_str(&format!(
            "The video editor’s notes on the edit (strategy and what made the cut):\n<edit_notes>\n{}\n</edit_notes>\n",
            notes.trim(),
        ));
    }
    prompt
}

/// The first `max` characters of `text`, saying when the rest was left out.
fn truncate_start(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((cut, _)) => format!("{}\n[… the rest is left out]", &text[..cut]),
        None => text.to_owned(),
    }
}

/// The last `max` characters of `text`, saying when the start was left out.
fn truncate_end(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_owned();
    }
    let (cut, _) = text.char_indices().nth(count - max).unwrap_or((0, ' '));
    format!("[… earlier notes are left out]\n{}", &text[cut..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_parse_with_or_without_wrapping() {
        let json = r#"{"title":"Sourdough in 24 hours","description":"Bake it.","tags":["sourdough","bread"]}"#;
        let expected = VideoDetails {
            title: "Sourdough in 24 hours".into(),
            description: "Bake it.".into(),
            tags: vec!["sourdough".into(), "bread".into()],
        };
        assert_eq!(parse_details(json), Some(expected.clone()));
        assert_eq!(parse_details(&format!("Here you go:\n```json\n{json}\n```")), Some(expected));
        assert_eq!(parse_details("I couldn't read the script."), None);
        assert_eq!(parse_details(r#"{"title":"Only a title"}"#), None);
    }

    #[test]
    fn the_prompt_carries_the_sources_and_the_creators_words() {
        let request = DetailsRequest {
            video_title: "Sourdough".into(),
            script: "## Starter\nFeed it twice a day.".into(),
            transcript: Some("[0.00-2.10] Feed it twice a day.".into()),
            edit_notes: Some("Kept take 2.".into()),
            current: VideoDetails {
                title: String::new(),
                description: "My course: https://example.com/course".into(),
                tags: vec!["bread".into()],
            },
            instructions: "Mention the course.".into(),
            ..DetailsRequest::default()
        };
        let prompt = details_prompt(&request);
        for part in [
            "a long-form YouTube video titled “Sourdough”",
            "<instructions>\nMention the course.\n</instructions>",
            "https://example.com/course",
            "<current_tags>bread</current_tags>",
            "<script>\n## Starter\nFeed it twice a day.\n</script>",
            "<transcript>\n[0.00-2.10] Feed it twice a day.\n</transcript>",
            "<edit_notes>\nKept take 2.\n</edit_notes>",
        ] {
            assert!(prompt.contains(part), "missing {part:?} in:\n{prompt}");
        }

        let bare = details_prompt(&DetailsRequest { short: true, ..DetailsRequest::default() });
        assert!(bare.contains("for a YouTube Short."), "{bare}");
        assert!(bare.contains("There is no script."), "{bare}");
        assert!(bare.contains("hasn’t been transcribed"), "{bare}");
        assert!(!bare.contains("<instructions>") && !bare.contains("<current_title>"), "{bare}");
    }

    #[test]
    fn the_packed_transcript_wins_over_the_raw_ones() {
        let dir = tempfile::tempdir().unwrap();
        let edit = dir.path().join("edit");
        assert_eq!(read_transcript(dir.path()), None, "never transcribed");
        assert!(!has_transcript(dir.path()));

        fs::create_dir_all(edit.join("transcripts")).unwrap();
        fs::write(edit.join("transcripts/take2.json"), r#"{"text":"Second take.","words":[]}"#).unwrap();
        fs::write(edit.join("transcripts/take1.json"), r#"{"text":"First take.","words":[]}"#).unwrap();
        fs::write(edit.join("transcripts/broken.json"), "{").unwrap();
        assert_eq!(read_transcript(dir.path()).as_deref(), Some("## take1\nFirst take.\n\n## take2\nSecond take."));
        assert!(has_transcript(dir.path()));

        fs::write(edit.join("takes_packed.md"), "## take1\n[0.00-1.20] First take.\n").unwrap();
        assert_eq!(read_transcript(dir.path()).as_deref(), Some("## take1\n[0.00-1.20] First take.\n"));
    }

    #[test]
    fn long_sources_are_cut_where_they_matter_least() {
        assert_eq!(truncate_start("abcdef", 3), "abc\n[… the rest is left out]");
        assert_eq!(truncate_start("abc", 3), "abc");
        assert_eq!(truncate_end("abcdef", 2), "[… earlier notes are left out]\nef");
        assert_eq!(truncate_end("ab", 2), "ab");
        // Characters, not bytes.
        assert_eq!(truncate_start("жжжж", 2), "жж\n[… the rest is left out]");
    }
}
