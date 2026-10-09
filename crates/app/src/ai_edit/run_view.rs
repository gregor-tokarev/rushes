//! The selected run of the AI edit panel: progress, the decision it waits for, its
//! output and the agent's activity.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use chrono::Utc;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::stepper::{Stepper, StepperItem};
use gpui_kit::component::text::TextView;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Div, ElementId, Entity, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, ScrollHandle, ScrollWheelEvent, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, px, rems,
};
use videocrm_core::{PipelineRun, RunId, RunStatus};
use videocrm_pipeline::{MediaInfo, probe_media};

use super::format;
use super::icons::tool_icon;
use super::service::{LivePhase, PipelineService, split_failure};
use super::timeline::{LogEvent, PhaseKind, TimelineItem, fold_timeline};
use crate::route::VideoTab;
use crate::state::AppState;

/// Distance from the end within which scrolling down resumes following new events.
const FOLLOW_SLACK: f32 = 48.;

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
    scroll: ScrollHandle,
    /// Keep the newest events in view while the agent works. Scrolling up stops it;
    /// scrolling back to the end resumes it.
    follow: bool,
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
                TextareaState::new(window, cx).auto_grow(2, 8).placeholder("Notes for the render (optional)")
            }),
            feedback: cx.new(|cx| TextareaState::new(window, cx).auto_grow(3, 10)),
            service,
            state: AppState::global(cx),
            run_id: None,
            feedback_open: None,
            expanded: HashSet::new(),
            items: Vec::new(),
            folded_len: 0,
            scroll: ScrollHandle::new(),
            follow: false,
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
        self.scroll = ScrollHandle::new();
        if let Some(id) = run_id {
            self.service.update(cx, |service, cx| service.load_timeline(id, cx));
        }
        self.follow = self.live(cx).is_some();
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
            self.items = fold_timeline(self.service.read(cx).timeline(id));
            self.folded_len = timeline_len;
        }

        if live && self.ticker.is_none() {
            // A phase just started: show what the agent does next.
            self.follow = true;
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
            state.open_video(video_id, VideoTab::Publish, cx);
        });
    }

    /// Wheel input on the timeline: up stops following new events, down to the end resumes.
    fn on_timeline_scroll(&mut self, event: &ScrollWheelEvent, window: &mut Window) {
        if event.delta.pixel_delta(window.line_height()).y > px(0.) {
            self.follow = false;
        } else {
            let max = self.scroll.max_offset().y;
            self.follow = self.scroll.offset().y <= -max + px(FOLLOW_SLACK);
        }
    }

    fn toggle_row(&mut self, seq: u32, cx: &mut Context<Self>) {
        if !self.expanded.remove(&seq) {
            self.expanded.insert(seq);
        }
        cx.notify();
    }

    /// The phase the run is in, or last ran.
    fn last_phase(&self, cx: &App) -> Option<PhaseKind> {
        self.live(cx).map(|live| live.kind).or_else(|| {
            self.service.read(cx).timeline(self.run_id?).iter().rev().find_map(|entry| match &entry.event {
                LogEvent::Phase(started) => Some(started.phase),
                LogEvent::Agent(_) => None,
            })
        })
    }

    fn render_stepper(&self, run: &PipelineRun, cx: &App) -> impl IntoElement {
        let rendering_step = self.last_phase(cx).is_some_and(PhaseKind::renders);
        let step = match run.status {
            RunStatus::Queued | RunStatus::Planning => 0,
            RunStatus::AwaitingApproval => 1,
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
        Stepper::new("run-stepper").small().selected_index(step).items(
            [
                (IconName::Search, "Plan"),
                (IconName::FileText, "Review strategy"),
                (IconName::Film, "Render"),
                (IconName::CircleCheck, "Done"),
            ]
            .map(|(icon, label)| StepperItem::new().icon(icon).child(div().text_sm().child(label))),
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
        let error = run.error.clone().unwrap_or_default();
        let (message, stderr) = split_failure(&error);
        let (message, stderr) = (message.to_owned(), stderr.map(str::to_owned));
        let resumable = run.session_id.is_some();
        let feedback = self.render_feedback(FeedbackPurpose::Continue, window, cx);
        let theme = cx.theme();
        let (icon, title, color) = if cancelled {
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
                                .primary()
                                .small()
                                .icon(IconName::RotateCcw)
                                .label("Retry")
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.act(cx, |service, id, cx| service.retry(id, cx))),
                                ),
                        ),
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

    fn render_timeline(&self, run: &PipelineRun, live: bool, cx: &mut Context<Self>) -> Div {
        let rows: Vec<AnyElement> = self.items.iter().map(|item| self.render_item(run.id, item, cx)).collect();
        let theme = cx.theme();
        let count = self.items.len();
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
            .child(
                div()
                    .id(element_id("timeline", run.id))
                    .max_h(rems(30.))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .on_scroll_wheel(
                        cx.listener(|this, event: &ScrollWheelEvent, window, _| this.on_timeline_scroll(event, window)),
                    )
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .child(
                        v_flex()
                            .py_2()
                            .when(rows.is_empty(), |this| {
                                this.child(
                                    div().px_4().py_3().text_sm().text_color(theme.muted_foreground).child(if live {
                                        "Waiting for the agent…"
                                    } else {
                                        "No activity recorded."
                                    }),
                                )
                            })
                            .children(rows),
                    ),
            )
    }

    fn render_item(&self, run_id: RunId, item: &TimelineItem, cx: &mut Context<Self>) -> AnyElement {
        let seq = item.seq();
        let expanded = self.expanded.contains(&seq);
        let theme = cx.theme();
        let (muted, danger, success, border, muted_bg, radius) =
            (theme.muted_foreground, theme.danger, theme.success, theme.border, theme.muted, theme.radius);
        let mono_font = theme.mono_font_family.clone();
        let row_id = element_id(&format!("timeline-{seq}"), run_id);
        match item {
            TimelineItem::Phase { phase, text, .. } => {
                let label = match phase {
                    PhaseKind::Plan => "Planning started",
                    PhaseKind::Auto => "Edit started, strategy pre-approved",
                    PhaseKind::Revise => "You asked for a revised strategy",
                    PhaseKind::Approve => "You approved the strategy",
                    PhaseKind::Feedback => "You asked for changes",
                };
                v_flex()
                    .id(row_id)
                    .gap_2()
                    .px_4()
                    .pt_3()
                    .pb_1()
                    .child(
                        h_flex()
                            .gap_3()
                            .child(div().flex_1().h(px(1.)).bg(border))
                            .child(div().text_xs().text_color(muted).child(label))
                            .child(div().flex_1().h(px(1.)).bg(border)),
                    )
                    .when_some(text.clone(), |this, text| {
                        this.child(div().mx_8().px_3().py_2().rounded(radius).bg(muted_bg).text_sm().child(text))
                    })
                    .into_any_element()
            }
            TimelineItem::Message { text, .. } => row(row_id, Icon::new(IconName::Sparkles).text_color(muted))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(markdown(element_id(&format!("message-{seq}"), run_id), text.clone())),
                )
                .into_any_element(),
            TimelineItem::Thinking { text, .. } => {
                let content = v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(div().text_sm().text_color(muted).child("Thinking"))
                    .when(expanded, |this| this.child(div().text_sm().text_color(muted).child(text.clone())));
                let base = row(row_id, Icon::new(IconName::Brain).text_color(muted)).child(content);
                expandable(base, seq, expanded, cx).into_any_element()
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
                if item.is_expandable() {
                    expandable(base, seq, expanded, cx).into_any_element()
                } else {
                    base.child(disclosure_lane()).into_any_element()
                }
            }
            TimelineItem::Problem { title, detail, .. } => {
                row(row_id, Icon::new(IconName::ShieldAlert).text_color(danger))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                div().text_sm().font_weight(FontWeight::MEDIUM).text_color(danger).child(title.clone()),
                            )
                            .when(!detail.is_empty(), |this| this.child(div().text_sm().child(detail.clone()))),
                    )
                    .into_any_element()
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
                    .into_any_element()
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
                expandable(base, seq, expanded, cx).into_any_element()
            }
        }
    }
}

impl Render for RunView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(run) = self.run(cx).cloned() else {
            return div();
        };
        let live = self.live(cx);
        if live.is_some() && self.follow {
            // Rows such as Markdown messages settle their height a frame late, so the
            // end is re-applied every frame rather than once per event.
            self.scroll.scroll_to_bottom();
        }
        let stepper = self.render_stepper(&run, cx).into_any_element();
        let state_card = match (live, run.status) {
            (Some(live), _) => Some(self.render_live(live, cx)),
            (None, RunStatus::AwaitingApproval) => Some(self.render_strategy(&run, window, cx)),
            (None, RunStatus::Succeeded) => Some(self.render_output(&run, window, cx)),
            (None, RunStatus::Failed | RunStatus::Cancelled) => Some(self.render_failure(&run, window, cx)),
            (None, _) => None,
        };
        let timeline = self.render_timeline(&run, live.is_some(), cx);
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

/// A timeline row: a fixed icon lane, then the content.
fn row(id: ElementId, icon: Icon) -> gpui_kit::Stateful<Div> {
    h_flex()
        .id(id)
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
