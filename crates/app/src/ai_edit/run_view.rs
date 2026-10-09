//! The selected run of the AI edit panel: progress, the decision it waits for, its
//! output and the agent's activity.

use std::cell::Cell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use chrono::Utc;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::stepper::{Stepper, StepperItem};
use gpui_kit::component::text::TextView;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Div, ElementId, Entity, FollowMode, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, ListAlignment, ListSizingBehavior, ListState, ParentElement as _, Pixels,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task, TestSupportExt as _,
    Window, canvas, div, list, px, rems,
};
use videocrm_core::{PipelineRun, RunId, RunStatus};
use videocrm_pipeline::{MediaInfo, probe_media};

use super::format;
use super::icons::tool_icon;
use super::service::{LivePhase, PipelineService, split_failure};
use super::timeline::{LogEvent, MarkerKind, PhaseKind, TimelineItem, fold_timeline};
use crate::route::VideoTab;
use crate::state::AppState;

/// Height of the activity box once the log outgrows it, in rems.
const TIMELINE_HEIGHT: f32 = 30.;
/// Space above the first and below the last row of the log, in rems.
const TIMELINE_PADDING: f32 = 0.5;

/// What the feedback box is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FeedbackPurpose {
    /// Changes to a proposed strategy.
    Strategy,
    /// Changes to a rendered edit.
    Iterate,
    /// Continue a failed or cancelled edit.
    Continue,
}

/// The final cut's facts, probed off the UI thread.
struct OutputFacts {
    path: PathBuf,
    size: Option<u64>,
    info: Option<MediaInfo>,
}

pub(crate) struct RunView {
    service: Entity<PipelineService>,
    state: Entity<AppState>,
    run_id: Option<RunId>,
    notes: Entity<TextareaState>,
    feedback: Entity<TextareaState>,
    feedback_open: Option<FeedbackPurpose>,
    /// Timeline rows (by log position) whose details are shown.
    expanded: HashSet<u32>,
    items: Vec<TimelineItem>,
    /// Log length `items` was folded from.
    folded_len: usize,
    /// Lays out only the rows in view: a long edit logs thousands of events. While
    /// it follows the tail, the newest rows stay in view until the creator scrolls up;
    /// scrolling back to the end resumes following.
    rows: ListState,
    /// The log's content height, measured after the last layout.
    log_height: Rc<Cell<Option<Pixels>>>,
    output: Option<OutputFacts>,
    output_task: Option<Task<()>>,
    action_error: Option<SharedString>,
    ticker: Option<Task<()>>,
    _subscription: Subscription,
}

