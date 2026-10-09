//! The Uploads page: every upload job, newest first, with what can be done about each.

use chrono::Local;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::table::{Table, TableBody, TableCell, TableHead, TableHeader, TableRow};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Rems,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, rems,
};
use videocrm_core::{UploadJob, UploadStatus};

use super::service::YoutubeService;
use super::transfer::{format_bytes, format_eta, format_speed};
use super::ui::{file_name, format_when, help_text, upload_chip};
use super::upload::ProcessingOutcome;
use crate::route::{Route, VideoTab};
use crate::state::AppState;

const STATUS_WIDTH: Rems = Rems(13.);
const TIME_WIDTH: Rems = Rems(8.5);
const ACTIONS_WIDTH: Rems = Rems(10.);

/// The Uploads page: every upload job, with resume for interrupted ones.
pub struct UploadsPage {
    state: Entity<AppState>,
    service: Entity<YoutubeService>,
    _subscriptions: Vec<Subscription>,
}

/// The one action a row offers as a button; the rest live in its menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowAction {
    Cancel,
    Resume,
    Open,
    GoToVideo,
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

    fn primary_action(&self, job: &UploadJob, cx: &App) -> RowAction {
        let service = self.service.read(cx);
        if service.is_active(job.id) {
            RowAction::Cancel
        } else if service.is_resumable(job) {
            RowAction::Resume
        } else if watch_url(job).is_some() {
            RowAction::Open
        } else {
            RowAction::GoToVideo
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
            RowAction::GoToVideo => {
                let video = job.video_id;
                self.state.update(cx, |state, cx| state.open_video(video, VideoTab::Publish, cx));
            }
        }
    }

    fn render_row(&self, job: &UploadJob, cx: &mut Context<Self>) -> TableRow {
        let service = self.service.read(cx);
        let theme = cx.theme();
        let video = self.state.read(cx).video(job.video_id);
        let title = video.map_or_else(|| "Deleted video".to_owned(), |video| display_title(video.upload_title()));
        let transfer = service.transfer(job.id);
        let cancelling = service.is_cancelling(job.id);
        let detail: Option<SharedString> = match (&job.status, transfer) {
            _ if cancelling => Some("Stopping…".into()),
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
            (UploadStatus::Done { .. }, None) => match service.processing(job.id) {
                Some(ProcessingOutcome::Ready) => Some("Processed and ready to watch.".into()),
                Some(ProcessingOutcome::StillProcessing) => Some("YouTube Studio shows when processing ends.".into()),
                _ => None,
            },
            _ => None,
        };
        let id = job.id.to_string();
        let primary = self.primary_action(job, cx);
        let connected = service.is_connected();
        let has_video = video.is_some();
        let url = watch_url(job);
        let action_button = |action: RowAction| {
            let job = job.clone();
            let button = Button::new(SharedString::from(format!("upload-{id}-{}", action_id(action))))
                .label(action_label(action))
                .small()
                .outline()
                .on_click(cx.listener(move |this, _, window, cx| this.run(action, &job, window, cx)));
            match action {
                RowAction::Resume if !connected => button.disabled(true).tooltip("Connect YouTube to resume"),
                RowAction::Cancel if cancelling => button.disabled(true),
                RowAction::GoToVideo if !has_video => button.disabled(true),
                _ => button,
            }
        };
        let page = cx.weak_entity();
        let menu_job = job.clone();
        let menu_actions: Vec<RowAction> = [
            has_video.then_some(RowAction::GoToVideo),
            url.as_ref().map(|_| RowAction::Open),
            service.is_resumable(job).then_some(RowAction::Resume),
            (service.is_active(job.id) && !cancelling).then_some(RowAction::Cancel),
        ]
        .into_iter()
        .flatten()
        .filter(|action| *action != primary)
        .collect();
        let more = Button::new(SharedString::from(format!("upload-{id}-more")))
            .icon(IconName::Ellipsis)
            .small()
            .ghost()
            .accessibility_label("More actions")
            .disabled(menu_actions.is_empty())
            .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |mut menu, _, _| {
                for action in &menu_actions {
                    let (page, job, action) = (page.clone(), menu_job.clone(), *action);
                    menu = menu.item(PopupMenuItem::new(action_label(action)).icon(action_icon(action)).on_click(
                        move |_, window, cx| {
                            page.update(cx, |page, cx| page.run(action, &job, window, cx)).ok();
                        },
                    ));
                }
                menu
            });

        TableRow::new()
            .child(
                TableCell::new().flex_1().min_w(rems(12.)).child(
                    v_flex()
                        .min_w_0()
                        .gap_0p5()
                        .child(div().font_weight(FontWeight::MEDIUM).truncate().child(title))
                        .child(
                            div().text_xs().text_color(theme.muted_foreground).truncate().child(file_name(&job.file)),
                        ),
                ),
            )
            .child(
                TableCell::new().w(STATUS_WIDTH).child(
                    v_flex()
                        .w_full()
                        .min_w_0()
                        .gap_1()
                        .child(h_flex().child(upload_chip(&job.status, transfer.map(|transfer| transfer.fraction()))))
                        .when_some(transfer, |this, transfer| {
                            this.child(
                                Progress::new(SharedString::from(format!("upload-{id}-progress")))
                                    .value(transfer.fraction() * 100.0)
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
                ),
            )
            .child(time_cell(format_when(job.created_at, Local::now()), cx))
            .child(time_cell(format_when(job.updated_at, Local::now()), cx))
            .child(
                TableCell::new()
                    .w(ACTIONS_WIDTH)
                    .child(h_flex().w_full().justify_end().gap_1().child(action_button(primary)).child(more)),
            )
    }

    fn render_empty(&self, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let state = self.state.clone();
        v_flex()
            .id("uploads-empty")
            .w_full()
            .items_center()
            .gap_4()
            .py_16()
            .child(
                div()
                    .size_12()
                    .rounded_full()
                    .bg(theme.muted)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Icon::new(IconName::CloudUpload).text_color(theme.muted_foreground)),
            )
            .child(
                v_flex()
                    .items_center()
                    .gap_1()
                    .child(div().font_weight(FontWeight::MEDIUM).child("No uploads yet"))
                    .child(help_text("Upload a video from its Publish tab. Its progress shows up here.", cx).text_sm()),
            )
            .child(
                Button::new("uploads-go-to-videos")
                    .label("Go to videos")
                    .small()
                    .outline()
                    .on_click(move |_, _, cx| state.update(cx, |state, cx| state.navigate(Route::default(), cx))),
            )
            .into_any_element()
    }
}

