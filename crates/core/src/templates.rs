//! Script templates for new videos.
//!
//! Each template is a skeleton of sections (headings) with spoken paragraphs or
//! talking points, B-roll and on-screen cues, and a production checklist.
//! Guidance for the creator is the placeholder of an empty block
//! ([`Block::guided`]): the editor shows it until the creator writes there, and
//! it never counts as script.
//! The video's hook and title are filled in where they belong.

use serde::{Deserialize, Serialize};

use crate::model::{Block, BlockKind, Video, VideoFormat};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TemplateKind {
    #[default]
    Blank,
    Tutorial,
    Review,
    Vlog,
    Short,
}

impl TemplateKind {
    pub const ALL: [TemplateKind; 5] =
        [TemplateKind::Blank, TemplateKind::Tutorial, TemplateKind::Review, TemplateKind::Vlog, TemplateKind::Short];

    pub fn label(self) -> &'static str {
        match self {
            TemplateKind::Blank => "Blank",
            TemplateKind::Tutorial => "Tutorial",
            TemplateKind::Review => "Review",
            TemplateKind::Vlog => "Vlog",
            TemplateKind::Short => "Short",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            TemplateKind::Blank => "An empty script.",
            TemplateKind::Tutorial => "Hook, prerequisites, three steps, common mistakes and a recap.",
            TemplateKind::Review => "Verdict tease, context, real-world use, drawbacks and who should buy it.",
            TemplateKind::Vlog => "Cold open and a three-beat story told from talking points, with B-roll cues.",
            TemplateKind::Short => "Hook, payoff and loop for a vertical video under 60 seconds.",
        }
    }

    /// Default page icon for videos created from this template.
    pub fn icon(self) -> Option<&'static str> {
        match self {
            TemplateKind::Blank => None,
            TemplateKind::Tutorial => Some("🛠️"),
            TemplateKind::Review => Some("⭐"),
            TemplateKind::Vlog => Some("🎥"),
            TemplateKind::Short => Some("⚡"),
        }
    }
}

/// Script blocks for a new video, personalised with its title, hook and format.
pub fn script_template(kind: TemplateKind, video: &Video) -> Vec<Block> {
    let hook = |guidance: &str| {
        let hook = video.hook.trim();
        if hook.is_empty() { guide(guidance) } else { p(hook) }
    };
    let title_card = if video.title.trim().is_empty() { "Title card".to_string() } else { video.title.clone() };

    let mut blocks = match kind {
        TemplateKind::Blank => return vec![Block::paragraph("")],
        TemplateKind::Tutorial => vec![
            h2("Hook · 0:00–0:20"),
            hook(
                "Lead with the end result. Example: “This is the finished setup, and in the next ten minutes \
                 you'll build it yourself.”",
            ),
            broll("Show the finished result first: 3–5 seconds of the end state."),
            onscreen(&title_card),
            h2("Intro · who this is for"),
            guide("Who is this for, and what do they need before starting? One or two sentences, then move on."),
            guide_bullet("Prerequisite: tool, account or version viewers need"),
            guide_bullet("Prerequisite: knowledge you assume"),
            h2("Step 1 · set up"),
            guide("Explain why this step matters before showing how. One idea per step."),
            broll("Screen recording of step 1, zoomed to the relevant area."),
            onscreen("Step 1 — short label"),
            h2("Step 2 · build"),
            guide("Show the core of the technique. Narrate decisions, not keystrokes."),
            broll("Screen recording of step 2; punch in on the key change."),
            onscreen("Step 2 — short label"),
            h2("Step 3 · finish and verify"),
            guide("Prove it works: run it, show the result and connect it back to the hook."),
            h2("Common mistakes"),
            guide_bullet("The mistake viewers hit most often, and the fix"),
            guide_bullet("A second gotcha (check comments on similar videos)"),
            h2("Recap & call to action"),
            guide(
                "Recap each step in one sentence, then point to one next video: “If you want to go further, \
                 watch this one next.”",
            ),
            onscreen("End screen: next video + subscribe"),
        ],
        TemplateKind::Review => vec![
            h2("Hook · the verdict tease"),
            hook(
                "Open with a strong opinion or a surprising result: “I used this every day for a month, and \
                 one thing completely changed my mind.”",
            ),
            broll("Hero shot of the product on the desk, slow push-in."),
            onscreen(&title_card),
            h2("Context"),
            guide("Why you bought it or were sent it, what it costs and what it competes with."),
            callout("Disclose sponsorships, affiliate links and loaner units here."),
            onscreen("Price · key specs · release date"),
            h2("First impressions & design"),
            guide("Build quality, size, what's in the box. Keep it brief: viewers came for the verdict."),
            broll("Unboxing and macro detail shots."),
            h2("Real-world use"),
            guide("How it performed in your actual workflow. Use specific numbers and moments, not adjectives."),
            broll("Product in use; side-by-side with the main competitor."),
            h2("What I don't like"),
            guide_bullet("The biggest drawback and who it affects"),
            guide_bullet("A second drawback, or what you'd change"),
            h2("Who should buy it"),
            guide("Recommend it for one kind of person, steer another away, and name the alternative."),
            h2("Verdict & call to action"),
            guide("One-sentence verdict, then point to the comparison or long-term follow-up video."),
            onscreen("Verdict · links in the description"),
        ],
        TemplateKind::Vlog => vec![
            h2("Cold open"),
            hook("Drop viewers into the most interesting moment of the day, then cut to the intro."),
            broll("The moment itself: handheld, natural sound."),
            h2("Intro"),
            guide("Where you are, what today is about, and the question this video answers."),
            onscreen(&title_card),
            h2("Beat 1 · the setup"),
            guide_point("The goal or the problem for the day"),
            broll("Establishing shots: location, weather, people."),
            h2("Beat 2 · the turn"),
            guide_point("What didn't go to plan: this is the story, so slow down here"),
            broll("Reaction shots and details that show the change."),
            h2("Beat 3 · the payoff"),
            guide_point("How it resolved and what you learned"),
            broll("Closing shot: sunset, timelapse or walking away from camera."),
            h2("Outro"),
            guide("Reflect in one or two sentences and tease the next video."),
        ],
        TemplateKind::Short => vec![
            h2("Hook · 0–3s"),
            hook("One line that stops the scroll."),
            onscreen(&title_card),
            h2("Payoff · 3–45s"),
            guide("Deliver one idea fast. Cut every word that doesn't move it forward."),
            broll("Fast cuts every 1–2 seconds, framed vertically."),
            h2("Loop · 45–55s"),
            guide("End on a line that flows back into the hook so the Short loops."),
        ],
    };

    blocks.push(Block::new(BlockKind::Divider, ""));
    blocks.push(h3("Production checklist"));
    blocks.extend(production_checklist(kind, video.format).into_iter().map(todo));
    blocks
}