impl RunView {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let service = PipelineService::global(cx);
        let subscription = cx.observe(&service, |this, _, cx| this.sync(cx));
        Self {
            notes: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(2, 8)
                    .placeholder("Optional, e.g. keep the pause before the punchline")
            }),
            feedback: cx.new(|cx| TextareaState::new(window, cx).auto_grow(3, 10)),
            service,
            state: AppState::global(cx),
            run_id: None,
            feedback_open: None,
            expanded: HashSet::new(),
            items: Vec::new(),
            folded_len: 0,
            rows: ListState::new(0, ListAlignment::Top, rems(TIMELINE_HEIGHT).to_pixels(window.rem_size())),
            log_height: Rc::default(),
            output: None,
            output_task: None,
            action_error: None,
            ticker: None,
            _subscription: subscription,
        }
    }

    pub(crate) fn run_id(&self) -> Option<RunId> {
        self.run_id
    }

    pub(crate) fn set_run(&mut self, run_id: Option<RunId>, window: &mut Window, cx: &mut Context<Self>) {
        if self.run_id == run_id {
            return;
        }
        self.run_id = run_id;
        self.feedback_open = None;
        self.expanded.clear();
        self.items.clear();
        self.folded_len = 0;
        self.output = None;
        self.output_task = None;
        self.action_error = None;
        self.notes.update(cx, |notes, cx| notes.set_value("", window, cx));
        // A finished run reads from the start; a live one follows its newest events.
        self.rows.reset(0);
        self.rows.set_follow_mode(FollowMode::Normal);
        self.log_height.set(None);
        if let Some(id) = run_id {
            self.service.update(cx, |service, cx| service.load_timeline(id, cx));
        }
        if self.live(cx).is_some() {
            self.rows.set_follow_mode(FollowMode::Tail);
        }
        self.sync(cx);
    }

    fn run<'a>(&self, cx: &'a App) -> Option<&'a PipelineRun> {
        self.run_id.and_then(|id| self.service.read(cx).run(id))
    }

    fn live(&self, cx: &App) -> Option<LivePhase> {
        self.run_id.and_then(|id| self.service.read(cx).live_phase(id))
    }

    /// Follows the service: refolds new events, keeps the clock ticking while the agent
    /// works and probes a new final cut.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.run_id else { return };
        let timeline_len = self.service.read(cx).timeline(id).len();
        let live = self.live(cx).is_some();
        if timeline_len != self.folded_len {
            let items = fold_timeline(self.service.read(cx).timeline(id));
            self.set_items(items);
            self.folded_len = timeline_len;
        }

        if live && self.ticker.is_none() {
            // A phase just started: show what the agent does next.
            self.rows.set_follow_mode(FollowMode::Tail);
            self.ticker = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs(1)).await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        break;
                    }
                }
            }));
        } else if !live {
            self.ticker = None;
        }

        let output = self.run(cx).and_then(|run| run.output.clone()).filter(|_| !live);
        let stale = match (&output, &self.output) {
            (Some(path), Some(facts)) => *path != facts.path,
            (Some(_), None) => self.output_task.is_none(),
            (None, _) => {
                self.output = None;
                false
            }
        };
        if let (Some(path), true) = (output, stale) {
            self.probe_output(path, cx);
        }
        cx.notify();
    }

    /// Takes newly folded rows. Rows usually only get appended; a tool result changes
    /// its call's row, so rows from the first difference on are measured again.
    fn set_items(&mut self, items: Vec<TimelineItem>) {
        let (old, new) = (self.items.len(), items.len());
        let same = self.items.iter().zip(&items).take_while(|(a, b)| a == b).count();
        if new < old {
            self.rows.reset(new);
        } else {
            if same < old {
                self.rows.remeasure_items(same..old);
            }
            self.rows.splice(old..old, new - old);
        }
        self.items = items;
    }

    fn probe_output(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.output_task = Some(cx.spawn(async move |this, cx| {
            let probed = path.clone();
            let (size, info) = cx
                .background_spawn(async move {
                    (std::fs::metadata(&probed).ok().map(|meta| meta.len()), probe_media(&probed))
                })
                .await;
            _ = this.update(cx, |this, cx| {
                this.output = Some(OutputFacts { path, size, info });
                this.output_task = None;
                cx.notify();
            });
        }));
    }

    fn act(
        &mut self,
        cx: &mut Context<Self>,
        action: impl FnOnce(&mut PipelineService, RunId, &mut Context<PipelineService>) -> Result<(), super::RunError>,
    ) {
        let Some(id) = self.run_id else { return };
        let result = self.service.update(cx, |service, cx| action(service, id, cx));
        self.action_error = result.err().map(|error| error.to_string().into());
        cx.notify();
    }

    fn approve(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let notes = self.notes.read(cx).value().to_string();
        self.act(cx, |service, id, cx| service.approve(id, Some(notes), cx));
        if self.action_error.is_none() {
            self.notes.update(cx, |notes, cx| notes.set_value("", window, cx));
        }
    }

    fn open_feedback(&mut self, purpose: FeedbackPurpose, window: &mut Window, cx: &mut Context<Self>) {
        let placeholder = match purpose {
            FeedbackPurpose::Strategy => "What should change in the strategy?",
            FeedbackPurpose::Iterate => "What should change? The agent edits and renders again.",
            FeedbackPurpose::Continue => "How should the agent continue?",
        };
        self.feedback.update(cx, |feedback, cx| {
            feedback.set_placeholder(placeholder, window, cx);
            feedback.focus(window, cx);
        });
        self.feedback_open = Some(purpose);
        cx.notify();
    }

    fn send_feedback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let message = self.feedback.read(cx).value().trim().to_string();
        if message.is_empty() {
            return;
        }
        self.act(cx, |service, id, cx| service.send_feedback(id, message, cx));
        if self.action_error.is_none() {
            self.feedback.update(cx, |feedback, cx| feedback.set_value("", window, cx));
            self.feedback_open = None;
        }
    }

    fn use_for_upload(&mut self, cx: &mut Context<Self>) {
        let Some((video_id, output)) = self.run(cx).and_then(|run| Some((run.video_id, run.output.clone()?))) else {
            return;
        };
        self.state.update(cx, |state, cx| {
            if let Some(mut video) = state.video(video_id).cloned()
                && video.publish.file.as_ref() != Some(&output)
            {
                video.publish.file = Some(output);
                state.update_video(video, cx);
            }
        });
        // The Publish tab opens on its first card, wherever it was scrolled to before.
        crate::video_page::open_tab_at_top(video_id, VideoTab::Publish, cx);
    }

    fn toggle_row(&mut self, seq: u32, cx: &mut Context<Self>) {
        if !self.expanded.remove(&seq) {
            self.expanded.insert(seq);
        }
        // The row grows or shrinks where it is, rather than the log jumping to its end.
        self.rows.pause_following_tail();
        if let Some(ix) = self.items.iter().position(|item| item.seq() == seq) {
            self.rows.remeasure_items(ix..ix + 1);
        }
        cx.notify();
    }

    /// The phase the run is in, or last ran.
    fn last_phase(&self, cx: &App) -> Option<PhaseKind> {
        self.live(cx).map(|live| live.kind).or_else(|| {
            self.service.read(cx).timeline(self.run_id?).iter().rev().find_map(|entry| match &entry.event {
                LogEvent::Phase(started) => Some(started.phase),
                LogEvent::Agent(_) | LogEvent::Marker(_) => None,
            })
        })
    }

    fn render_stepper(&self, run: &PipelineRun, cx: &App) -> impl IntoElement {
        let rendering_step = self.last_phase(cx).is_some_and(PhaseKind::renders);
        // A discarded strategy is still there to review.
        let restorable = run.status == RunStatus::Cancelled && self.service.read(cx).can_restore(run.id);
        let step = match run.status {
            RunStatus::Queued | RunStatus::Planning => 0,
            RunStatus::AwaitingApproval => 1,
            RunStatus::Cancelled if restorable => 1,
            RunStatus::Rendering => 2,
            RunStatus::Succeeded => 4,
            RunStatus::Failed | RunStatus::Cancelled => {
                if rendering_step {
                    2
                } else {
                    0
                }
            }
        };
        // The step a run stopped at says so: its icon and its label, not only a colour.
        let stopped = match run.status {
            RunStatus::Failed => Some((IconName::CircleX, "failed", cx.theme().danger)),
            RunStatus::Cancelled if !restorable => {
                Some((IconName::CircleSlash, "stopped", cx.theme().muted_foreground))
            }
            _ => None,
        };
        let steps = [
            (IconName::Search, "Plan"),
            (IconName::FileText, "Review strategy"),
            (IconName::Film, "Render"),
            (IconName::CircleCheck, "Done"),
        ];
        Stepper::new("run-stepper").small().text_center(true).selected_index(step).items(
            steps.into_iter().enumerate().map(|(ix, (icon, label))| match stopped {
                Some((icon, state, color)) if ix == step => StepperItem::new().icon(Icon::new(icon)).child(
                    div().id("run-stopped-step").test_support().text_color(color).child(format!("{label} {state}")),
                ),
                _ => StepperItem::new().icon(icon).child(label),
            }),
        )
    }

    fn render_live(&self, live: LivePhase, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let label = match (live.cancelling, live.kind) {
            (true, _) => "Stopping the agent…",
            (false, PhaseKind::Plan) => "Planning the edit…",
            (false, PhaseKind::Revise) => "Revising the strategy…",
            (false, PhaseKind::Approve) => "Rendering the edit…",
            (false, PhaseKind::Feedback) => "Applying your feedback…",
            (false, PhaseKind::Auto) => "Planning and rendering…",
        };
        let elapsed = (Utc::now() - live.started_at).to_std().unwrap_or_default();
        h_flex()
            .gap_3()
            .px_4()
            .py_3()
            .rounded(theme.radius_lg)
            .bg(theme.muted)
            .child(Spinner::new().small())
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(label))
            .child(div().flex_1().text_sm().text_color(theme.muted_foreground).child(format::elapsed(elapsed)))
            .child(
                Button::new("run-cancel")
                    .outline()
                    .small()
                    .icon(IconName::CircleStop)
                    .label("Cancel")
                    .disabled(live.cancelling)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(id) = this.run_id {
                            this.service.update(cx, |service, cx| service.cancel(id, cx));
                        }
                    })),
            )
    }

    fn render_strategy(&self, run: &PipelineRun, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let strategy = run.summary.clone().unwrap_or_else(|| "The agent did not describe a strategy.".to_owned());
        let id = run.id;
        let feedback = self.render_feedback(FeedbackPurpose::Strategy, window, cx);
        let theme = cx.theme();
        card(cx)
            .child(card_title(IconName::FileText, "Proposed strategy", "Approve it to render, or ask for changes.", cx))
            .child(markdown(element_id("strategy", id), strategy).into_any_element())
            .child(
                v_flex()
                    .gap_1p5()
                    .child(div().text_xs().text_color(theme.muted_foreground).child("Notes for the render"))
                    .child(Textarea::new(&self.notes).small()),
            )
            .children(feedback)
            .when(self.feedback_open.is_none(), |this| {
                this.child(
                    h_flex()
                        .gap_2()
                        .child(Button::new("run-discard").ghost().small().label("Discard").on_click(cx.listener(
                            move |this, _, _, cx| {
                                this.service.update(cx, |service, cx| service.cancel(id, cx));
                            },
                        )))
                        .child(div().flex_1())
                        .child(
                            Button::new("run-request-changes")
                                .outline()
                                .small()
                                .icon(IconName::MessageSquare)
                                .label("Request changes…")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_feedback(FeedbackPurpose::Strategy, window, cx)
                                })),
                        )
                        .child(
                            Button::new("run-approve")
                                .primary()
                                .small()
                                .icon(IconName::Play)
                                .label("Approve & render")
                                .on_click(cx.listener(|this, _, window, cx| this.approve(window, cx))),
                        ),
                )
            })
    }

    fn render_output(&self, run: &PipelineRun, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let output = run.output.clone().unwrap_or_default();
        let is_upload_file =
            self.state.read(cx).video(run.video_id).is_some_and(|video| video.publish.file.as_ref() == Some(&output));
        let facts = self.output.as_ref().filter(|facts| facts.path == output);
        let meta = match facts {
            None => "Reading the file…".to_owned(),
            Some(facts) => {
                let mut parts = Vec::new();
                if let Some(info) = &facts.info {
                    parts.push(format::media_info(info));
                }
                if let Some(size) = facts.size {
                    parts.push(format::bytes(size));
                }
                if parts.is_empty() { "The file is missing or unreadable.".to_owned() } else { parts.join(" · ") }
            }
        };
        let feedback = self.render_feedback(FeedbackPurpose::Iterate, window, cx);
        let theme = cx.theme();
        let name =
            output.file_name().map_or_else(|| output.display().to_string(), |name| name.to_string_lossy().into_owned());

        card(cx)
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        div()
                            .size_10()
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(theme.radius)
                            .bg(theme.muted)
                            .child(Icon::new(IconName::Film).text_color(theme.muted_foreground)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(name))
                            .child(div().text_xs().text_color(theme.muted_foreground).child(meta)),
                    )
                    .when(is_upload_file, |this| {
                        this.child(
                            h_flex()
                                .gap_1()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(Icon::new(IconName::Check).xsmall())
                                .child("Set as the upload file"),
                        )
                    }),
            )
            .when_some(run.summary.clone(), |this, summary| {
                this.child(markdown(element_id("summary", run.id), summary).into_any_element())
            })
            .children(feedback)
            .when(self.feedback_open.is_none(), |this| {
                let open = output.clone();
                let reveal = output.clone();
                this.child(
                    h_flex()
                        .gap_2()
                        .flex_wrap()
                        .child(
                            Button::new("run-open-video")
                                .outline()
                                .small()
                                .icon(IconName::Play)
                                .label("Open video")
                                .on_click(move |_, _, cx| cx.open_url(&file_url(&open))),
                        )
                        .child(
                            Button::new("run-show-in-folder")
                                .ghost()
                                .small()
                                .icon(IconName::FolderOpen)
                                .label("Show in folder")
                                .on_click(move |_, _, cx| cx.reveal_path(&reveal)),
                        )
                        .child(div().flex_1())
                        .child(
                            Button::new("run-iterate")
                                .outline()
                                .small()
                                .icon(IconName::MessageSquare)
                                .label("Iterate…")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_feedback(FeedbackPurpose::Iterate, window, cx)
                                })),
                        )
                        .child(
                            Button::new("run-use-for-upload")
                                .primary()
                                .small()
                                .icon(IconName::Upload)
                                .label(if is_upload_file { "Go to Publish" } else { "Use for upload" })
                                .on_click(cx.listener(|this, _, _, cx| this.use_for_upload(cx))),
                        ),
                )
            })
    }

    fn render_failure(&self, run: &PipelineRun, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let cancelled = run.status == RunStatus::Cancelled;
        // A discarded strategy is kept; bringing it back costs nothing.
        let restorable = cancelled && self.service.read(cx).can_restore(run.id);
        let error = run.error.clone().unwrap_or_default();
        let (message, stderr) = split_failure(&error);
        let message =
            if restorable { "Restore it to approve or change it, or plan again." } else { message }.to_owned();
        let stderr = stderr.map(str::to_owned);
        let resumable = run.session_id.is_some() && !restorable;
        let feedback = self.render_feedback(FeedbackPurpose::Continue, window, cx);
        let theme = cx.theme();
        let (icon, title, color) = if restorable {
            (IconName::FileText, "Strategy discarded", theme.muted_foreground)
        } else if cancelled {
            (IconName::CircleStop, "Edit cancelled", theme.muted_foreground)
        } else {
            (IconName::CircleX, "Edit failed", theme.danger)
        };

        card(cx)
            .when(!cancelled, |this| this.border_color(theme.danger.opacity(0.4)))
            .child(
                h_flex()
                    .gap_2()
                    .child(Icon::new(icon).small().text_color(color))
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(title)),
            )
            .when(!message.is_empty(), |this| this.child(div().text_sm().child(message)))
            .when_some(stderr, |this, stderr| this.child(code_block(stderr, cx)))
            .children(feedback)
            .when(self.feedback_open.is_none(), |this| {
                this.child(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .when(resumable, |this| {
                            this.child(
                                Button::new("run-continue")
                                    .outline()
                                    .small()
                                    .icon(IconName::MessageSquare)
                                    .label("Continue with feedback…")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_feedback(FeedbackPurpose::Continue, window, cx)
                                    })),
                            )
                        })
                        .child(
                            Button::new("run-retry")
                                .when(restorable, |button| button.outline())
                                .when(!restorable, |button| button.primary())
                                .small()
                                .icon(IconName::RotateCcw)
                                .label(if restorable { "Plan again" } else { "Retry" })
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.act(cx, |service, id, cx| service.retry(id, cx))),
                                ),
                        )
                        .when(restorable, |this| {
                            this.child(
                                Button::new("run-restore")
                                    .primary()
                                    .small()
                                    .icon(IconName::Undo2)
                                    .label("Restore strategy")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.act(cx, |service, id, cx| service.restore(id, cx))
                                    })),
                            )
                        }),
                )
            })
    }

    /// The feedback box, when it is open for `purpose`.
    fn render_feedback(&self, purpose: FeedbackPurpose, _: &mut Window, cx: &mut Context<Self>) -> Option<Div> {
        if self.feedback_open != Some(purpose) {
            return None;
        }
        Some(
            v_flex().gap_2().child(Textarea::new(&self.feedback).small()).child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(Button::new("run-feedback-cancel").ghost().small().label("Cancel").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.feedback_open = None;
                            cx.notify();
                        },
                    )))
                    .child(
                        Button::new("run-send-feedback")
                            .primary()
                            .small()
                            .label("Send feedback")
                            .on_click(cx.listener(|this, _, window, cx| this.send_feedback(window, cx))),
                    ),
            ),
        )
    }

    fn render_timeline(&self, live: bool, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let count = self.items.len();
        let rem_size = window.rem_size();
        let body = if self.items.is_empty() {
            div()
                .px_4()
                .py_5()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(if live { "Waiting for the agent…" } else { "No activity recorded." })
                .into_any_element()
        } else {
            let rows = list(
                self.rows.clone(),
                cx.processor(|this: &mut Self, ix: usize, _, cx| match (this.run_id, this.items.get(ix)) {
                    (Some(run_id), Some(item)) => this.render_item(run_id, item, cx),
                    _ => div().into_any_element(),
                }),
            )
            // The box takes the log's measured height, up to TIMELINE_HEIGHT. The list's
            // own `Infer` sizing measures rows at an unwrapped width, which re-pins a log
            // with a row taller than the box to its end on every frame.
            .with_sizing_behavior(ListSizingBehavior::Auto)
            .w_full()
            .h(self.log_box_height(rem_size))
            .py(rems(TIMELINE_PADDING));
            let (at_start, at_end) = log_edges(&self.rows, rems(TIMELINE_PADDING).to_pixels(rem_size));
            div()
                .id("run-activity")
                .test_support()
                .relative()
                // The page scrolls too: the wheel moves the log until it reaches its start
                // or end, then the page.
                .on_scroll_wheel(move |event, _, cx| {
                    let delta = event.delta.pixel_delta(px(20.)).y;
                    if (delta > px(0.) && !at_start) || (delta < px(0.) && !at_end) {
                        cx.stop_propagation();
                    }
                })
                .child(rows)
                .child(Scrollbar::vertical(&self.rows))
                .child(self.measure_log(rem_size))
                .into_any_element()
        };
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("Activity"))
                    .when(count > 0, |this| {
                        this.child(div().text_sm().text_color(theme.muted_foreground).child(count.to_string()))
                    }),
            )
            .child(div().rounded(theme.radius_lg).border_1().border_color(theme.border).overflow_hidden().child(body))
    }

    /// Height of the activity box: the log's measured content, at most TIMELINE_HEIGHT.
    fn log_box_height(&self, rem_size: Pixels) -> Pixels {
        let max = rems(TIMELINE_HEIGHT).to_pixels(rem_size);
        self.log_height.get().map_or(max, |content| content.min(max))
    }

    /// Measures the log once it is laid out, and lays the view out again when the box
    /// should take another height (rows came in, a row was expanded, another run shows).
    fn measure_log(&self, rem_size: Pixels) -> impl IntoElement {
        let (rows, measured) = (self.rows.clone(), self.log_height.clone());
        let used = self.log_box_height(rem_size);
        let max = rems(TIMELINE_HEIGHT).to_pixels(rem_size);
        let padding = rems(TIMELINE_PADDING).to_pixels(rem_size);
        canvas(
            move |_, window, _| {
                let Some(content) = log_content_height(&rows, padding) else { return };
                measured.set(Some(content));
                if (content.min(max) - used).abs() >= px(0.5) {
                    window.request_animation_frame();
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_0()
    }

    fn render_item(&self, run_id: RunId, item: &TimelineItem, cx: &mut Context<Self>) -> AnyElement {
        let seq = item.seq();
        let expanded = self.expanded.contains(&seq);
        let theme = cx.theme();
        let (muted, danger, success, border, muted_bg, radius) =
            (theme.muted_foreground, theme.danger, theme.success, theme.border, theme.muted, theme.radius);
        let mono_font = theme.mono_font_family.clone();
        let row_id = element_id(&format!("timeline-{seq}"), run_id);
        let element = match item {
            TimelineItem::Phase { phase, text, retry, .. } => {
                let label = match (retry, phase) {
                    // A retry sends the earlier text again; the creator did not write it anew.
                    (true, _) => "Retried",
                    (false, PhaseKind::Plan) => "Planning started",
                    (false, PhaseKind::Auto) => "Edit started, strategy pre-approved",
                    (false, PhaseKind::Revise) => "You asked for a revised strategy",
                    (false, PhaseKind::Approve) => "You approved the strategy",
                    (false, PhaseKind::Feedback) => "You asked for changes",
                };
                let text = text.clone().filter(|_| !retry);
                divider(row_id, label, border, muted).when_some(text, |this, text| {
                    this.child(div().mx_8().px_3().py_2().rounded(radius).bg(muted_bg).text_sm().child(text))
                })
            }
            TimelineItem::Marker { kind, .. } => {
                let label = match kind {
                    MarkerKind::Cancelled => "Cancelled",
                    MarkerKind::Interrupted => super::INTERRUPTED,
                    MarkerKind::Discarded => "You discarded the strategy",
                    MarkerKind::Restored => "You restored the strategy",
                };
                divider(row_id, label, border, muted)
            }
            TimelineItem::Message { text, .. } => {
                // A long message (the strategy, the final summary) shows its start; the
                // state card above has it in full.
                let preview = format::message_preview(text);
                let body = match preview.clone() {
                    Some(preview) if !expanded => div().line_clamp(3).text_ellipsis().child(preview).into_any_element(),
                    _ => markdown(element_id(&format!("message-{seq}"), run_id), text.clone()).into_any_element(),
                };
                let toggle = preview.is_some().then(|| {
                    Button::new(element_id(&format!("message-toggle-{seq}"), run_id))
                        .link()
                        .xsmall()
                        .label(if expanded { "Show less" } else { "Show more" })
                        .on_click(cx.listener(move |this, _, _, cx| this.toggle_row(seq, cx)))
                });
                row(row_id, Icon::new(IconName::Sparkles).text_color(muted)).child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(body)
                        .children(toggle.map(|toggle| h_flex().child(toggle))),
                )
            }
            TimelineItem::Thinking { text, .. } => {
                let content = v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(div().text_sm().text_color(muted).child("Thinking"))
                    .when(expanded, |this| this.child(div().text_sm().text_color(muted).child(text.clone())));
                let base = row(row_id, Icon::new(IconName::Brain).text_color(muted)).child(content);
                expandable(base, seq, expanded, cx)
            }
            TimelineItem::Tool { name, summary, output, .. } => {
                let failed = output.as_ref().is_some_and(|output| output.is_error);
                let mono = name == "Bash";
                let details =
                    expanded.then(|| output.as_ref().map(|output| code_block(output.summary.clone(), cx))).flatten();
                let base = row(row_id, Icon::new(tool_icon(name)).text_color(if failed { danger } else { muted }))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .text_sm()
                                    .child(div().flex_shrink_0().font_weight(FontWeight::MEDIUM).child(name.clone()))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .text_color(muted)
                                            .when(mono, |this| this.font_family(mono_font.clone()).text_xs())
                                            .child(summary.clone()),
                                    )
                                    .when(failed, |this| {
                                        this.child(div().flex_shrink_0().text_xs().text_color(danger).child("failed"))
                                    }),
                            )
                            .children(details),
                    );
                if item.is_expandable() { expandable(base, seq, expanded, cx) } else { base.child(disclosure_lane()) }
            }
            TimelineItem::Problem { title, detail, .. } => {
                row(row_id, Icon::new(IconName::ShieldAlert).text_color(danger)).child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(danger).child(title.clone()))
                        .when(!detail.is_empty(), |this| this.child(div().text_sm().child(detail.clone()))),
                )
            }
            TimelineItem::Finished { cost_usd, duration_ms, .. } => {
                let mut facts = vec!["Agent finished".to_owned()];
                if let Some(ms) = duration_ms {
                    facts.push(format::elapsed(Duration::from_millis(*ms)));
                }
                if let Some(cost) = cost_usd {
                    facts.push(format::cost(*cost));
                }
                row(row_id, Icon::new(IconName::CircleCheck).text_color(success))
                    .child(div().text_sm().text_color(muted).child(facts.join(" · ")))
            }
            TimelineItem::Background { lines, .. } => {
                let label = if lines.len() == 1 {
                    "1 background event".to_owned()
                } else {
                    format!("{} background events", lines.len())
                };
                let details = expanded.then(|| code_block(lines.join("\n"), cx));
                let base = row(row_id, Icon::new(IconName::Layers).text_color(muted)).child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(div().text_sm().text_color(muted).child(label))
                        .children(details),
                );
                expandable(base, seq, expanded, cx)
            }
        };
        element.test_support().into_any_element()
    }
}

