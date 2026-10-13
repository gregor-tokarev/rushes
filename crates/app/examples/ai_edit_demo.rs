//! The AI edit views on a sample video, driven by `scripts/fake-agent`, for trying
//! them by hand and for screenshots. Nothing calls a model.
//!
//! `cargo run -p rushes --example ai_edit_demo -- [--page footage|pipelines|settings]
//!  [--dark] [--empty] [--fail] [--missing] [--delay <ms>] [--font Inter]`
//!
//! `--missing` points the skill folder at an empty directory, so the requirement
//! checklist reports the skill and the ElevenLabs key as missing.
//!
//! Data lives in `$RUSHES_DATA_DIR` (a fresh temporary folder when unset): the
//! database, the media folder with generated takes and a stand-in video-use skill.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Selectable as _, Sizable as _, Theme, ThemeMode, h_flex, v_flex};
use gpui_kit::{
    AppContext as _, Bounds, Context, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, StatefulInteractiveElement as _, Styled as _, Window, WindowBounds, WindowOptions, div, point, px, size,
};
use rushes::ai_edit::{AiEditPanel, AiSettings, PipelineService, PipelinesPage, WithAiEditIcons};
use rushes::state::AppState;
use rushes_core::settings::DATA_DIR_ENV;
use rushes_core::templates::TemplateKind;
use rushes_core::{NewVideo, Store, VideoFormat, VideoId};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Footage,
    Pipelines,
    Settings,
}

impl Page {
    const ALL: [Page; 3] = [Page::Footage, Page::Pipelines, Page::Settings];

    fn label(self) -> &'static str {
        match self {
            Page::Footage => "Footage & AI edit",
            Page::Pipelines => "Pipelines",
            Page::Settings => "AI settings",
        }
    }
}

struct Demo {
    page: Page,
    title: String,
    panel: Entity<AiEditPanel>,
    pipelines: Entity<PipelinesPage>,
    settings: Entity<AiSettings>,
}

impl Demo {
    fn new(page: Page, video: VideoId, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let title = AppState::global(cx).read(cx).video(video).map(|video| video.title.clone()).unwrap_or_default();
        Self {
            page,
            title,
            panel: cx.new(|cx| AiEditPanel::new(video, window, cx)),
            pipelines: cx.new(|cx| PipelinesPage::new(window, cx)),
            settings: cx.new(|cx| AiSettings::new(window, cx)),
        }
    }
}

