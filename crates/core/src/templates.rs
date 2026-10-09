//! Script templates for new videos.
//!
//! Each template is a skeleton of sections (headings) with spoken paragraphs,
//! B-roll and on-screen cues, and a production checklist. Placeholder guidance
//! is written in italics so it reads as "replace me" in the editor; the
//! video's hook and title are filled in where they belong.

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
            TemplateKind::Vlog => "Cold open and a three-beat story with B-roll cues.",
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
        if hook.is_empty() { p(guidance) } else { p(hook) }
    };
    let title_card = if video.title.trim().is_empty() { "Title card".to_string() } else { video.title.clone() };

    let mut blocks = match kind {
        TemplateKind::Blank => return vec![Block::paragraph("")],
        TemplateKind::Tutorial => vec![
            h2("Hook · 0:00–0:20"),
            hook(
                "*Lead with the end result. Example: “This is the finished setup, and in the next ten minutes \
                 you'll build it yourself.”*",
            ),
            broll("Show the finished result first: 3–5 seconds of the end state."),
            onscreen(&title_card),
            h2("Intro · who this is for"),
            p("*Who is this for, and what do they need before starting? One or two sentences, then move on.*"),
            bullet("*Prerequisite: tool, account or version viewers need*"),
            bullet("*Prerequisite: knowledge you assume*"),
            h2("Step 1 · set up"),
            p("*Explain why this step matters before showing how. One idea per step.*"),
            broll("Screen recording of step 1, zoomed to the relevant area."),
            onscreen("Step 1 — short label"),
            h2("Step 2 · build"),
            p("*Show the core of the technique. Narrate decisions, not keystrokes.*"),
            broll("Screen recording of step 2; punch in on the key change."),
            onscreen("Step 2 — short label"),
            h2("Step 3 · finish and verify"),
            p("*Prove it works: run it, show the result and connect it back to the hook.*"),
            h2("Common mistakes"),
            bullet("*The mistake viewers hit most often, and the fix*"),
            bullet("*A second gotcha (check comments on similar videos)*"),
            h2("Recap & call to action"),
            p("*Recap each step in one sentence, then point to one next video: “If you want to go further, \
                 watch this one next.”*"),
            onscreen("End screen: next video + subscribe"),
        ],
        TemplateKind::Review => vec![
            h2("Hook · the verdict tease"),
            hook(
                "*Open with a strong opinion or a surprising result: “I used this every day for a month, and \
                 one thing completely changed my mind.”*",
            ),
            broll("Hero shot of the product on the desk, slow push-in."),
            onscreen(&title_card),
            h2("Context"),
            p("*Why you bought it or were sent it, what it costs and what it competes with.*"),
            callout("Disclose sponsorships, affiliate links and loaner units here."),
            onscreen("Price · key specs · release date"),
            h2("First impressions & design"),
            p("*Build quality, size, what's in the box. Keep it brief: viewers came for the verdict.*"),
            broll("Unboxing and macro detail shots."),
            h2("Real-world use"),
            p("*How it performed in your actual workflow. Use specific numbers and moments, not adjectives.*"),
            broll("Product in use; side-by-side with the main competitor."),
            h2("What I don't like"),
            bullet("*The biggest drawback and who it affects*"),
            bullet("*A second drawback, or what you'd change*"),
            h2("Who should buy it"),
            p("*Recommend it for one kind of person, steer another away, and name the alternative.*"),
            h2("Verdict & call to action"),
            p("*One-sentence verdict, then point to the comparison or long-term follow-up video.*"),
            onscreen("Verdict · links in the description"),
        ],
        TemplateKind::Vlog => vec![
            h2("Cold open"),
            hook("*Drop viewers into the most interesting moment of the day, then cut to the intro.*"),
            broll("The moment itself: handheld, natural sound."),
            h2("Intro"),
            p("*Where you are, what today is about, and the question this video answers.*"),
            onscreen(&title_card),
            h2("Beat 1 · the setup"),
            p("*Set up the goal or the problem for the day.*"),
            broll("Establishing shots: location, weather, people."),
            h2("Beat 2 · the turn"),
            p("*Something doesn't go to plan. This is the story, so slow down here.*"),
            broll("Reaction shots and details that show the change."),
            h2("Beat 3 · the payoff"),
            p("*How it resolved and what you learned.*"),
            broll("Closing shot: sunset, timelapse or walking away from camera."),
            h2("Outro"),
            p("*Reflect in one or two sentences and tease the next video.*"),
        ],
        TemplateKind::Short => vec![
            h2("Hook · 0–3s"),
            hook("*One line that stops the scroll.*"),
            onscreen(&title_card),
            h2("Payoff · 3–45s"),
            p("*Deliver one idea fast. Cut every word that doesn't move it forward.*"),
            broll("Fast cuts every 1–2 seconds, framed vertically."),
            h2("Loop · 45–55s"),
            p("*End on a line that flows back into the hook so the Short loops.*"),
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

pub(crate) fn callout(text: &str) -> Block {
    Block::new(BlockKind::Callout, text)
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