fn production_checklist(kind: TemplateKind, format: VideoFormat) -> Vec<&'static str> {
    let mut items = vec!["Record A-roll", "Capture the B-roll from the cues above"];
    let specific: &[&str] = match kind {
        TemplateKind::Blank => &[],
        TemplateKind::Tutorial => &["Capture screen recordings for every step", "Test each step on a clean setup"],
        TemplateKind::Review => &["Shoot product beauty shots", "Double-check price, specs and disclosure"],
        TemplateKind::Vlog => &["Charge batteries and clear SD cards", "Pick music"],
        TemplateKind::Short => &["Frame vertically (9:16) inside the safe area", "Burn in captions"],
    };
    items.extend(specific);
    match format {
        VideoFormat::Short if kind != TemplateKind::Short => items.push("Export a vertical 9:16 cut"),
        VideoFormat::Livestream => items.push("Test stream key, scenes and audio before going live"),
        _ => {}
    }
    items.extend(["Design the thumbnail", "Write title, description and chapters"]);
    items
}

pub(crate) fn h2(text: &str) -> Block {
    Block::new(BlockKind::Heading2, text)
}

pub(crate) fn h3(text: &str) -> Block {
    Block::new(BlockKind::Heading3, text)
}

pub(crate) fn p(text: &str) -> Block {
    Block::paragraph(text)
}

pub(crate) fn bullet(text: &str) -> Block {
    Block::new(BlockKind::Bulleted, text)
}

/// An empty paragraph showing `guidance` until the creator writes there.
fn guide(guidance: &str) -> Block {
    Block::guided(BlockKind::Paragraph, guidance)
}

/// An empty bullet showing `guidance` until the creator writes there.
fn guide_bullet(guidance: &str) -> Block {
    Block::guided(BlockKind::Bulleted, guidance)
}

/// An empty talking point showing `guidance` until the creator writes there.
fn guide_point(guidance: &str) -> Block {
    Block::guided(BlockKind::TalkingPoint, guidance)
}

pub(crate) fn callout(text: &str) -> Block {
    Block::new(BlockKind::Callout, text)
}

pub(crate) fn talking_point(text: &str) -> Block {
    Block::new(BlockKind::TalkingPoint, text)
}

pub(crate) fn broll(text: &str) -> Block {
    Block::new(BlockKind::BRoll, text)
}

pub(crate) fn onscreen(text: &str) -> Block {
    Block::new(BlockKind::OnScreen, text)
}