impl Render for RunView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(run) = self.run(cx).cloned() else {
            return div();
        };
        let live = self.live(cx);
        let stepper = self.render_stepper(&run, cx).into_any_element();
        let state_card = match (live, run.status) {
            (Some(live), _) => Some(self.render_live(live, cx)),
            (None, RunStatus::AwaitingApproval) => Some(self.render_strategy(&run, window, cx)),
            (None, RunStatus::Succeeded) => Some(self.render_output(&run, window, cx)),
            (None, RunStatus::Failed | RunStatus::Cancelled) => Some(self.render_failure(&run, window, cx)),
            (None, _) => None,
        };
        let timeline = self.render_timeline(live.is_some(), window, cx);
        let error = self.action_error.clone();
        let danger = cx.theme().danger;

        v_flex()
            .gap_4()
            .child(stepper)
            .children(state_card)
            .when_some(error, |this, error| this.child(div().text_sm().text_color(danger).child(error)))
            .child(timeline)
    }
}

/// The height of the log's rows plus its padding, after its last layout; `None` until
/// the last row has been laid out.
fn log_content_height(rows: &ListState, padding: Pixels) -> Option<Pixels> {
    let viewport = rows.viewport_bounds();
    let overflow = rows.max_offset_for_scrollbar().y;
    let items = if overflow > px(0.) {
        viewport.size.height + overflow
    } else {
        let last = rows.bounds_for_item(rows.item_count().checked_sub(1)?)?;
        last.bottom() - viewport.top() - rows.scroll_px_offset_for_scrollbar().y
    };
    Some(items + padding * 2.)
}

