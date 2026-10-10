//! First-run onboarding: what VideoCRM is, the media folder, YouTube, AI
//! editing, and how to start. Embeds `publish::YoutubeSettings` and
//! `ai_edit::DoctorChecklist`.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::stepper::{Stepper, StepperItem};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Rems,
    Render, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div,
    rems,
};

use crate::ai_edit::DoctorChecklist;
use crate::publish::YoutubeSettings;
use crate::settings_page::{Commit, MediaFolderField};
use crate::state::AppState;
use crate::ui::ChoiceCard;
use crate::ui::settings::FieldStack;
use crate::wizard;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Welcome,
    MediaFolder,
    Youtube,
    AiEditing,
    GetStarted,
}

impl Step {
    pub const ALL: [Step; 5] = [Step::Welcome, Step::MediaFolder, Step::Youtube, Step::AiEditing, Step::GetStarted];

    fn index(self) -> usize {
        Step::ALL.iter().position(|step| *step == self).unwrap_or(0)
    }

    fn label(self) -> &'static str {
        match self {
            Step::Welcome => "Welcome",
            Step::MediaFolder => "Media folder",
            Step::Youtube => "YouTube",
            Step::AiEditing => "AI editing",
            Step::GetStarted => "Get started",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Step::Welcome => IconName::Sparkles,
            Step::MediaFolder => IconName::FolderOpen,
            Step::Youtube => IconName::CirclePlay,
            Step::AiEditing => IconName::WandSparkles,
            Step::GetStarted => IconName::Rocket,
        }
    }
}

/// How the workspace starts once onboarding completes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StartChoice {
    #[default]
    SampleVideos,
    FirstVideo,
    Empty,
}

pub struct Onboarding {
    state: Entity<AppState>,
    step: Step,
    media_folder: Entity<MediaFolderField>,
    /// The media folder is being checked before the next step shows.
    checking: bool,
    youtube: Option<Entity<YoutubeSettings>>,
    doctor: Option<Entity<DoctorChecklist>>,
    choice: StartChoice,
    scroll: ScrollHandle,
}

