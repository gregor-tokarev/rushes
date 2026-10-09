//! Presentation pieces shared by the publish views: field chrome, the account
//! summary and the connect controls.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike as _, Local, Utc};
use gpui_kit::assets::IconName;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::avatar::Avatar;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::group_box::{GroupBox, GroupBoxVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, Size, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClipboardItem, Div, ElementId, Entity, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, RenderOnce, SharedString, Styled as _, Window, div, relative,
};
use videocrm_core::{TagColor, UploadStatus};
use videocrm_youtube::Account;

use super::rules::group_thousands;
use super::service::{Connection, YoutubeService};
use crate::database::Chip;
use crate::database::colors::upload_status_color;
use crate::route::Route;
use crate::state::AppState;

/// Heading of a group of fields.
pub(super) fn section_heading(title: &'static str, cx: &App) -> Div {
    div().text_base().font_weight(FontWeight::SEMIBOLD).text_color(cx.theme().foreground).child(title)
}

/// A labelled control: label (with an optional `used/limit` counter), the control,
/// then help text.
#[derive(IntoElement)]
pub(super) struct FieldRow {
    label: SharedString,
    counter: Option<(usize, usize)>,
    help: Option<SharedString>,
    control: Option<AnyElement>,
}

impl FieldRow {
    pub(super) fn new(label: impl Into<SharedString>) -> Self {
        Self { label: label.into(), counter: None, help: None, control: None }
    }

    /// Shows `used/limit`, in the danger color once over the limit.
    pub(super) fn counter(mut self, used: usize, limit: usize) -> Self {
        self.counter = Some((used, limit));
        self
    }

    pub(super) fn help(mut self, help: impl Into<SharedString>) -> Self {
        self.help = Some(help.into());
        self
    }

    pub(super) fn control(mut self, control: impl IntoElement) -> Self {
        self.control = Some(control.into_any_element());
        self
    }
}

impl RenderOnce for FieldRow {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .gap_1p5()
            .child(
                h_flex()
                    .justify_between()
                    .gap_2()
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(self.label))
                    .when_some(self.counter, |this, (used, limit)| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(if used > limit { theme.danger } else { theme.muted_foreground })
                                .child(format!("{}/{}", group_thousands(used), group_thousands(limit))),
                        )
                    }),
            )
            .children(self.control)
            .when_some(self.help, |this, help| this.child(help_text(help, cx)))
    }
}

/// Small muted text under a control.
pub(super) fn help_text(text: impl Into<SharedString>, cx: &App) -> Div {
    div().text_xs().line_height(relative(1.45)).text_color(cx.theme().muted_foreground).child(text.into())
}

/// An icon beside a short title and an optional explanation.
pub(super) fn status_line(
    icon: IconName,
    color: Hsla,
    title: impl Into<SharedString>,
    detail: Option<SharedString>,
    cx: &App,
) -> Div {
    h_flex().gap_2().items_start().child(div().pt_0p5().child(Icon::new(icon).small().text_color(color))).child(
        v_flex()
            .min_w_0()
            .gap_0p5()
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(title.into()))
            .when_some(detail, |this, detail| this.child(help_text(detail, cx))),
    )
}

/// A titled, outlined card of the side column.
pub(super) fn card(id: impl Into<ElementId>, title: &'static str) -> GroupBox {
    GroupBox::new().id(id).outline().title(div().text_sm().child(title))
}

/// Status of an upload job as the videos table shows it, except that a failure
/// the job can resume from reads as a stop that needs attention, not a dead end.
pub(super) fn upload_chip(status: &UploadStatus, progress: Option<f32>, resumable: bool) -> Chip {
    let color = match status {
        UploadStatus::Failed { .. } if resumable => TagColor::Orange,
        status => upload_status_color(status),
    };
    Chip::new(upload_label(status, progress, resumable), color).dot(true)
}

/// `Uploading 42%` from the live progress when there is one, else the stored status.
fn upload_label(status: &UploadStatus, progress: Option<f32>, resumable: bool) -> String {
    match (status, progress) {
        (UploadStatus::Failed { .. }, _) if resumable => "Stopped".to_owned(),
        (UploadStatus::Uploading { .. }, Some(fraction)) => format!("Uploading {}%", (fraction * 100.0).floor()),
        (UploadStatus::Uploading { sent, total }, None) if *total > 0 => {
            format!("Uploading {}%", sent.saturating_mul(100) / total)
        }
        (status, _) => status.label().to_owned(),
    }
}

/// `Today, 14:02`, `Yesterday, 09:10`, `Oct 7, 14:02`, or `Oct 7, 2025` in another year.
pub(super) fn format_when(at: DateTime<Utc>, now: DateTime<Local>) -> String {
    let at = at.with_timezone(&Local);
    let days = (now.date_naive() - at.date_naive()).num_days();
    match days {
        0 => format!("Today, {}", at.format("%H:%M")),
        1 => format!("Yesterday, {}", at.format("%H:%M")),
        _ if at.year() == now.year() => at.format("%b %-d, %H:%M").to_string(),
        _ => at.format("%b %-d, %Y").to_string(),
    }
}

