//! [`PipelinesPage`]: every AI edit across all videos.

use std::time::Duration;

use chrono::{Local, Utc};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    Context, ElementId, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Rems, Render,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div,
};
use videocrm_core::{PipelineRun, RunStatus};

use super::format;
use super::service::PipelineService;
use crate::database::Chip;
use crate::database::colors::run_status_color;
use crate::route::{Route, VideoTab};
use crate::state::AppState;
use crate::ui::{
    EmptyState, PAGE_INSET, PageHeader, TableColumn, TableRowButton, format_when, page_frame, table_header, video_icon,
};

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
        // The tab opens on its content rather than on the page header.
        crate::video_page::open_tab_at_top(video_id, VideoTab::Footage, cx);
    }

    fn render_row(&self, run: &PipelineRun, cx: &mut Context<Self>) -> TableRowButton {
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
        let muted = cx.theme().muted_foreground;
        let page = cx.weak_entity();
        let label = format!("{title}, {}", run.status.label());
        let [video_column, status, started, duration_column, cost] = COLUMNS;
        let opened = run.clone();

        TableRowButton::new(ElementId::Name(format!("pipeline-run-{}", run.id).into()), label)
            .cell(
                video_column,
                h_flex()
                    .min_w_0()
                    .gap_1p5()
                    .child(video_icon(icon.as_deref()))
                    .child(div().min_w_0().truncate().font_weight(FontWeight::MEDIUM).child(title)),
            )
            .cell(status, Chip::new(run.status.label(), run_status_color(run.status)).dot(true))
            .cell(started, div().truncate().text_color(muted).child(format_when(run.created_at, Local::now())))
            .cell(
                duration_column,
                div().text_color(muted).child(if run.status == RunStatus::Queued {
                    "—".to_owned()
                } else {
                    format::elapsed(duration)
                }),
            )
            .cell(cost, div().text_color(muted).child(run.cost_usd.map_or("—".to_owned(), format::cost)))
            .on_open(move |_, cx| {
                _ = page.update(cx, |page, cx| page.open(&opened, cx));
            })
    }

    fn render_empty(&self) -> EmptyState {
        let state = self.state.clone();
        EmptyState::new(
            "pipelines-empty",
            IconName::WandSparkles,
            "No AI edits yet",
            "Start one from a video’s Footage & AI edit tab, once its takes are in.",
        )
        .action(
            Button::new("pipelines-go-to-videos")
                .outline()
                .small()
                .label("Go to videos")
                .on_click(move |_, _, cx| state.update(cx, |state, cx| state.navigate(Route::default(), cx))),
        )
    }
}

/// Video, status, started, duration and cost.
const COLUMNS: [TableColumn; 5] = [
    TableColumn::fill("Video"),
    TableColumn::fixed("Status", Rems(9.)),
    TableColumn::fixed("Started", Rems(9.)),
    TableColumn::fixed("Duration", Rems(6.)).end(),
    TableColumn::fixed("Cost", Rems(5.)).end(),
];

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
        let header = PageHeader::new("Pipelines").subtitle(if runs.is_empty() {
            "AI edits across all your videos, newest first.".to_owned()
        } else {
            summary.join(" · ")
        });
        let content = if runs.is_empty() {
            self.render_empty().into_any_element()
        } else {
            let rows: Vec<TableRowButton> = runs.iter().map(|run| self.render_row(run, cx)).collect();
            div()
                .id("pipelines-list")
                .size_full()
                .overflow_y_scroll()
                .child(v_flex().px(PAGE_INSET).pb_8().child(table_header(&COLUMNS, cx)).children(rows))
                .into_any_element()
        };
        page_frame(header, content).id("pipelines-page")
    }
}
