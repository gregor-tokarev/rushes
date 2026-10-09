//! Presentation pieces shared by the publish views: field chrome, the account
//! summary and the connect controls.

use std::path::PathBuf;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::group_box::{GroupBox, GroupBoxVariants as _};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ClipboardItem, Div, ElementId, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce,
    SharedString, Styled as _, Window, div,
};
use videocrm_core::UploadStatus;
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
    control: Option<gpui_kit::AnyElement>,
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
    div().text_xs().line_height(gpui_kit::relative(1.45)).text_color(cx.theme().muted_foreground).child(text.into())
}

/// A titled, outlined card of the side column.
pub(super) fn card(id: impl Into<ElementId>, title: &'static str) -> GroupBox {
    GroupBox::new().id(id).outline().title(div().text_sm().child(title))
}

/// Status of an upload job as the videos table shows it.
pub(super) fn upload_chip(status: &UploadStatus, progress: Option<f32>) -> Chip {
    let label = match (status, progress) {
        (UploadStatus::Uploading { .. }, Some(fraction)) => format!("Uploading {}%", (fraction * 100.0).floor()),
        (UploadStatus::Uploading { sent, total }, None) if *total > 0 => {
            format!("Uploading {}%", sent.saturating_mul(100) / total)
        }
        (status, _) => status.label().to_owned(),
    };
    Chip::new(label, upload_status_color(status)).dot(true)
}

/// Avatar, channel and account of a signed-in YouTube account.
#[derive(IntoElement)]
pub(super) struct AccountSummary {
    account: Account,
    avatar: Option<PathBuf>,
}

impl AccountSummary {
    pub(super) fn new(account: Account, avatar: Option<PathBuf>) -> Self {
        Self { account, avatar }
    }
}

impl RenderOnce for AccountSummary {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let account = self.account;
        let title = display_name(&account);
        let mut details = Vec::new();
        if let Some(handle) = &account.channel_handle {
            details.push(handle.clone());
        }
        if let Some(subscribers) = account.subscriber_count {
            details.push(plural(subscribers, "subscriber"));
        }
        if let Some(videos) = account.video_count {
            details.push(plural(videos, "video"));
        }
        h_flex()
            .gap_3()
            .min_w_0()
            .child(avatar(&account, self.avatar).large())
            .child(
                v_flex()
                    .min_w_0()
                    .gap_0p5()
                    .child(div().font_weight(FontWeight::MEDIUM).truncate().child(title))
                    .child(div().text_sm().text_color(cx.theme().muted_foreground).truncate().child(account.email))
                    .when(!details.is_empty(), |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .truncate()
                                .child(details.join(" · ")),
                        )
                    }),
            )
    }
}

/// The channel title, falling back to the account name and email.
pub(super) fn display_name(account: &Account) -> String {
    account.channel_title.clone().or_else(|| account.name.clone()).unwrap_or_else(|| account.email.clone())
}

pub(super) fn avatar(account: &Account, picture: Option<PathBuf>) -> gpui_kit::component::avatar::Avatar {
    gpui_kit::component::avatar::Avatar::new().name(display_name(account)).when_some(picture, |this, path| this.src(path))
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
    /// Offer "Disconnect" for a connected account (settings) or a link to settings.
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
        let id = self.id;
        let config_problem = service.config().err().cloned();
        let theme = cx.theme();
        let status_row = |icon: IconName, color, title: SharedString, detail: Option<SharedString>| {
            h_flex()
                .gap_3()
                .items_start()
                .child(div().pt_0p5().child(Icon::new(icon).text_color(color)))
                .child(
                    v_flex()
                        .min_w_0()
                        .gap_0p5()
                        .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(title))
                        .when_some(detail, |this, detail| this.child(help_text(detail, cx))),
                )
        };
        let connect = {
            let service = self.service.clone();
            Button::new("connect").label("Connect").small().on_click(move |_, _, cx| {
                service.update(cx, |service, cx| service.connect(cx));
            })
        };
        let open_settings = Button::new("settings").label("Open settings").small().outline().on_click(
            |_, _, cx| {
                let state = AppState::global(cx);
                state.update(cx, |state, cx| state.navigate(Route::Settings, cx));
            },
        );

        let body = match service.connection().clone() {
            Connection::Connected(account) => v_flex()
                .gap_3()
                .child(AccountSummary::new(account, service.avatar().map(ToOwned::to_owned)))
                .when(self.manage, |this| {
                    let service = self.service.clone();
                    this.child(
                        h_flex().child(
                            Button::new("disconnect")
                                .label("Disconnect")
                                .small()
                                .outline()
                                .on_click(move |_, _, cx| service.update(cx, |service, cx| service.disconnect(cx))),
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
                            .gap_3()
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
                                        .on_click(move |_, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(url.to_string()))
                                        }),
                                )
                            })
                            .child(cancel),
                    )
            }
            Connection::Failed(message) => v_flex()
                .gap_3()
                .child(status_row(IconName::CircleAlert, theme.danger, "Couldn't connect".into(), Some(message)))
                .child(h_flex().gap_2().map(|this| match &config_problem {
                    Some(_) => this.child(open_settings),
                    None => this.child(connect.label("Try again")),
                })),
            Connection::Disconnected => v_flex()
                .gap_3()
                .child(status_row(
                    IconName::CircleUser,
                    theme.muted_foreground,
                    "Not connected".into(),
                    Some(config_problem.clone().unwrap_or_else(|| "Connect the channel you upload to.".into())),
                ))
                .child(h_flex().gap_2().map(|this| match &config_problem {
                    Some(_) if !self.manage => this.child(open_settings),
                    Some(_) => this,
                    None => this.child(connect.primary()),
                })),
        };
        body.id(id)
    }
}