pub(super) fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned())
}

/// Avatar, channel and account of a signed-in YouTube account.
#[derive(IntoElement)]
pub(super) struct AccountSummary {
    account: Account,
    avatar: Option<PathBuf>,
    compact: bool,
}

impl AccountSummary {
    pub(super) fn new(account: Account, avatar: Option<PathBuf>) -> Self {
        Self { account, avatar, compact: false }
    }

    /// A smaller avatar and no video count, for narrow columns.
    pub(super) fn compact(mut self, compact: bool) -> Self {
        self.compact = compact;
        self
    }
}

impl RenderOnce for AccountSummary {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let account = self.account;
        let title = display_name(&account);
        let mut details = Vec::new();
        if let Some(handle) = &account.channel_handle {
            details.push(handle.clone());
        }
        if let Some(subscribers) = account.subscriber_count {
            details.push(plural(subscribers, "subscriber"));
        }
        if let Some(videos) = account.video_count.filter(|_| !self.compact) {
            details.push(plural(videos, "video"));
        }
        if account.channel_id.is_none() {
            details.push("No YouTube channel yet".to_owned());
        }
        h_flex()
            .gap_3()
            .min_w_0()
            .child(avatar(&account, self.avatar).with_size(if self.compact {
                Size::Size(window.rem_size() * 2.5)
            } else {
                Size::Medium
            }))
            .child(
                v_flex()
                    .min_w_0()
                    .gap_0p5()
                    .child(div().font_weight(FontWeight::MEDIUM).truncate().child(title))
                    .child(div().text_sm().text_color(cx.theme().muted_foreground).truncate().child(account.email))
                    .when(!details.is_empty(), |this| this.child(help_text(details.join(" · "), cx))),
            )
    }
}

/// The channel title, falling back to the account name and email.
pub(super) fn display_name(account: &Account) -> String {
    account.channel_title.clone().or_else(|| account.name.clone()).unwrap_or_else(|| account.email.clone())
}

/// The channel picture when it has been downloaded, otherwise initials.
pub(super) fn avatar(account: &Account, picture: Option<PathBuf>) -> Avatar {
    Avatar::new().name(display_name(account)).when_some(picture, |this, path| this.src(path))
}

fn plural(count: u64, noun: &str) -> String {
    let count_text = group_thousands(usize::try_from(count).unwrap_or(usize::MAX));
    if count == 1 { format!("{count_text} {noun}") } else { format!("{count_text} {noun}s") }
}

/// Where the connection stands and what can be done about it: connect, follow the
/// browser sign-in (with the link to copy when no browser opened), or try again.
/// Its buttons are scoped by `id`, so several panels can share a window.
#[derive(IntoElement)]
pub(super) struct ConnectionPanel {
    id: ElementId,
    service: Entity<YoutubeService>,
    /// In settings: offer "Disconnect", and leave fixing the settings to the form below.
    manage: bool,
}

impl ConnectionPanel {
    pub(super) fn new(id: impl Into<ElementId>, service: Entity<YoutubeService>) -> Self {
        Self { id: id.into(), service, manage: false }
    }

    pub(super) fn manage(mut self, manage: bool) -> Self {
        self.manage = manage;
        self
    }
}

impl RenderOnce for ConnectionPanel {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let service = self.service.read(cx);
        let config_problem = service.config().err().cloned();
        let theme = cx.theme();
        let connect = {
            let service = self.service.clone();
            Button::new("connect").label("Connect").small().on_click(move |_, _, cx| {
                service.update(cx, |service, cx| service.connect(cx));
            })
        };
        let open_settings = Button::new("settings").label("Open settings").small().outline().on_click(|_, _, cx| {
            let state = AppState::global(cx);
            state.update(cx, |state, cx| state.navigate(Route::Settings, cx));
        });