/// Whether the log is scrolled to its start and to its end, after its last layout. A
/// log that fits its box is at both.
fn log_edges(rows: &ListState, padding: Pixels) -> (bool, bool) {
    let scrolled = -rows.scroll_px_offset_for_scrollbar().y;
    let end = log_content_height(rows, padding)
        .map_or(px(0.), |content| (content - rows.viewport_bounds().size.height).max(px(0.)));
    (scrolled <= px(0.), scrolled >= end - px(1.))
}

/// A bordered section of the run view.
fn card(cx: &App) -> Div {
    let theme = cx.theme();
    v_flex().gap_4().p_4().rounded(theme.radius_lg).border_1().border_color(theme.border)
}

fn card_title(icon: IconName, title: &'static str, description: &'static str, cx: &App) -> Div {
    let theme = cx.theme();
    h_flex().gap_3().child(Icon::new(icon).text_color(theme.muted_foreground)).child(
        v_flex()
            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(title))
            .child(div().text_xs().text_color(theme.muted_foreground).child(description)),
    )
}

/// A timeline row that marks a turn of the run: a label between two rules.
fn divider(id: ElementId, label: &'static str, border: Hsla, muted: Hsla) -> gpui_kit::Stateful<Div> {
    v_flex().id(id).w_full().gap_2().px_4().pt_3().pb_1().child(
        h_flex()
            .gap_3()
            .child(div().flex_1().h(px(1.)).bg(border))
            .child(div().text_xs().text_color(muted).child(label))
            .child(div().flex_1().h(px(1.)).bg(border)),
    )
}

