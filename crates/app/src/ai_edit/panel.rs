//! [`AiEditPanel`]: the "Footage & AI edit" tab of a video page.

use std::time::Duration;

use chrono::Local;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Div, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, Styled as _, Subscription, Task, TestSupportExt as _, Window, div,
};
use videocrm_core::settings::EditPreferences;
use videocrm_core::{Block, DocumentKind, PipelineRun, RunId, RunStatus, Video, VideoId};

use super::brief::{
    BriefDraft, COLOR_GRADES, apply_edits, brief_source, color_grade_label, default_brief, preference_edits,
    shift_offset,
};
use super::doctor::DoctorChecklist;
use super::footage::{FootageCard, FootageChanged};
use super::format;
use super::run_view::RunView;
use super::service::PipelineService;
use crate::database::Chip;
use crate::database::colors::run_status_color;
use crate::state::{AppEvent, AppState};
use crate::ui::format_when;
use crate::ui::{Card, SectionHeader};

/// How long the brief form waits after a change before it is saved.
const DRAFT_SAVE_DELAY: Duration = Duration::from_millis(500);

/// The "Footage & AI edit" tab of a video page: the raw takes, the edit brief, and
/// the video's AI edits with the selected one in detail.
///
/// The panel has its natural height; the page that embeds it owns scrolling.
pub struct AiEditPanel {
    video_id: VideoId,
    state: Entity<AppState>,
    service: Entity<PipelineService>,
    footage: Entity<FootageCard>,
    brief: Entity<TextareaState>,
    /// The creator changed the brief by hand. Until then it follows the video: title,
    /// hook, script and standing instructions.
    brief_edited: bool,
    /// What the brief was drafted from ([`brief_source`]).
    brief_source: String,
    /// The video changed after the creator edited the brief.
    brief_stale: bool,
    /// The brief as the panel's own last change left it: the change events that
    /// change causes are not the creator typing.
    applied_brief: Option<String>,
    /// Saves the form shortly after it changes.
    save_task: Option<Task<()>>,
    target_length: Entity<InputState>,
    preferences: EditPreferences,
    auto_approve: bool,
    /// The brief form is shown (always while the video has no edits).
    composing: bool,
    run_view: Entity<RunView>,
    doctor: Entity<DoctorChecklist>,
    start_error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl AiEditPanel {
    pub fn new(video_id: VideoId, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = AppState::global(cx);
        let service = PipelineService::global(cx);
        let settings = state.read(cx).settings().pipeline.clone();
        let draft = service.read(cx).draft(video_id).cloned();
        let (preferences, auto_approve) = match &draft {
            Some(draft) => (draft.preferences.clone(), draft.auto_approve),
            None => (settings.default_preferences.clone(), settings.auto_approve),
        };

        let brief =
            cx.new(|cx| TextareaState::new(window, cx).auto_grow(6, 18).placeholder("Describe the edit you want"));
        let target_length = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Natural length").default_value(preferences.target_length.clone())
        });
        let footage = cx.new(|cx| FootageCard::new(video_id, cx));
        let run_view = cx.new(|cx| RunView::new(window, cx));
        let doctor = cx.new(|cx| DoctorChecklist::new(window, cx).compact(true));

        let subscriptions = vec![
            cx.observe_in(&service, window, |this, _, window, cx| this.on_runs_changed(window, cx)),
            cx.observe(&state, |_, _, cx| cx.notify()),
            cx.subscribe_in(&state, window, |this, _, event: &AppEvent, window, cx| {
                if matches!(event, AppEvent::DataChanged | AppEvent::SettingsChanged) {
                    this.follow_video(window, cx);
                }
            }),
            cx.subscribe(&brief, |this, brief, event: &InputEvent, cx| {
                if !matches!(event, InputEvent::Change) {
                    return;
                }
                // `set_value` emits nothing, and the panel's own edits leave the brief as
                // it noted; any other change is the creator typing.
                if this.applied_brief.as_deref() != Some(brief.read(cx).value().as_ref()) {
                    this.applied_brief = None;
                    this.brief_edited = true;
                    this.schedule_save(cx);
                    cx.notify();
                }
            }),
            cx.subscribe(&footage, |_, _, _: &FootageChanged, cx| cx.notify()),
            cx.subscribe_in(&target_length, window, |this, input, event, window, cx| {
                if matches!(event, InputEvent::Change) {
                    let target_length = input.read(cx).value().to_string();
                    this.set_preferences(window, cx, |preferences| preferences.target_length = target_length);
                }
            }),
            cx.on_release(|this, cx| this.save_draft(cx)),
            cx.on_app_quit(|this, cx| {
                this.save_draft(cx);
                async {}
            }),
        ];