impl Render for Demo {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let tabs =
            h_flex().gap_1().px_6().py_2().border_b_1().border_color(theme.border).children(Page::ALL.map(|page| {
                Button::new(page.label()).ghost().small().label(page.label()).selected(page == self.page).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.page = page;
                        cx.notify();
                    }),
                )
            }));
        let content = match self.page {
            Page::Pipelines => div().size_full().child(self.pipelines.clone()).into_any_element(),
            Page::Footage | Page::Settings => div()
                .id("demo-scroll")
                .size_full()
                .overflow_y_scroll()
                .child(
                    v_flex()
                        .w_full()
                        .max_w(gpui_kit::rems(56.))
                        .px_10()
                        .py_8()
                        .gap_8()
                        .child(div().text_3xl().font_weight(FontWeight::BOLD).child(match self.page {
                            Page::Footage => format!("🖥️ {}", self.title),
                            _ => "AI editing".to_owned(),
                        }))
                        .child(match self.page {
                            Page::Footage => self.panel.clone().into_any_element(),
                            _ => self.settings.clone().into_any_element(),
                        }),
                )
                .into_any_element(),
        };
        v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(tabs)
            .child(div().flex_1().min_h_0().child(content))
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| args.iter().any(|arg| arg == name);
    let value = |name: &str| args.iter().position(|arg| arg == name).and_then(|ix| args.get(ix + 1)).cloned();
    let page = match value("--page").as_deref() {
        Some("pipelines") => Page::Pipelines,
        Some("settings") => Page::Settings,
        _ => Page::Footage,
    };
    let delay = value("--delay").unwrap_or_else(|| "700".to_owned());
    let font = value("--font");
    let dark = flag("--dark");
    let empty = flag("--empty");
    let fail = flag("--fail");
    let missing = flag("--missing");

    let data = std::env::var_os(DATA_DIR_ENV).map(PathBuf::from).unwrap_or_else(|| {
        let dir = std::env::temp_dir().join(format!("rushes-ai-edit-demo-{}", std::process::id()));
        eprintln!("ai_edit_demo: data in {}", dir.display());
        dir
    });
    let skill = data.join("video-use");
    fs::create_dir_all(skill.join("helpers")).expect("create the demo skill");
    fs::write(skill.join("SKILL.md"), "# video-use (demo stand-in)\n").expect("write SKILL.md");
    fs::write(skill.join(".env"), "ELEVENLABS_API_KEY=demo\n").expect("write .env");

    let store = Store::open(data.join("rushes.db")).expect("open the demo database");
    let mut state = AppState::new(store).expect("load the demo workspace");
    let fake_agent = fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/fake-agent"))
        .expect("scripts/fake-agent");

    let mut agent_env = vec![("FAKE_AGENT_DELAY_MS".to_owned(), delay)];
    if fail {
        agent_env.push(("FAKE_AGENT_MODE".to_owned(), "fail".to_owned()));
    }

    gpui_kit::application().with_assets(WithAiEditIcons(gpui_kit::assets::Assets)).run(move |cx| {
        gpui_kit::init(cx);
        if dark {
            Theme::change(ThemeMode::Dark, None, cx);
        }
        if let Some(font) = font {
            Theme::update(cx, |theme| theme.font_family = font.into());
        }

        let state = cx.new(|cx| {
            state.update_settings(cx, |settings| {
                settings.onboarding_completed = true;
                settings.media_root = Some(data.join("media"));
                settings.pipeline.agent_command = fake_agent.display().to_string();
                settings.pipeline.skill_dir =
                    Some(if missing { data.join("not-installed").join("video-use") } else { skill.clone() });
                settings.pipeline.extra_instructions = "Keep my intro jingle and end on the subscribe card.".into();
                settings.pipeline.default_preferences.target_length = "under 1 minute".into();
            });
            state
        });
        AppState::set_global(state.clone(), cx);
        let video = state.update(cx, |state, cx| match state.rows().first() {
            Some(row) => row.video.id,
            None => {
                let id = state
                    .create_video(
                        NewVideo {
                            title: "Desk setup 2026".into(),
                            format: VideoFormat::LongForm,
                            hook: "Everything on my desk, and the three things I regret buying.".into(),
                            publish_date: None,
                            template: TemplateKind::Review,
                        },
                        cx,
                    )
                    .expect("create the sample video");
                let mut video = state.video(id).cloned().expect("sample video");
                video.icon = Some("🖥️".into());
                state.update_video(video, cx);
                id
            }
        });
        if !empty {
            let footage = state.update(cx, |state, cx| state.ensure_video_folder(video, cx)).expect("media folder");
            make_takes(&footage.join("footage"));
        }
        PipelineService::init(state.clone(), cx);
        PipelineService::global(cx).update(cx, |service, cx| service.set_agent_env(agent_env, cx));

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(point(px(0.), px(0.)), size(px(1440.), px(900.))))),
            ..WindowOptions::default()
        };
        gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| Demo::new(page, video, window, cx)))
            .expect("failed to open the demo window");
    });
}

/// Three short takes (test pattern and tone), unless the folder has some already.
fn make_takes(dir: &Path) {
    if fs::read_dir(dir).is_ok_and(|mut entries| entries.any(|entry| entry.is_ok_and(|e| e.path().is_file()))) {
        return;
    }
    for (name, pattern, seconds) in
        [("take1-intro.mp4", "testsrc", 4), ("take2-desk-tour.mp4", "smptebars", 6), ("take3-outro.mp4", "testsrc2", 3)]
    {
        let status = Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("{pattern}=duration={seconds}:size=1280x720:rate=30"))
            .args(["-f", "lavfi", "-i"])
            .arg(format!("sine=frequency=440:duration={seconds}"))
            .args(["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
            .arg(dir.join(name))
            .status();
        if !status.is_ok_and(|status| status.success()) {
            eprintln!("ai_edit_demo: ffmpeg could not generate {name}");
        }
    }
}