/// A timeline row: a fixed icon lane, then the content.
fn row(id: ElementId, icon: Icon) -> gpui_kit::Stateful<Div> {
    h_flex()
        .id(id)
        .w_full()
        .items_start()
        .gap_3()
        .px_4()
        .py_1p5()
        .child(div().w_4().flex_shrink_0().pt_0p5().child(icon.small()))
}

/// Makes a row toggle its details on click, with a disclosure chevron.
fn expandable(
    row: gpui_kit::Stateful<Div>,
    seq: u32,
    expanded: bool,
    cx: &mut Context<RunView>,
) -> gpui_kit::Stateful<Div> {
    let hover = cx.theme().muted.opacity(0.6);
    let muted = cx.theme().muted_foreground;
    row.hover(move |style| style.bg(hover))
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_row(seq, cx)))
        .child(disclosure_lane().child(
            Icon::new(if expanded { IconName::ChevronDown } else { IconName::ChevronRight }).xsmall().text_color(muted),
        ))
}

/// The trailing lane of timeline rows, where expandable rows show their chevron.
fn disclosure_lane() -> Div {
    div().w_4().flex_shrink_0().pt_0p5()
}

fn code_block(text: impl Into<SharedString>, cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .px_3()
        .py_2()
        .rounded(theme.radius)
        .bg(theme.muted)
        .font_family(theme.mono_font_family.clone())
        .text_xs()
        .text_color(theme.foreground)
        .whitespace_normal()
        .child(text.into())
}

fn markdown(id: ElementId, text: impl Into<SharedString>) -> TextView {
    TextView::markdown(id, text).selectable(true)
}

fn element_id(part: &str, run: RunId) -> ElementId {
    ElementId::Name(format!("{part}-{run}").into())
}

/// A `file://` URL for `path`, percent-encoding everything but unreserved characters.
fn file_url(path: &std::path::Path) -> String {
    let mut url = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => url.push(byte as char),
            byte => url.push_str(&format!("%{byte:02X}")),
        }
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_urls_are_percent_encoded() {
        assert_eq!(
            file_url(std::path::Path::new("/home/me/Videos/Desk setup (2026)/final.mp4")),
            "file:///home/me/Videos/Desk%20setup%20%282026%29/final.mp4"
        );
        assert_eq!(file_url(std::path::Path::new("/tmp/café.mp4")), "file:///tmp/caf%C3%A9.mp4");
    }
}