        let latest = service
            .update(cx, |service, _| service.take_revealed(video_id))
            .or_else(|| service.read(cx).video_runs(video_id).next().map(|run| run.id));
        let mut panel = Self {
            video_id,
            state,
            service,
            footage,
            brief,
            brief_edited: false,
            brief_source: String::new(),
            brief_stale: false,
            applied_brief: None,
            save_task: None,
            target_length,
            preferences,
            auto_approve,
            composing: latest.is_none(),
            run_view,
            doctor,
            start_error: None,
            _subscriptions: subscriptions,
        };
        match draft {
            // A brief the creator wrote stays; one they did not follows what changed meanwhile.
            Some(draft) if draft.edited => {
                panel.brief.update(cx, |input, cx| input.set_value(draft.brief, window, cx));
                panel.brief_edited = true;
                panel.brief_source = draft.source;
                panel.follow_video(window, cx);
            }
            _ => panel.reset_brief(window, cx),
        }
        panel.run_view.update(cx, |view, cx| view.set_run(latest, window, cx));
        panel
    }

    pub fn video_id(&self) -> VideoId {
        self.video_id
    }

    /// The brief as the form shows it.
    pub fn brief_text(&self, cx: &App) -> String {
        self.brief.read(cx).value().to_string()
    }

    /// The video, its script and the standing instructions a brief is drafted from.
    fn brief_inputs(&self, cx: &App) -> Option<(Video, Vec<Block>, String)> {
        let state = self.state.read(cx);
        let video = state.video(self.video_id).cloned()?;
        let script = state
            .documents(self.video_id)
            .into_iter()
            .find(|document| document.kind == DocumentKind::Script)
            .map(|document| document.blocks)
            .unwrap_or_default();
        Some((video, script, state.settings().pipeline.extra_instructions.clone()))
    }

    /// Replaces the brief with the default for the video, its script and the
    /// preferences; from then on it follows them again.
    fn reset_brief(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((video, script, extra)) = self.brief_inputs(cx) else { return };
        let brief = default_brief(&video, &script, &self.preferences, &extra);
        self.brief.update(cx, |input, cx| input.set_value(brief, window, cx));
        self.brief_source = brief_source(&video, &script, &extra);
        self.brief_edited = false;
        self.brief_stale = false;
        self.schedule_save(cx);
        cx.notify();
    }

    /// The form as it is now.
    fn draft(&self, cx: &App) -> BriefDraft {
        BriefDraft {
            brief: self.brief.read(cx).value().to_string(),
            preferences: self.preferences.clone(),
            auto_approve: self.auto_approve,
            edited: self.brief_edited,
            source: self.brief_source.clone(),
        }
    }

    /// Saves the form once the creator pauses.
    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DRAFT_SAVE_DELAY).await;
            _ = this.update(cx, |this, cx| this.save_draft(cx));
        }));
    }

    fn save_draft(&mut self, cx: &mut App) {
        self.save_task = None;
        let (video_id, draft) = (self.video_id, self.draft(cx));
        self.service.update(cx, |service, cx| service.keep_draft(video_id, draft, cx));
    }

    /// Drafts the brief again when what it was drafted from changed, unless the creator
    /// edited it; then it only notes that it is out of date.
    fn follow_video(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((video, script, extra)) = self.brief_inputs(cx) else { return };
        let source = brief_source(&video, &script, &extra);
        if source == self.brief_source {
            return;
        }
        if self.brief_edited {
            if !self.brief_stale {
                self.brief_stale = true;
                cx.notify();
            }
        } else {
            self.reset_brief(window, cx);
        }
    }

    /// Changes the preferences and the brief's lines for the ones that changed. The
    /// creator's own lines stay, and the change can be undone in the brief like typing.
    fn set_preferences(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut EditPreferences),
    ) {
        let before = self.preferences.clone();
        change(&mut self.preferences);
        let current = self.brief.read(cx).value().to_string();
        let edits = preference_edits(&current, &before, &self.preferences);
        if !edits.is_empty() {
            self.applied_brief = Some(apply_edits(&current, &edits));
            self.brief.update(cx, |input, cx| {
                let selection = input.selected_range();
                // From the end, so the earlier ranges stay where they are.
                for (range, text) in edits.iter().rev() {
                    input.set_selected_range(range.clone(), cx);
                    input.replace(text.clone(), window, cx);
                }
                let (start, end) = (shift_offset(selection.start, &edits), shift_offset(selection.end, &edits));
                input.set_selected_range(start..end, cx);
            });
        }
        self.schedule_save(cx);
        cx.notify();
    }

    fn runs<'a>(&self, cx: &'a App) -> Vec<&'a PipelineRun> {
        self.service.read(cx).video_runs(self.video_id).collect()
    }

    /// Keeps a run selected while the video has any.
    fn on_runs_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.service.update(cx, |service, _| service.take_revealed(self.video_id)) {
            self.select_run(id, window, cx);
            return;
        }
        let selected = self.run_view.read(cx).run_id();
        if !selected.is_some_and(|id| self.service.read(cx).run(id).is_some()) {
            let latest = self.service.read(cx).video_runs(self.video_id).next().map(|run| run.id);
            self.run_view.update(cx, |view, cx| view.set_run(latest, window, cx));
        }
        cx.notify();
    }

    fn select_run(&mut self, run: RunId, window: &mut Window, cx: &mut Context<Self>) {
        self.composing = false;
        self.run_view.update(cx, |view, cx| view.set_run(Some(run), window, cx));
        cx.notify();
    }

    /// Why the edit cannot start now, if it cannot.
    fn start_blocker(&self, cx: &App) -> Option<&'static str> {
        let footage = self.footage.read(cx);
        let service = self.service.read(cx);
        if !footage.is_loaded() {
            Some("Reading the footage folder…")
        } else if footage.is_importing() {
            Some("Copying footage…")
        } else if !footage.has_takes() {
            Some("Add footage to start an edit.")
        } else if service.live_run_of(self.video_id).is_some() {
            Some("An edit of this video is running.")
        } else {
            match service.doctor() {
                None => Some("Checking requirements…"),
                Some(doctor) if !doctor.ready() => Some("Fix the missing requirements first."),
                Some(_) => None,
            }
        }
    }

    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let brief = self.brief.read(cx).value().trim().to_string();
        let (video_id, auto_approve) = (self.video_id, self.auto_approve);
        let started = self.service.update(cx, |service, cx| service.start_run(video_id, brief, auto_approve, cx));
        match started {
            Ok(id) => {
                self.start_error = None;
                self.select_run(id, window, cx);
            }
            Err(error) => {
                self.start_error = Some(error.to_string().into());
                cx.notify();
            }
        }
    }

    fn render_section_header(&self, cx: &mut Context<Self>) -> SectionHeader {
        let runs = self.runs(cx);
        let selected = self.run_view.read(cx).run_id();
        let now = Local::now();
        let history = selected.and_then(|id| runs.iter().find(|run| run.id == id)).map(|run| {
            let label = format!("{} · {}", format_when(run.created_at, now), run.status.label());
            let entries: Vec<HistoryEntry> = runs
                .iter()
                .map(|run| HistoryEntry {
                    id: run.id,
                    status: run.status,
                    started: format_when(run.created_at, now),
                    cost: run.cost_usd.map(format::cost),
                    selected: Some(run.id) == selected,
                })
                .collect();
            let panel = cx.weak_entity();
            Button::new("ai-edit-history")
                .ghost()
                .small()
                .icon(IconName::Clock)
                .label(label)
                .dropdown_caret(true)
                .dropdown_menu(move |mut menu, _, _| {
                    menu = menu.label("Edits of this video");
                    for entry in &entries {
                        let (id, panel) = (entry.id, panel.clone());
                        let item = entry.clone();
                        menu = menu.item(
                            PopupMenuItem::element(move |_, cx| item.render(cx)).checked(entry.selected).on_click(
                                move |_, window, cx| {
                                    _ = panel.update(cx, |panel, cx| panel.select_run(id, window, cx));
                                },
                            ),
                        );
                    }
                    menu
                })
        });
        let new_edit = (!runs.is_empty() && !self.composing).then(|| {
            Button::new("ai-edit-new")
                .outline()
                .small()
                .icon(IconName::Plus)
                .label("New edit")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.composing = true;
                    this.start_error = None;
                    cx.notify();
                }))
                .into_any_element()
        });
        SectionHeader::new("AI edit")
            .description("video-use cuts the takes: it proposes a strategy, you approve, it renders.")
            .actions(history.map(IntoElement::into_any_element).into_iter().chain(new_edit))
    }

    fn render_composer(&self, cx: &mut Context<Self>) -> Card {
        let blocker = self.start_blocker(cx);
        let doctor_missing = self.service.read(cx).doctor().is_some_and(|doctor| !doctor.ready());
        let has_runs = !self.runs(cx).is_empty();
        let grade = self.preferences.color_grade.clone();
        let theme = cx.theme();

        let grade_button = {
            let panel = cx.weak_entity();
            let current = grade.clone();
            Button::new("brief-grade")
                .ghost()
                .small()
                .icon(IconName::Palette)
                .label(format!("Grade: {}", color_grade_label(&grade)))
                .dropdown_caret(true)
                .dropdown_menu(move |mut menu, _, _| {
                    for (value, label) in COLOR_GRADES {
                        let panel = panel.clone();
                        menu = menu.item(PopupMenuItem::new(label).checked(current == value).on_click(
                            move |_, window, cx| {
                                _ = panel.update(cx, |panel, cx| {
                                    panel.set_preferences(window, cx, |preferences| {
                                        preferences.color_grade = value.to_owned()
                                    })
                                });
                            },
                        ));
                    }
                    menu
                })
        };

        Card::new()
            .gap_4()
            .title("Edit brief")
            .description("What the agent should make of the footage. Drafted from the video and its script.")
            .action(
                Button::new("brief-reset")
                    .ghost()
                    .small()
                    .icon(IconName::RotateCcw)
                    .label("Reset to default")
                    .on_click(cx.listener(|this, _, window, cx| this.reset_brief(window, cx))),
            )
            .when(self.brief_stale, |this| {
                this.child(
                    h_flex()
                        .id("brief-stale")
                        .test_support()
                        .gap_2()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(Icon::new(IconName::Info).small())
                        .child(
                            "The video or its script changed since you edited this brief. Reset it to draft it again.",
                        ),
                )
            })
            .child(div().id("brief").test_support().child(Textarea::new(&self.brief)))
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_x_4()
                    .gap_y_2()
                    .child(
                        Checkbox::new("brief-remove-fillers")
                            .label("Remove fillers")
                            .checked(self.preferences.remove_fillers)
                            .on_change(cx.listener(|this, checked: &bool, window, cx| {
                                let checked = *checked;
                                this.set_preferences(window, cx, |preferences| preferences.remove_fillers = checked)
                            })),
                    )
                    .child(
                        Checkbox::new("brief-subtitles")
                            .label("Burn subtitles")
                            .checked(self.preferences.subtitles)
                            .on_change(cx.listener(|this, checked: &bool, window, cx| {
                                let checked = *checked;
                                this.set_preferences(window, cx, |preferences| preferences.subtitles = checked)
                            })),
                    )
                    .child(grade_button)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().text_sm().text_color(theme.muted_foreground).child("Target length"))
                            .child(div().w_40().child(Input::new(&self.target_length).small())),
                    ),
            )
            .when(doctor_missing, |this| {
                this.child(div().pt_4().border_t_1().border_color(theme.border).child(self.doctor.clone()))
            })
            .child(
                h_flex()
                    .gap_3()
                    .pt_4()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        Switch::new("brief-auto-approve")
                            .label("Skip strategy review")
                            .tooltip("Plan and render in one pass, without waiting for your approval")
                            .checked(self.auto_approve)
                            .on_change(cx.listener(|this, checked: &bool, _, cx| {
                                this.auto_approve = *checked;
                                this.schedule_save(cx);
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1())
                    .when_some(blocker, |this, reason| {
                        this.child(div().text_sm().text_color(theme.muted_foreground).child(reason))
                    })
                    .when(has_runs, |this| {
                        this.child(Button::new("ai-edit-cancel-new").ghost().small().label("Cancel").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.composing = false;
                                this.start_error = None;
                                cx.notify();
                            }),
                        ))
                    })
                    .child(
                        Button::new("ai-edit-start")
                            .primary()
                            .small()
                            .icon(IconName::WandSparkles)
                            .label("Start AI edit")
                            .disabled(blocker.is_some())
                            .on_click(cx.listener(|this, _, window, cx| this.start(window, cx))),
                    ),
            )
            .when_some(self.start_error.clone(), |this, error| {
                this.child(div().text_sm().text_color(theme.danger).child(error))
            })
    }
}