impl Render for UploadsPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let jobs: Vec<UploadJob> = self.service.read(cx).jobs().to_vec();
        let theme = cx.theme();
        let header = v_flex().gap_1().child(div().text_2xl().font_weight(FontWeight::SEMIBOLD).child("Uploads")).child(
            div().text_sm().text_color(theme.muted_foreground).child(if jobs.is_empty() {
                "Videos you send to YouTube, newest first.".to_owned()
            } else {
                self.summary(cx)
            }),
        );
        let content = if jobs.is_empty() {
            self.render_empty(cx)
        } else {
            let head = |label: &'static str| TableHead::new().child(label);
            div()
                .w_full()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .overflow_hidden()
                .child(
                    Table::new()
                        .accessibility_label("Uploads")
                        .child(
                            TableHeader::new().child(
                                TableRow::new()
                                    .child(head("Video").flex_1().min_w(rems(12.)))
                                    .child(head("Status").w(STATUS_WIDTH))
                                    .child(head("Started").w(TIME_WIDTH))
                                    .child(head("Updated").w(TIME_WIDTH))
                                    .child(TableHead::new().w(ACTIONS_WIDTH)),
                            ),
                        )
                        .child(TableBody::new().children(jobs.iter().map(|job| self.render_row(job, cx)))),
                )
                .into_any_element()
        };
        div()
            .id("uploads-page")
            .size_full()
            .overflow_y_scroll()
            .child(v_flex().w_full().max_w(rems(72.)).px_10().py_8().gap_6().child(header).child(content))
    }
}

fn time_cell(text: String, cx: &App) -> TableCell {
    TableCell::new().w(TIME_WIDTH).child(div().text_color(cx.theme().muted_foreground).truncate().child(text))
}

fn action_id(action: RowAction) -> &'static str {
    match action {
        RowAction::Cancel => "cancel",
        RowAction::Resume => "resume",
        RowAction::Open => "open",
        RowAction::GoToVideo => "go-to-video",
    }
}

fn action_label(action: RowAction) -> &'static str {
    match action {
        RowAction::Cancel => "Cancel",
        RowAction::Resume => "Resume",
        RowAction::Open => "Open on YouTube",
        RowAction::GoToVideo => "Go to video",
    }
}

fn action_icon(action: RowAction) -> IconName {
    match action {
        RowAction::Cancel => IconName::CircleX,
        RowAction::Resume => IconName::Upload,
        RowAction::Open => IconName::ExternalLink,
        RowAction::GoToVideo => IconName::ArrowRight,
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
