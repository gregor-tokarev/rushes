//! [`PipelinesPage`]: every AI edit across all videos.

use std::time::Duration;

use chrono::Utc;
use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, Div, ElementId, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Role, StatefulInteractiveElement as _, Styled as _, Subscription, Task,
    TestSupportExt as _, Window, div,
};
use videocrm_core::{PipelineRun, RunStatus};

use super::format;
use super::service::PipelineService;
use crate::database::Chip;
use crate::database::colors::run_status_color;
use crate::route::VideoTab;
use crate::state::AppState;

/// The Pipelines page: runs across all videos, newest first. A row opens the
/// video's "Footage & AI edit" tab with that run selected. Owns its scrolling.
pub struct PipelinesPage {
    service: Entity<PipelineService>,
    state: Entity<AppState>,
    /// Redraws once a second while an agent works, for its running time.
    ticker: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl PipelinesPage {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let service = PipelineService::global(cx);
        let state = AppState::global(cx);
        let subscriptions =
            vec![cx.observe(&service, |this, _, cx| this.sync_ticker(cx)), cx.observe(&state, |_, _, cx| cx.notify())];
        let mut page = Self { service, state, ticker: None, _subscriptions: subscriptions };
        page.sync_ticker(cx);
        page
    }

    fn sync_ticker(&mut self, cx: &mut Context<Self>) {
        let live = self.service.read(cx).runs().iter().any(|run| self.service.read(cx).live_phase(run.id).is_some());
        if live && self.ticker.is_none() {
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
        cx.notify();
    }

    fn open(&mut self, run: &PipelineRun, cx: &mut Context<Self>) {
        let (id, video_id) = (run.id, run.video_id);
        self.service.update(cx, |service, cx| service.reveal_run(id, cx));
        self.state.update(cx, |state, cx| state.open_video(video_id, VideoTab::Footage, cx));
    }

    fn render_row(&self, run: &PipelineRun, last: bool, cx: &mut Context<Self>) -> AnyElement {
        let state = self.state.read(cx);
        let video = state.video(run.video_id);
        let title = video.map_or("Deleted video".to_owned(), |video| {
            if video.title.trim().is_empty() { "Untitled".to_owned() } else { video.title.clone() }
        });
        let icon = video.and_then(|video| video.icon.clone());
        let now = Utc::now();
        let live = self.service.read(cx).live_phase(run.id).is_some();
        let end = if live { now } else { run.updated_at };
        let duration = (end - run.created_at).to_std().unwrap_or_default();
        let run = run.clone();
        let theme = cx.theme();
        let hover = theme.muted.opacity(0.6);

        h_flex()
            .id(ElementId::Name(format!("pipeline-run-{}", run.id).into()))
            .role(Role::Button)
            .aria_label(format!("{title}, {}", run.status.label()))
            .test_support()
            .gap_4()
            .px_3()
            .py_2p5()
            .text_sm()
            .rounded(theme.radius)
            .when(!last, |this| this.border_b_1().border_color(theme.border))
            .hover(move |style| style.bg(hover))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .child(match icon {
                        Some(icon) => div().w_5().flex_shrink_0().child(icon),
                        None => div()
                            .w_5()
                            .flex_shrink_0()
                            .child(Icon::new(IconName::Clapperboard).small().text_color(theme.muted_foreground)),
                    })
                    .child(div().min_w_0().truncate().font_weight(FontWeight::MEDIUM).child(title)),
            )
            .child(
                div()
                    .w_32()
                    .flex_shrink_0()
                    .child(Chip::new(run.status.label(), run_status_color(run.status)).dot(true)),
            )
            .child(
                div()
                    .w_40()
                    .flex_shrink_0()
                    .text_color(theme.muted_foreground)
                    .child(format::started(run.created_at, now)),
            )
            .child(
                div()
                    .w_20()
                    .flex_shrink_0()
                    .text_right()
                    .text_color(theme.muted_foreground)
                    .child(if run.status == RunStatus::Queued { "—".to_owned() } else { format::elapsed(duration) }),
            )
            .child(
                div()
                    .w_16()
                    .flex_shrink_0()
                    .text_right()
                    .text_color(theme.muted_foreground)
                    .child(run.cost_usd.map_or("—".to_owned(), format::cost)),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.open(&run, cx)))
            .into_any_element()
    }

    fn render_empty(&self, cx: &App) -> Div {
        let theme = cx.theme();
        v_flex()
            .items_center()
            .gap_2()
            .py_16()
            .child(Icon::new(IconName::WandSparkles).large().text_color(theme.muted_foreground))
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("No AI edits yet"))
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("Start one from a video's Footage & AI edit tab."),
            )
    }
}

impl Render for PipelinesPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let runs: Vec<PipelineRun> = self.service.read(cx).runs().to_vec();
        let working = runs.iter().filter(|run| run.status.is_active()).count();
        let waiting = runs.iter().filter(|run| run.status == RunStatus::AwaitingApproval).count();
        let mut summary = vec![match runs.len() {
            1 => "1 edit".to_owned(),
            count => format!("{count} edits"),
        }];
        if working > 0 {
            summary.push(format!("{working} running"));
        }
        if waiting > 0 {
            summary.push(format!("{waiting} waiting for approval"));
        }
        let rows: Vec<AnyElement> =
            runs.iter().enumerate().map(|(ix, run)| self.render_row(run, ix + 1 == runs.len(), cx)).collect();
        let empty = runs.is_empty().then(|| self.render_empty(cx));
        let theme = cx.theme();
        let column = |label: &'static str| div().flex_shrink_0().child(label);

        div().id("pipelines-page").size_full().overflow_y_scroll().child(
            v_flex()
                .w_full()
                .px_10()
                .py_8()
                .gap_6()
                .child(
                    v_flex().gap_1().child(div().text_2xl().font_weight(FontWeight::BOLD).child("Pipelines")).child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("AI edits across all your videos, newest first."),
                    ),
                )
                .children(empty)
                .when(!runs.is_empty(), |this| {
                    this.child(
                        v_flex()
                            .gap_1()
                            .child(div().text_sm().text_color(theme.muted_foreground).child(summary.join(" · ")))
                            .child(
                                h_flex()
                                    .gap_4()
                                    .px_3()
                                    .py_2()
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.muted_foreground)
                                    .border_b_1()
                                    .border_color(theme.border)
                                    .child(div().flex_1().child("Video"))
                                    .child(column("Status").w_32())
                                    .child(column("Started").w_40())
                                    .child(column("Duration").w_20().text_right())
                                    .child(column("Cost").w_16().text_right()),
                            )
                            .children(rows),
                    )
                }),
        )
    }
}
