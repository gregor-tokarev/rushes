//! Opens the block editor on a sample YouTube script for manual testing.
//!
//! ```sh
//! cargo run -p videocrm --example editor_demo            # sample script
//! cargo run -p videocrm --example editor_demo -- --dark  # dark theme
//! cargo run -p videocrm --example editor_demo -- --blocks 300
//! cargo run -p videocrm --example editor_demo -- --font Inter
//! ```

use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode, h_flex, v_flex};
use gpui_kit::{
    AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, TitlebarOptions, Window, WindowBounds, WindowOptions, div, px,
    size,
};
use videocrm::editor::{DocumentEditor, DocumentEditorEvent};
use videocrm_core::{Block, BlockKind, Document, DocumentId, DocumentKind, VideoId};

struct Demo {
    editor: Entity<DocumentEditor>,
    edits: usize,
    _subscription: Subscription,
}

impl Demo {
    fn new(document: Document, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let editor = cx.new(|cx| DocumentEditor::new(document, window, cx));
        let subscription = cx.subscribe(&editor, |this, _, event, cx| {
            if *event == DocumentEditorEvent::Changed {
                this.edits += 1;
                cx.notify();
            }
        });
        Self { editor, edits: 0, _subscription: subscription }
    }
}

impl Render for Demo {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let document = self.editor.read(cx).document(cx);
        let words: usize = document.blocks.iter().map(|block| block.text.split_whitespace().count()).sum();
        let status: SharedString = format!(
            "{} blocks · {} words · {}",
            document.blocks.len(),
            words,
            if self.edits == 0 { "No changes".to_owned() } else { format!("{} edits", self.edits) }
        )
        .into();

        v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                div().id("page").flex_1().min_h_0().overflow_y_scrollbar().child(
                    v_flex()
                        .w_full()
                        .pt_16()
                        .child(
                            v_flex()
                                .w_full()
                                .items_center()
                                .child(
                                    v_flex()
                                        .w_full()
                                        .max_w(gpui_kit::rems(46.))
                                        .gap_1()
                                        .child(div().text_3xl().font_weight(gpui_kit::FontWeight::BOLD).child(document.title))
                                        .child(
                                            div()
                                                .text_sm()
                                                .text_color(theme.muted_foreground)
                                                .child("Script · Long-form · Scripting"),
                                        ),
                                ),
                        )
                        .child(div().h_6())
                        .child(self.editor.clone()),
                ),
            )
            .child(
                h_flex()
                    .flex_none()
                    .h_8()
                    .px_4()
                    .border_t_1()
                    .border_color(theme.border)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(status),
            )
    }
}

fn block(kind: BlockKind, text: &str) -> Block {
    Block::new(kind, text)
}

fn sample_script() -> Vec<Block> {
    vec![
        block(BlockKind::Heading2, "Hook"),
        block(
            BlockKind::Paragraph,
            "What if I told you that **90% of editing time** goes into work an AI can do in seconds?",
        ),
        block(BlockKind::BRoll, "Fast montage of timeline scrubbing, cut to a render bar hitting 100%"),
        block(BlockKind::OnScreen, "90% of editing = busywork"),
        block(BlockKind::Heading2, "Intro"),
        block(
            BlockKind::Paragraph,
            "Hey, I'm Sam. I've published *every week* for three years, and today I'm showing you the exact pipeline I use, with [video-use](https://github.com/browser-use/video-use) doing the heavy lifting.",
        ),
        block(BlockKind::Callout, "Keep the intro under **20 seconds**. Retention drops hard after that."),
        block(BlockKind::Heading2, "The workflow"),
        block(BlockKind::Numbered, "Record everything in one take, mistakes included"),
        block(BlockKind::Numbered, "Let the agent cut the `ums`, silences and retakes"),
        Block { indent: 1, ..block(BlockKind::Numbered, "It reads the transcript, not the waveform") },
        Block { indent: 1, ..block(BlockKind::Numbered, "Every cut lands on a word boundary") },
        block(BlockKind::Numbered, "Review the edit plan before anything renders"),
        block(BlockKind::BRoll, "Screen recording: the agent's plan scrolling by in the terminal"),
        block(BlockKind::Paragraph, "What you get back is a first cut with:"),
        block(BlockKind::Bulleted, "Color grade presets applied per scene"),
        block(BlockKind::Bulleted, "Burned-in subtitles in your brand font"),
        Block { indent: 1, ..block(BlockKind::Bulleted, "Styled like the rest of the channel") },
        block(BlockKind::Quote, "The best edit is the one viewers never notice."),
        block(BlockKind::Divider, ""),
        block(BlockKind::Heading3, "Production checklist"),
        Block { checked: true, ..block(BlockKind::Todo, "Write the hook") },
        Block { checked: true, ..block(BlockKind::Todo, "Film A-roll") },
        block(BlockKind::Todo, "Capture screen recordings for B-roll"),
        block(BlockKind::Todo, "Design the thumbnail"),
        block(BlockKind::Code, "claude -p \"edit footage/ with video-use\" \\\n  --output-format stream-json"),
        block(BlockKind::Heading2, "Outro"),
        block(
            BlockKind::Paragraph,
            "If this saved you an hour, **subscribe**. Next week I'm automating thumbnails.",
        ),
        block(BlockKind::OnScreen, "Subscribe for part 2 →"),
    ]
}

/// A long document for checking that editing stays smooth.
fn long_document(count: usize) -> Vec<Block> {
    let script = sample_script();
    (0..count).map(|ix| Block { id: videocrm_core::BlockId::new(), ..script[ix % script.len()].clone() }).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dark = args.iter().any(|arg| arg == "--dark");
    let value = |flag: &str| args.iter().position(|arg| arg == flag).and_then(|ix| args.get(ix + 1)).cloned();
    let count = value("--blocks").and_then(|count| count.parse().ok());
    let font = value("--font");

    let document = Document {
        id: DocumentId::new(),
        video_id: VideoId::new(),
        kind: DocumentKind::Script,
        title: "I let AI edit my videos for a month".into(),
        blocks: count.map(long_document).unwrap_or_else(sample_script),
        position: 0,
        updated_at: chrono::Utc::now(),
    };

    gpui_kit::application().with_assets(videocrm::editor::Assets).run(move |cx| {
        gpui_kit::init(cx);
        if dark {
            Theme::change(ThemeMode::Dark, None, cx);
        }
        if let Some(font) = font {
            Theme::update(cx, |theme| theme.font_family = font.into());
        }
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(1180.), px(860.)), cx)),
            titlebar: Some(TitlebarOptions { title: Some("Editor demo".into()), ..Default::default() }),
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| Demo::new(document, window, cx)))
            .expect("failed to open the demo window");
    });
}