impl Onboarding {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            state: AppState::global(cx),
            step: Step::Welcome,
            media_folder: cx.new(|cx| MediaFolderField::new("onboarding-media-root", window, cx)),
            checking: false,
            youtube: None,
            doctor: None,
            choice: StartChoice::default(),
            scroll: ScrollHandle::new(),
        }
    }

    pub fn step(&self) -> Step {
        self.step
    }

    pub fn choice(&self) -> StartChoice {
        self.choice
    }

    pub fn set_choice(&mut self, choice: StartChoice, cx: &mut Context<Self>) {
        self.choice = choice;
        cx.notify();
    }

    pub fn next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.checking {
            return;
        }
        if self.step == Step::MediaFolder {
            // The folder is created and checked now, so the first video can't fail on it.
            let saved = self.media_folder.update(cx, |field, cx| field.commit(Commit::Keep, true, window, cx));
            self.checking = true;
            cx.notify();
            cx.spawn_in(window, async move |this, cx| {
                let saved = saved.await;
                _ = this.update_in(cx, |this, window, cx| {
                    this.checking = false;
                    if saved && this.step == Step::MediaFolder {
                        this.show(Step::Youtube, window, cx);
                    }
                    cx.notify();
                });
            })
            .detach();
            return;
        }
        match Step::ALL.get(self.step.index() + 1) {
            Some(step) => self.show(*step, window, cx),
            None => self.finish(window, cx),
        }
    }

    pub fn back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(step) = self.step.index().checked_sub(1).and_then(|ix| Step::ALL.get(ix)) {
            self.show(*step, window, cx);
        }
    }

    fn show(&mut self, step: Step, window: &mut Window, cx: &mut Context<Self>) {
        self.step = step;
        match step {
            Step::Youtube if self.youtube.is_none() => {
                self.youtube = Some(cx.new(|cx| YoutubeSettings::new(window, cx)));
            }
            Step::AiEditing if self.doctor.is_none() => {
                self.doctor = Some(cx.new(|cx| DoctorChecklist::new(window, cx)));
            }
            _ => {}
        }
        self.scroll.set_offset(Default::default());
        cx.notify();
    }

    /// Completes onboarding and starts the workspace the chosen way.
    pub fn finish(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let choice = self.choice;
        self.state.update(cx, |state, cx| {
            if choice == StartChoice::SampleVideos {
                state.seed_sample_workspace(cx);
            }
            state.complete_onboarding(cx);
        });
        if choice == StartChoice::FirstVideo {
            window.defer(cx, |window, cx| wizard::open_new_video_dialog(None, window, cx));
        }
    }

    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.step {
            Step::Welcome => {
                let feature = |icon: IconName, title: &'static str, description: &'static str| {
                    h_flex()
                        .items_start()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .flex_none()
                                .size_9()
                                .items_center()
                                .justify_center()
                                .rounded(cx.theme().radius_lg)
                                .bg(cx.theme().muted)
                                .child(Icon::new(icon).text_color(cx.theme().foreground)),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_0p5()
                                .child(div().text_sm().font_medium().child(title))
                                .child(div().text_sm().text_color(cx.theme().muted_foreground).child(description)),
                        )
                };
                v_flex()
                    .gap_4()
                    .child(feature(
                        IconName::Sheet,
                        "Plan videos in a database",
                        "Track every idea through scripting, filming, editing and publishing, as a table or a board.",
                    ))
                    .child(feature(
                        IconName::ScrollText,
                        "Write scripts in rich documents",
                        "Notion-style blocks with B-roll and on-screen cues, word count and runtime.",
                    ))
                    .child(feature(
                        IconName::WandSparkles,
                        "Edit with AI",
                        "Hand your footage and script to an AI editor, then approve or refine its cut.",
                    ))
                    .child(feature(
                        IconName::CloudUpload,
                        "Upload to YouTube",
                        "Prepare title, description and tags on the page and upload straight from it.",
                    ))
                    .into_any_element()
            }
            // The same field as in Settings > General.
            Step::MediaFolder => FieldStack::new("Media folder")
                .description("Each video gets its own folder inside, with footage/ for raw takes. You can change this later in Settings.")
                .control(self.media_folder.clone())
                .into_any_element(),
            Step::Youtube => self.youtube.clone().map_or_else(|| div().into_any_element(), IntoElement::into_any_element),
            Step::AiEditing => self.doctor.clone().map_or_else(|| div().into_any_element(), IntoElement::into_any_element),
            Step::GetStarted => {
                let choice = |id: &'static str, value: StartChoice, icon: IconName, title: &'static str, description: &'static str| {
                    ChoiceCard::new(id, Icon::new(icon).small(), title, description)
                        .selected(self.choice == value)
                        .on_click(cx.listener(move |this, _, _, cx| this.set_choice(value, cx)))
                };
                v_flex()
                    .gap_2()
                    .child(choice(
                        "start-samples",
                        StartChoice::SampleVideos,
                        IconName::Sparkles,
                        "Start with sample videos",
                        "A few example videos with scripts, to see how planning, writing and publishing fit together.",
                    ))
                    .child(choice(
                        "start-first-video",
                        StartChoice::FirstVideo,
                        IconName::Plus,
                        "Create my first video",
                        "Open the New video dialog and start with a script template.",
                    ))
                    .child(choice(
                        "start-empty",
                        StartChoice::Empty,
                        IconName::Inbox,
                        "Start empty",
                        "An empty database, ready for your ideas.",
                    ))
                    .into_any_element()
            }
        }
    }
}

