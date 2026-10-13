//! [`DoctorChecklist`]: what an AI edit needs on this machine, and how to fix what is missing.

use std::path::PathBuf;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, Context, Div, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, Stateful, Styled as _, Subscription, Window, div,
};
use rushes_pipeline::{Check, Doctor, default_skill_dir};

use super::service::PipelineService;

/// Requirement checklist (agent CLI, video-use skill, ffmpeg, ElevenLabs key, Python)
/// with fix hints. Used by onboarding and settings, and inline (compact) by the
/// AI edit panel when something is missing.
///
/// Reads the cached check of [`PipelineService`]; "Re-check" runs it again.
pub struct DoctorChecklist {
    service: Entity<PipelineService>,
    compact: bool,
    _subscription: Subscription,
}

struct Requirement<'a> {
    key: &'static str,
    name: &'static str,
    check: &'a Check,
    fix: String,
}

impl DoctorChecklist {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let service = PipelineService::global(cx);
        let subscription = cx.observe(&service, |_, _, cx| cx.notify());
        Self { service, compact: false, _subscription: subscription }
    }

    /// Show only the missing requirements, for use next to a disabled action.
    pub fn compact(mut self, compact: bool) -> Self {
        self.compact = compact;
        self
    }

    fn recheck(&mut self, cx: &mut Context<Self>) {
        self.service.update(cx, |service, cx| service.refresh_doctor(cx));
    }

    fn render_header(&self, doctor: Option<&Doctor>, checking: bool, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let missing = doctor.map_or(0, |doctor| requirements(doctor, cx).iter().filter(|r| !r.check.ok).count());
        let (icon, title, subtitle): (Option<Icon>, SharedString, SharedString) = match doctor {
            None => (None, "Checking requirements…".into(), "This takes a few seconds.".into()),
            Some(_) if missing == 0 => (
                Some(Icon::new(IconName::CircleCheck).small().text_color(theme.success)),
                "Ready for AI edits".into(),
                "Everything video-use needs is installed.".into(),
            ),
            Some(_) => (
                Some(Icon::new(IconName::TriangleAlert).small().text_color(theme.warning)),
                if missing == 1 {
                    "1 requirement missing".into()
                } else {
                    format!("{missing} requirements missing").into()
                },
                "AI edits can start once these are fixed.".into(),
            ),
        };
        h_flex()
            .gap_3()
            .child(match icon {
                Some(icon) => icon.into_any_element(),
                None => Spinner::new().small().into_any_element(),
            })
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(title))
                    .when(!self.compact, |this| {
                        this.child(div().text_xs().text_color(theme.muted_foreground).child(subtitle))
                    }),
            )
            .child(
                Button::new("doctor-recheck")
                    .ghost()
                    .small()
                    .icon(IconName::RefreshCw)
                    .label("Re-check")
                    .loading(checking)
                    .disabled(checking)
                    .on_click(cx.listener(|this, _, _, cx| this.recheck(cx))),
            )
    }

    fn render_row(&self, requirement: &Requirement, cx: &App) -> Stateful<Div> {
        let theme = cx.theme();
        let ok = requirement.check.ok;
        h_flex()
            .id(requirement.key)
            .items_start()
            .gap_3()
            .child(div().pt_0p5().child(
                Icon::new(if ok { IconName::CircleCheck } else { IconName::CircleX }).small().text_color(if ok {
                    theme.success
                } else {
                    theme.danger
                }),
            ))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(requirement.name))
                    .child(div().text_xs().text_color(theme.muted_foreground).child(requirement.check.detail.clone()))
                    .when(!ok, |this| {
                        this.child(
                            div().pt_1().text_xs().child(
                                TextView::markdown(format!("doctor-fix-{}", requirement.key), requirement.fix.clone())
                                    .selectable(true),
                            ),
                        )
                    }),
            )
    }
}

impl Render for DoctorChecklist {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let service = self.service.read(cx);
        let doctor = service.doctor().cloned();
        let checking = service.is_checking();
        let header = self.render_header(doctor.as_ref(), checking, cx);
        let rows: Vec<_> = doctor.as_ref().map(|doctor| requirements(doctor, cx)).unwrap_or_default();
        let shown: Vec<_> = rows.iter().filter(|row| !self.compact || !row.check.ok).collect();

        // A plain list: the checklist sits in a page or a card that already frames it.
        v_flex().w_full().gap_4().child(header).when(!shown.is_empty(), |this| {
            this.child(v_flex().gap_3().children(shown.iter().map(|row| self.render_row(row, cx))))
        })
    }
}

fn requirements<'a>(doctor: &'a Doctor, cx: &App) -> Vec<Requirement<'a>> {
    let skill_dir = doctor
        .skill_dir
        .clone()
        .or_else(|| PipelineService::global(cx).read(cx).state().read(cx).settings().pipeline.skill_dir.clone())
        .or_else(default_skill_dir)
        .unwrap_or_else(|| PathBuf::from("~/.claude/skills/video-use"));
    let skill = skill_dir.display();
    let install_ffmpeg = if cfg!(target_os = "macos") {
        "Install ffmpeg with `brew install ffmpeg`"
    } else if cfg!(windows) {
        "Install ffmpeg with `winget install ffmpeg`"
    } else {
        "Install ffmpeg with your package manager, for example `sudo apt install ffmpeg`"
    };
    vec![
        Requirement {
            key: "agent",
            name: "Claude Code",
            check: &doctor.agent,
            fix: "Install it with `npm install -g @anthropic-ai/claude-code` and run `claude` once to sign in, \
                  or set the agent command in Settings > AI editing."
                .to_owned(),
        },
        Requirement {
            key: "skill",
            name: "video-use skill",
            check: &doctor.skill,
            fix: format!(
                "Install it with `git clone https://github.com/browser-use/video-use ~/.claude/skills/video-use`, \
                 or choose the folder you cloned it to. Currently looking in `{skill}`."
            ),
        },
        Requirement {
            key: "ffmpeg",
            name: "ffmpeg",
            check: &doctor.ffmpeg,
            fix: format!("{install_ffmpeg}; video-use cuts and renders with it."),
        },
        Requirement {
            key: "ffprobe",
            name: "ffprobe",
            check: &doctor.ffprobe,
            fix: format!("{install_ffmpeg}; ffprobe comes with it."),
        },
        Requirement {
            key: "elevenlabs",
            name: "ElevenLabs API key",
            check: &doctor.elevenlabs,
            fix: format!(
                "video-use transcribes the footage with ElevenLabs. Add a line `ELEVENLABS_API_KEY=<your key>` to \
                 `{skill}/.env`, or set `ELEVENLABS_API_KEY` in the environment Rushes starts from."
            ),
        },
        Requirement {
            key: "python",
            name: "Python",
            check: &doctor.python,
            fix: format!(
                "Install uv (`curl -LsSf https://astral.sh/uv/install.sh | sh`), then run `uv sync` in `{skill}`."
            ),
        },
    ]
}