pub(crate) fn todo(text: &str) -> Block {
    Block::new(BlockKind::Todo, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{
        DEFAULT_WPM, estimated_runtime_secs, outline, spoken_word_count, teleprompter_text, to_markdown,
    };

    fn count(blocks: &[Block], kind: BlockKind) -> usize {
        blocks.iter().filter(|block| block.kind == kind).count()
    }

    #[test]
    fn every_template_has_metadata() {
        for kind in TemplateKind::ALL {
            assert!(!kind.label().is_empty());
            assert!(!kind.description().is_empty());
        }
        assert_eq!(TemplateKind::default(), TemplateKind::Blank);
    }

    #[test]
    fn blank_is_a_single_empty_paragraph() {
        let blocks = script_template(TemplateKind::Blank, &Video::new("Anything"));
        assert_eq!(blocks.len(), 1);
        assert_eq!((blocks[0].kind, blocks[0].text.as_str()), (BlockKind::Paragraph, ""));
    }

    #[test]
    fn templates_have_sections_cues_and_a_checklist() {
        let video = Video::new("Rust in 10 minutes");
        for kind in TemplateKind::ALL.into_iter().filter(|kind| *kind != TemplateKind::Blank) {
            let blocks = script_template(kind, &video);
            assert!(outline(&blocks).len() >= 3, "{kind:?} has sections");
            assert!(
                blocks.iter().any(|block| block.kind == BlockKind::Paragraph && block.guidance().is_some()),
                "{kind:?} has guided paragraphs to write the script in"
            );
            assert!(count(&blocks, BlockKind::BRoll) >= 1, "{kind:?} has B-roll cues");
            assert!(count(&blocks, BlockKind::OnScreen) >= 1, "{kind:?} has on-screen cues");
            assert!(count(&blocks, BlockKind::Todo) >= 4, "{kind:?} has a checklist");
            assert_eq!(blocks[0].kind, BlockKind::Heading2, "{kind:?} opens with the hook section");
            assert!(blocks.iter().all(|block| {
                !block.text.trim().is_empty() || block.kind == BlockKind::Divider || block.guidance().is_some()
            }));
            let ids: std::collections::HashSet<_> = blocks.iter().map(|block| block.id).collect();
            assert_eq!(ids.len(), blocks.len(), "{kind:?} block ids are unique");
        }
    }

    #[test]
    fn vlog_beats_are_talking_points() {
        let blocks = script_template(TemplateKind::Vlog, &Video::new("A day in Osaka"));
        let points: Vec<_> = blocks.iter().filter(|block| block.kind == BlockKind::TalkingPoint).collect();
        assert_eq!(points.len(), 3);
        assert!(points.iter().all(|point| point.guidance().is_some()));
    }

    #[test]
    fn templates_use_title_and_hook() {
        let mut video = Video::new("I tried every keyboard switch");
        let blocks = script_template(TemplateKind::Review, &video);
        assert!(blocks.iter().any(|b| b.kind == BlockKind::OnScreen && b.text == "I tried every keyboard switch"));
        assert_eq!(blocks[1].text, "", "without a hook the paragraph only shows guidance");
        assert!(blocks[1].guidance().is_some_and(|guidance| guidance.starts_with("Open with")));

        video.hook = "  Ninety switches, one winner.  ".into();
        let blocks = script_template(TemplateKind::Review, &video);
        assert_eq!((blocks[1].kind, blocks[1].text.as_str()), (BlockKind::Paragraph, "Ninety switches, one winner."));
    }

    #[test]
    fn guidance_is_never_part_of_the_script() {
        for kind in TemplateKind::ALL {
            let blocks = script_template(kind, &Video::new("Unwritten"));
            assert_eq!(spoken_word_count(&blocks), 0, "{kind:?} guidance is not spoken");
            assert_eq!(teleprompter_text(&blocks), "", "{kind:?} guidance is not read out");
            let markdown = to_markdown(&blocks);
            for guidance in blocks.iter().filter_map(Block::guidance) {
                assert!(!markdown.contains(guidance), "{kind:?} exports {guidance:?}");
            }
        }

        // Written text replaces the guidance everywhere.
        let mut blocks = script_template(TemplateKind::Vlog, &Video::new("Written"));
        let guided = blocks.iter().position(|block| block.guidance().is_some()).unwrap();
        blocks[guided].text = "We got lost in Osaka.".into();
        assert_eq!(blocks[guided].guidance(), None);
        assert_eq!(spoken_word_count(&blocks), 5);
        assert_eq!(teleprompter_text(&blocks), "We got lost in Osaka.");
    }

    #[test]
    fn short_template_fits_in_a_minute() {
        let video = Video {
            hook: "This one Python trick saves me an hour every week, and nobody talks about it".into(),
            ..Video::new("Python trick")
        };
        let blocks = script_template(TemplateKind::Short, &video);
        assert!(estimated_runtime_secs(&blocks, DEFAULT_WPM) < 60);
        assert!(blocks.iter().any(|b| b.text.contains("9:16")));
    }

    #[test]
    fn checklist_follows_the_format() {
        let has = |kind, format, item: &str| {
            let video = Video { format, ..Video::new("Video") };
            script_template(kind, &video).iter().any(|b| b.kind == BlockKind::Todo && b.text.contains(item))
        };
        assert!(has(TemplateKind::Tutorial, VideoFormat::Livestream, "stream key"));
        assert!(!has(TemplateKind::Tutorial, VideoFormat::LongForm, "stream key"));
        assert!(has(TemplateKind::Vlog, VideoFormat::Short, "Export a vertical 9:16 cut"));
        assert!(!has(TemplateKind::Short, VideoFormat::Short, "Export a vertical 9:16 cut"));
    }
}
