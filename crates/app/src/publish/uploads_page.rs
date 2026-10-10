//! The Uploads page: every upload job, newest first, with what can be done about each.

use chrono::Local;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, Div, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Rems,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div,
};
use videocrm_core::{UploadJob, UploadStatus};

use super::service::YoutubeService;
use super::transfer::{format_bytes, format_eta, format_speed};
use super::ui::{file_name, upload_chip, upload_label, youtube_link};
use super::upload::ProcessingOutcome;
use crate::route::{Route, VideoTab};
use crate::state::AppState;
use crate::ui::{
    CollectionTable, EmptyState, PAGE_INSET, PageHeader, TableColumn, TableRowButton, format_when, page_frame,
    video_icon,
};

/// The Uploads page: every upload job, with resume for interrupted ones.
pub struct UploadsPage {
    state: Entity<AppState>,
    service: Entity<YoutubeService>,
    _subscriptions: Vec<Subscription>,
}

/// What a row offers besides opening its video.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowAction {
    Cancel,
    Resume,
    Open,
}

impl UploadsPage {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = AppState::global(cx);
        let service = YoutubeService::global(cx);
        let subscriptions =
            vec![cx.observe(&service, |_, _, cx| cx.notify()), cx.observe(&state, |_, _, cx| cx.notify())];
        Self { state, service, _subscriptions: subscriptions }
    }

    fn summary(&self, cx: &App) -> String {
        let service = self.service.read(cx);
        let jobs = service.jobs();
        let mut parts = vec![match jobs.len() {
            1 => "1 upload".to_owned(),
            count => format!("{count} uploads"),
        }];
        let active = service.active_upload_count();
        if active > 0 {
            parts.push(format!("{active} uploading"));
        }
        let resumable = jobs.iter().filter(|job| service.is_resumable(job)).count();
        if resumable > 0 {
            parts.push(format!("{resumable} can be resumed"));
        }
        parts.join(" · ")
    }

    fn row_action(&self, job: &UploadJob, cx: &App) -> Option<RowAction> {
        let service = self.service.read(cx);
        if service.is_active(job.id) {
            Some(RowAction::Cancel)
        } else if service.is_resumable(job) {
            Some(RowAction::Resume)
        } else {
            watch_url(job).map(|_| RowAction::Open)
        }
    }

    fn run(&mut self, action: RowAction, job: &UploadJob, window: &mut Window, cx: &mut Context<Self>) {
        let id = job.id;
        match action {
            RowAction::Cancel => self.service.update(cx, |service, cx| service.cancel_upload(id, cx)),
            RowAction::Resume => {
                if let Err(error) = self.service.update(cx, |service, cx| service.resume_upload(id, cx)) {
                    window.push_notification(
                        Notification::error(error.to_string()).title("Couldn't resume the upload"),
                        cx,
                    );
                }
            }
            RowAction::Open => {
                if let Some(url) = watch_url(job) {
                    cx.open_url(&url);
                }
            }
        }
    }

    fn render_row(&self, job: &UploadJob, cx: &mut Context<Self>) -> TableRowButton {
        let service = self.service.read(cx);
        let theme = cx.theme();
        let video = self.state.read(cx).video(job.video_id);
        let title = video.map_or_else(|| "Deleted video".to_owned(), |video| display_title(video.upload_title()));
        let icon = video.and_then(|video| video.icon.clone());
        let transfer = service.transfer(job.id);
        // While a retry runs, what YouTube confirmed rather than what was in flight.
        let progress = service.progress(job.id);
        let cancelling = service.is_cancelling(job.id);
        let detail: Option<SharedString> = match (&job.status, transfer) {
            _ if cancelling => Some("Stopping…".into()),
            _ if let Some(retry) = service.retry(job.id) => {
                Some(format!("{}. {}", retry.headline(), retry.detail()).into())
            }
            (_, Some(transfer)) if transfer.total() > 0 => {
                // Most telling first: the cell truncates, and the tooltip has the rest.
                let mut parts =
                    vec![format!("{} of {}", format_bytes(transfer.sent()), format_bytes(transfer.total()))];
                parts.extend(transfer.eta().map(format_eta));
                parts.extend(transfer.bytes_per_second().map(format_speed));
                Some(parts.join(" · ").into())
            }
            (UploadStatus::Failed { error }, None) => Some(error.clone().into()),
            (UploadStatus::Processing { .. }, None) => Some("YouTube is processing the video.".into()),
            (UploadStatus::Done { .. }, None) if let Some(problem) = service.thumbnail_problem(job.id) => {
                Some(format!("Thumbnail not set. {problem}").into())
            }
            (UploadStatus::Done { .. }, None) => match service.processing(job.id) {
                Some(ProcessingOutcome::Ready) => Some("Processed and ready to watch.".into()),
                Some(ProcessingOutcome::StillProcessing) => Some("YouTube Studio shows when processing ends.".into()),
                _ => None,
            },
            _ => None,
        };
        let id = job.id.to_string();
        let connected = service.is_connected();
        // The row itself opens the video's Publish tab; the trailing lane holds
        // what the job allows besides that.
        let action: Option<AnyElement> = match self.row_action(job, cx) {
            Some(RowAction::Open) => watch_url(job).map(|url| youtube_link(format!("upload-{id}-open"), url)),
            Some(action) => {
                let job = job.clone();
                let button = Button::new(SharedString::from(format!("upload-{id}-{}", action_id(action))))
                    .label(action_label(action))
                    .small()
                    .outline()
                    .on_click(cx.listener(move |this, _, window, cx| this.run(action, &job, window, cx)));
                Some(
                    match action {
                        RowAction::Resume if !connected => button.disabled(true).tooltip("Connect YouTube to resume"),
                        RowAction::Cancel if cancelling => button.disabled(true),
                        _ => button,
                    }
                    .into_any_element(),
                )
            }
            None => None,
        };
        let status_label = upload_label(&job.status, progress, service.is_resumable(job));
        let [video_column, status, started, updated, actions] = COLUMNS;
        let video_id = job.video_id;
        let row = TableRowButton::new(SharedString::from(format!("upload-{id}")), format!("{title}, {status_label}"))
            .cell(
                video_column,
                h_flex().min_w_0().gap_1p5().items_start().child(video_icon(icon.as_deref())).child(
                    v_flex()
                        .min_w_0()
                        .gap_0p5()
                        .child(div().font_weight(FontWeight::MEDIUM).truncate().child(title))
                        .child(
                            div().text_xs().text_color(theme.muted_foreground).truncate().child(file_name(&job.file)),
                        ),
                ),
            )
            .cell(
                status,
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_1()
                    .child(h_flex().child(upload_chip(&job.status, progress, service.is_resumable(job))))
                    .when_some(progress, |this, progress| {
                        this.child(
                            Progress::new(SharedString::from(format!("upload-{id}-progress")))
                                .value(progress * 100.0)
                                .xsmall()
                                .accessibility_label("Upload progress"),
                        )
                    })
                    .when_some(detail, |this, detail| {
                        this.child(
                            div()
                                .id(SharedString::from(format!("upload-{id}-detail")))
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .truncate()
                                .tooltip({
                                    let detail = detail.clone();
                                    move |window, cx| Tooltip::new(detail.clone()).build(window, cx)
                                })
                                .child(detail),
                        )
                    }),
            )
            .cell(started, time_text(format_when(job.created_at, Local::now()), cx))
            .cell(updated, time_text(format_when(job.updated_at, Local::now()), cx))
            .cell(actions, h_flex().children(action));
        if video.is_none() {
            return row;
        }
        row.on_open(move |_, cx| crate::video_page::open_tab_at_top(video_id, VideoTab::Publish, cx))
    }

    fn render_empty(&self) -> EmptyState {
        let state = self.state.clone();
        EmptyState::new(
            "uploads-empty",
            IconName::CloudUpload,
            "No uploads yet",
            "Upload a video from its Publish tab. Its progress shows up here.",
        )
        .action(
            Button::new("uploads-go-to-videos")
                .label("Go to videos")
                .small()
                .outline()
                .on_click(move |_, _, cx| state.update(cx, |state, cx| state.navigate(Route::default(), cx))),
        )
    }
}