fn step_copy(step: Step) -> (&'static str, &'static str) {
    match step {
        Step::Welcome => (
            "Welcome to VideoCRM",
            "Your production home for YouTube videos, from the first idea to the published upload.",
        ),
        Step::MediaFolder => {
            ("Where should footage live?", "VideoCRM keeps one folder per video for raw takes and AI edit outputs.")
        }
        Step::Youtube => {
            ("Connect YouTube", "Upload videos and keep their links on each page. You can do this later in Settings.")
        }
        Step::AiEditing => (
            "Set up AI editing",
            "AI edits run on this computer through Claude Code and video-use. This is optional; everything else works without it.",
        ),
        Step::GetStarted => ("How do you want to start?", "You can add, change or delete videos at any time."),
    }
}

/// Windows shorter than this give the onboarding card less room above it.
const SHORT_WINDOW: Rems = Rems(48.);
/// Room for the tallest step (YouTube) in a comfortable window.
const CARD_MAX_HEIGHT: Rems = Rems(44.);

impl Render for Onboarding {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let short = window.viewport_size().height < SHORT_WINDOW.to_pixels(window.rem_size());
        let (title, description): (SharedString, SharedString) = {
            let (title, description) = step_copy(self.step);
            (title.into(), description.into())
        };
        let first = self.step == Step::Welcome;
        let last = self.step == Step::GetStarted;
        let optional = matches!(self.step, Step::Youtube | Step::AiEditing);

        // The stepper and the buttons stay in view; the step between them
        // scrolls. One height for every step keeps Back and Continue in place.
        let card = v_flex()
            .id("onboarding-card")
            .test_support()
            .w_full()
            .max_w(rems(42.5))
            .h_full()
            .max_h(CARD_MAX_HEIGHT)
            .min_h_0()
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .shadow_lg()
            .child(
                div().flex_none().px_8().pt_6().pb_5().border_b_1().border_color(cx.theme().border).child(
                    Stepper::new("onboarding-steps")
                        .small()
                        .text_center(true)
                        .selected_index(self.step.index())
                        .items(
                            Step::ALL
                                .iter()
                                .map(|step| StepperItem::new().icon(Icon::new(step.icon())).child(step.label())),
                        )
                        .on_click(cx.listener(|this, ix: &usize, window, cx| {
                            // Steps already passed can be revisited; later ones come through Next.
                            if *ix < this.step.index() {
                                this.show(Step::ALL[*ix], window, cx);
                            }
                        })),
                ),
            )
            .child(
                div()
                    .id("onboarding-step")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(
                        v_flex()
                            .px_8()
                            .pt_6()
                            .pb_6()
                            .gap_5()
                            .child(
                                v_flex()
                                    .gap_1()
                                    .child(div().text_2xl().font_semibold().child(title))
                                    .child(div().text_sm().text_color(cx.theme().muted_foreground).child(description)),
                            )
                            .child(self.render_body(cx)),
                    )
                    .vertical_scrollbar(&self.scroll),
            )
            .child(
                h_flex()
                    .flex_none()
                    .px_8()
                    .py_4()
                    .gap_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .when(!first, |this| {
                        this.child(
                            Button::new("onboarding-back")
                                .ghost()
                                .label("Back")
                                .on_click(cx.listener(|this, _, window, cx| this.back(window, cx))),
                        )
                    })
                    .child(div().flex_1())
                    .when(optional, |this| {
                        this.child(
                            Button::new("onboarding-skip")
                                .ghost()
                                .label("Skip for now")
                                .on_click(cx.listener(|this, _, window, cx| this.next(window, cx))),
                        )
                    })
                    .child(
                        Button::new("onboarding-next")
                            .primary()
                            .loading(self.checking)
                            .label(if last { "Get started" } else { "Continue" })
                            .on_click(cx.listener(|this, _, window, cx| this.next(window, cx))),
                    ),
            );

        v_flex()
            .id("onboarding")
            .size_full()
            .items_center()
            .px_6()
            // Anchored at the top so the stepper stays put while steps change height.
            .map(|this| if short { this.pt_6().pb_6() } else { this.pt_20().pb_10() })
            .bg(cx.theme().muted.opacity(0.4))
            .child(card)
    }
}