        let body = match service.connection().clone() {
            Connection::Connected(account) => h_flex()
                .gap_3()
                .justify_between()
                .child(div().flex_1().min_w_0().child(
                    // Settings has room for every detail; the Publish side column does not.
                    AccountSummary::new(account.clone(), service.avatar().map(ToOwned::to_owned)).compact(!self.manage),
                ))
                .when(self.manage, |this| {
                    let service = self.service.clone();
                    let name = display_name(&account);
                    this.child(
                        Button::new("disconnect").label("Disconnect…").small().outline().on_click(
                            move |_, window, cx| confirm_disconnect(service.clone(), name.clone(), window, cx),
                        ),
                    )
                }),
            Connection::Connecting { authorize_url } => {
                let cancel = {
                    let service = self.service.clone();
                    Button::new("cancel")
                        .label("Cancel")
                        .small()
                        .ghost()
                        .on_click(move |_, _, cx| service.update(cx, |service, cx| service.cancel(cx)))
                };
                v_flex()
                    .gap_3()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_start()
                            .child(div().pt_0p5().child(Spinner::new().small().color(theme.muted_foreground)))
                            .child(
                                v_flex()
                                    .min_w_0()
                                    .gap_0p5()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child("Finish signing in in your browser"),
                                    )
                                    .child(help_text(
                                        "If no browser opened, copy the sign-in link and open it in a browser on this computer.",
                                        cx,
                                    )),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .when_some(authorize_url, |this, url| {
                                this.child(
                                    Button::new("copy-link")
                                        .label("Copy link")
                                        .icon(IconName::Copy)
                                        .small()
                                        .outline()
                                        .on_click(move |_, window, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(url.to_string()));
                                            let copied = Notification::success(
                                                "Open it in a browser on this computer to finish signing in.",
                                            )
                                            .title("Sign-in link copied");
                                            window.push_notification(copied, cx);
                                        }),
                                )
                            })
                            .child(cancel),
                    )
            }
            Connection::Failed(message) => {
                let action = match (&config_problem, self.manage) {
                    (Some(_), true) => None,
                    (Some(_), false) => Some(open_settings),
                    (None, _) => Some(connect.label("Try again")),
                };
                v_flex()
                    .gap_3()
                    .child(status_line(IconName::CircleAlert, theme.danger, "Couldn't connect", Some(message), cx))
                    .children(action.map(|button| h_flex().child(button)))
            }
            Connection::Disconnected => {
                let action = match (&config_problem, self.manage) {
                    // The settings form right below is where the problem gets fixed.
                    (Some(_), true) => None,
                    (Some(_), false) => Some(open_settings),
                    (None, _) => Some(connect.primary()),
                };
                v_flex()
                    .gap_3()
                    .child(status_line(
                        IconName::CircleUser,
                        theme.muted_foreground,
                        "Not connected",
                        Some(config_problem.clone().unwrap_or_else(|| "Connect the channel you upload to.".into())),
                        cx,
                    ))
                    .children(action.map(|button| h_flex().child(button)))
            }
        };
        body.id(self.id)
    }
}

fn confirm_disconnect(service: Entity<YoutubeService>, name: String, window: &mut Window, cx: &mut App) {
    let uploading = service.read(cx).active_upload_count() > 0;
    window.open_alert_dialog(cx, move |dialog, _, _| {
        let service = service.clone();
        let consequence = if uploading {
            "VideoCRM forgets the sign-in and Google revokes it. Uploads in progress stop; you can resume them after connecting again."
        } else {
            "VideoCRM forgets the sign-in and Google revokes it. Connect again to upload."
        };
        dialog
            .title(format!("Disconnect “{name}”?"))
            .description(consequence)
            .confirm()
            .ok_text("Disconnect")
            .ok_variant(ButtonVariant::Danger)
            .on_ok(move |_, _, cx| {
                service.update(cx, |service, cx| service.disconnect(cx));
                true
            })
    });
}

#[cfg(test)]
mod tests {
    use chrono::{NaiveDate, TimeZone as _};

    use super::*;

    #[test]
    fn times_read_relative_to_today() {
        let local = |y, m, d, h, min| {
            let naive = NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, min, 0).unwrap();
            Local.from_local_datetime(&naive).earliest().unwrap()
        };
        let now = local(2026, 10, 9, 18, 30);
        let utc = |at: DateTime<Local>| at.with_timezone(&Utc);
        assert_eq!(format_when(utc(local(2026, 10, 9, 14, 2)), now), "Today, 14:02");
        assert_eq!(format_when(utc(local(2026, 10, 8, 9, 10)), now), "Yesterday, 09:10");
        assert_eq!(format_when(utc(local(2026, 10, 1, 7, 5)), now), "Oct 1, 07:05");
        assert_eq!(format_when(utc(local(2025, 12, 31, 23, 0)), now), "Dec 31, 2025");
    }

    #[test]
    fn upload_chips_show_progress() {
        let uploading = UploadStatus::Uploading { sent: 25, total: 100 };
        assert_eq!(upload_label(&uploading, None, false), "Uploading 25%");
        assert_eq!(upload_label(&uploading, Some(0.426), false), "Uploading 42%");
        assert_eq!(upload_label(&UploadStatus::Done { video_id: "a".into() }, None, false), "Uploaded");
        let failed = UploadStatus::Failed { error: "Network error.".into() };
        assert_eq!(upload_label(&failed, None, false), "Failed");
        assert_eq!(upload_label(&failed, None, true), "Stopped");
    }
}
