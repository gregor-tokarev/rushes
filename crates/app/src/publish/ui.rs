//! Presentation pieces shared by the publish views: field chrome, the account
//! summary and the connect controls.

use std::path::{Path, PathBuf};

use gpui_kit::assets::IconName;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::avatar::Avatar;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::link::Link;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, Size, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClipboardItem, Div, ElementId, Entity, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, RenderOnce, SharedString, Styled as _, TestSupportExt as _, Window, div, relative,
};
use videocrm_core::{TagColor, UploadStatus};
use videocrm_youtube::Account;

use super::rules::group_thousands;
use super::service::{Connection, YoutubeService};
use crate::database::Chip;
use crate::database::colors::upload_status_color;
use crate::route::SettingsSection;
use crate::state::AppState;
use crate::ui::{Card, SectionHeader};

/// Heading of a group of fields.
pub(super) fn section_heading(title: &'static str) -> SectionHeader {
    SectionHeader::new(title)
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

/// A titled card of the side column.
pub(super) fn card(id: impl Into<ElementId>, title: &'static str) -> Card {
    Card::new().id(id).title(title)
}

/// "Open on YouTube ↗": the video's page on YouTube, a link everywhere it is
/// offered. `id` names the frame tests find it by.
pub(super) fn youtube_link(id: impl Into<ElementId>, url: String) -> AnyElement {
    let id = id.into();
    div()
        .id(id.clone())
        .text_sm()
        .child(
            Link::new(ElementId::Name(format!("{id}-link").into()))
                .href(url)
                .child(h_flex().gap_1().child("Open on YouTube").child(Icon::new(IconName::ExternalLink).xsmall())),
        )
        // Links are not observable on their own; tests find the link through this frame.
        .test_support()
        .into_any_element()
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
pub(super) fn upload_label(status: &UploadStatus, progress: Option<f32>, resumable: bool) -> String {
    match (status, progress) {
        (UploadStatus::Failed { .. }, _) if resumable => "Stopped".to_owned(),
        (UploadStatus::Uploading { .. }, Some(fraction)) => format!("Uploading {}%", (fraction * 100.0).floor()),
        (UploadStatus::Uploading { sent, total }, None) if *total > 0 => {
            format!("Uploading {}%", sent.saturating_mul(100) / total)
        }
        (status, _) => status.label().to_owned(),
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
    tag: Option<SharedString>,
}

impl AccountSummary {
    pub(super) fn new(account: Account, avatar: Option<PathBuf>) -> Self {
        Self { account, avatar, compact: false, tag: None }
    }

    /// A short state beside the name, such as "Default".
    pub(super) fn tag(mut self, tag: Option<impl Into<SharedString>>) -> Self {
        self.tag = tag.map(Into::into);
        self
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
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap_2()
                            .child(div().min_w_0().font_weight(FontWeight::MEDIUM).truncate().child(title))
                            .when_some(self.tag, |this, tag| {
                                this.child(Tag::secondary().small().flex_shrink_0().child(tag))
                            }),
                    )
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

/// Where an account stands and what can be done about it: connect, follow the
/// browser sign-in (with the link to copy when no browser opened), sign in again,
/// or try again. Its buttons are scoped by `id`, so several panels can share a window.
#[derive(IntoElement)]
pub(super) struct ConnectionPanel {
    id: ElementId,
    service: Entity<YoutubeService>,
    /// Which account, by key; the default account for `None`.
    account: Option<SharedString>,
    /// In settings: leave fixing the settings to the form below.
    manage: bool,
}

impl ConnectionPanel {
    pub(super) fn new(id: impl Into<ElementId>, service: Entity<YoutubeService>) -> Self {
        Self { id: id.into(), service, account: None, manage: false }
    }

    pub(super) fn account(mut self, key: Option<impl Into<SharedString>>) -> Self {
        self.account = key.map(Into::into);
        self
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
        // One browser sign-in at a time: another one must end first.
        let busy = service.is_signing_in();
        let connect = {
            let service = self.service.clone();
            Button::new("connect").label("Connect").small().disabled(busy).on_click(move |_, _, cx| {
                service.update(cx, |service, cx| service.connect(cx));
            })
        };
        let open_settings = Button::new("settings").label("Open settings").small().outline().on_click(|_, _, cx| {
            let state = AppState::global(cx);
            state.update(cx, |state, cx| state.open_settings(SettingsSection::Youtube, cx));
        });

        let body = match service.connection_for(self.account.as_deref()) {
            // Settings has room for every detail; the Publish side column does not.
            Connection::Connected(account) => div().child(
                AccountSummary::new(
                    account.clone(),
                    service.find_account(account.key()).and_then(|known| known.avatar()).map(ToOwned::to_owned),
                )
                .compact(!self.manage),
            ),
            Connection::Connecting { authorize_url } => sign_in_progress(&self.service, authorize_url, cx),
            Connection::NeedsSignIn(account) => {
                let action = match (&config_problem, self.manage) {
                    (Some(_), true) => Some(reconnect_button(&self.service, &account).disabled(true)),
                    (Some(_), false) => None,
                    (None, _) => Some(reconnect_button(&self.service, &account).disabled(busy)),
                };
                v_flex().gap_3().child(needs_sign_in_line(&account, service, cx)).child(h_flex().map(
                    |this| match action {
                        Some(button) => this.child(button),
                        None => this.child(open_settings),
                    },
                ))
            }
            Connection::Failed(message) => {
                let action = match (&config_problem, self.manage) {
                    (Some(_), true) => None,
                    (Some(_), false) => Some(open_settings),
                    (None, _) => Some(connect.label("Try again")),
                };
                v_flex()
                    .gap_3()
                    .child(status_line(IconName::CircleAlert, theme.danger, "Couldn’t connect", Some(message), cx))
                    .children(action.map(|button| h_flex().child(button)))
            }
            Connection::Disconnected => {
                let action = match (&config_problem, self.manage) {
                    // The settings form right below is where the problem gets fixed. The
                    // button waits there, so the form doesn't jump down once it is usable.
                    (Some(_), true) => Some(connect.primary().disabled(true)),
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

/// "Sign in again · Reconnect to keep uploading as Demo Creator (@creator)."
pub(super) fn needs_sign_in_line(account: &Account, service: &YoutubeService, cx: &App) -> Div {
    let who = match &account.channel_handle {
        Some(handle) => format!("{} ({handle})", display_name(account)),
        None => display_name(account),
    };
    let mut detail = format!("Reconnect to keep uploading as {who}.");
    if service.config().is_ok_and(|config| config.is_google()) {
        detail.push_str(" Google ends the sign-ins of apps in Testing after 7 days.");
    }
    status_line(IconName::TriangleAlert, cx.theme().warning, "Sign in again", Some(detail.into()), cx)
}

/// "Reconnect" for an account whose sign-in YouTube no longer accepts.
pub(super) fn reconnect_button(service: &Entity<YoutubeService>, account: &Account) -> Button {
    let service = service.clone();
    let key = account.key().to_owned();
    Button::new("reconnect")
        .label("Reconnect")
        .small()
        .primary()
        .on_click(move |_, _, cx| service.update(cx, |service, cx| service.reconnect(&key, cx)))
}

/// The browser sign-in under way: what to do in the browser, the link to copy
/// once there is one, and Cancel.
pub(super) fn sign_in_progress(service: &Entity<YoutubeService>, authorize_url: Option<SharedString>, cx: &App) -> Div {
    let theme = cx.theme();
    let cancel = {
        let service = service.clone();
        Button::new("cancel")
            .label("Cancel")
            .small()
            .outline()
            .on_click(move |_, _, cx| service.update(cx, |service, cx| service.cancel(cx)))
    };
    let google = service.read(cx).config().is_ok_and(|config| config.is_google());
    // Before the browser opens, the sign-in is checked: emulators up, client known.
    let (title, help) = match &authorize_url {
        None => ("Getting ready to sign in…", None),
        Some(_) => (
            "Finish signing in with your browser",
            Some(if google {
                "If no browser opened, copy the sign-in link and open it in a browser on this computer. \
                 If Google shows “Error 401: invalid_client”, the client ID or secret is wrong: cancel \
                 and copy them again."
            } else {
                "If no browser opened, copy the sign-in link and open it in a browser on this computer."
            }),
        ),
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
                        .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(title))
                        .when_some(help, |this, help| this.child(help_text(help, cx))),
                ),
        )
        .child(
            h_flex()
                .gap_2()
                .when_some(authorize_url, |this, url| {
                    this.child(
                        Button::new("copy-link").label("Copy link").icon(IconName::Copy).small().outline().on_click(
                            move |_, window, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(url.to_string()));
                                let copied = Notification::success(
                                    "Open it in a browser on this computer to finish signing in.",
                                )
                                .title("Sign-in link copied");
                                window.push_notification(copied, cx);
                            },
                        ),
                    )
                })
                .child(cancel),
        )
}

/// Asks before signing the account `key` out, saying what happens to its uploads
/// and to the videos that upload to the default account.
pub(super) fn confirm_disconnect(service: Entity<YoutubeService>, key: String, window: &mut Window, cx: &mut App) {
    let youtube = service.read(cx);
    let Some(account) = youtube.find_account(&key) else { return };
    let name = display_name(account.account());
    let uploading =
        youtube.jobs().iter().any(|job| youtube.is_active(job.id) && job.channel.as_deref() == Some(key.as_str()));
    let successor = (youtube.default_account().map(|default| default.key()) == Some(key.as_str()))
        .then(|| youtube.accounts().iter().find(|other| other.key() != key))
        .flatten()
        .map(|other| display_name(other.account()));
    let mut consequence = "VideoCRM forgets the sign-in and Google revokes it.".to_owned();
    if uploading {
        consequence.push_str(" Its uploads in progress stop; you can resume them after connecting it again.");
    }
    match successor {
        Some(successor) => {
            consequence.push_str(&format!(" Videos that haven’t chosen an account upload to “{successor}” instead."))
        }
        None if !uploading => consequence.push_str(" Connect it again to upload as it."),
        None => {}
    }
    window.open_alert_dialog(cx, move |dialog, _, _| {
        let (service, key) = (service.clone(), key.clone());
        dialog
            .title(format!("Disconnect “{name}”?"))
            .description(consequence.clone())
            .confirm()
            .ok_text("Disconnect")
            .ok_variant(ButtonVariant::Danger)
            .on_ok(move |_, _, cx| {
                service.update(cx, |service, cx| service.disconnect(&key, cx));
                true
            })
    });
}

#[cfg(test)]
mod tests {
    use super::*;

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