/// Video, status, started, updated and the job's action; a narrow page drops
/// the update time.
const COLUMNS: [TableColumn; 5] = [
    TableColumn::fill("Video"),
    TableColumn::fixed("Status", Rems(10.5)),
    TableColumn::fixed("Started", Rems(9.)),
    TableColumn::fixed("Updated", Rems(9.)).optional(),
    TableColumn::fixed("", Rems(9.)).end(),
];

impl Render for UploadsPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let jobs: Vec<UploadJob> = self.service.read(cx).jobs().to_vec();
        let header = PageHeader::new("Uploads").subtitle(if jobs.is_empty() {
            "Videos you send to YouTube, newest first.".to_owned()
        } else {
            self.summary(cx)
        });
        let content = if jobs.is_empty() {
            self.render_empty().into_any_element()
        } else {
            let rows: Vec<TableRowButton> = jobs.iter().map(|job| self.render_row(job, cx)).collect();
            div()
                .id("uploads-list")
                .size_full()
                .overflow_y_scroll()
                .child(div().px(PAGE_INSET).pb_8().child(CollectionTable::new("uploads-table", &COLUMNS).rows(rows)))
                .into_any_element()
        };
        page_frame(header, content).id("uploads-page")
    }
}

fn time_text(text: String, cx: &App) -> Div {
    div().text_color(cx.theme().muted_foreground).truncate().child(text)
}

fn action_id(action: RowAction) -> &'static str {
    match action {
        RowAction::Cancel => "cancel",
        RowAction::Resume => "resume",
        RowAction::Open => "open",
    }
}

fn action_label(action: RowAction) -> &'static str {
    match action {
        RowAction::Cancel => "Cancel",
        RowAction::Resume => "Resume",
        RowAction::Open => "Open on YouTube",
    }
}

/// Where the job's video is on YouTube, once the bytes arrived.
fn watch_url(job: &UploadJob) -> Option<String> {
    match &job.status {
        UploadStatus::Processing { video_id } | UploadStatus::Done { video_id } => {
            Some(videocrm_youtube::watch_url(video_id))
        }
        _ => None,
    }
}

fn display_title(title: &str) -> String {
    let title = title.trim();
    if title.is_empty() { "Untitled".to_owned() } else { title.to_owned() }
}