/// One run in the history menu.
#[derive(Clone)]
struct HistoryEntry {
    id: RunId,
    status: RunStatus,
    started: String,
    cost: Option<String>,
    selected: bool,
}

impl HistoryEntry {
    fn render(&self, cx: &App) -> Div {
        let muted = cx.theme().muted_foreground;
        h_flex()
            .w_full()
            .gap_3()
            .text_sm()
            .child(
                h_flex()
                    .w_32()
                    .flex_shrink_0()
                    .child(Chip::new(self.status.label(), run_status_color(self.status)).dot(true)),
            )
            .child(div().flex_1().child(self.started.clone()))
            .child(div().w_12().text_right().text_color(muted).children(self.cost.clone()))
    }
}

impl Render for AiEditPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self.render_section_header(cx);
        let composer = self.composing.then(|| self.render_composer(cx));
        let selected = self.run_view.read(cx).run_id().and_then(|id| self.service.read(cx).run(id).cloned());

        v_flex().w_full().gap_10().child(self.footage.clone()).child(
            v_flex().gap_4().child(header).children(composer).when_some(selected, |this, run| {
                let color = run_status_color(run.status);
                this.child(
                    v_flex()
                        .gap_4()
                        .when(self.composing, |this| {
                            this.child(
                                h_flex()
                                    .gap_2()
                                    .text_sm()
                                    .child(div().font_weight(FontWeight::MEDIUM).child("Latest edit"))
                                    .child(Chip::new(run.status.label(), color).dot(true)),
                            )
                        })
                        .child(self.run_view.clone()),
                )
            }),
        )
    }
}
