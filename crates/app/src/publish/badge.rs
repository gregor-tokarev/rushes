//! The sidebar's account widget: who uploads, and whether that works right now.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, Entity, FontWeight, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Subscription, Window, div,
};

use super::service::{Connection, YoutubeService};
use super::ui::{avatar, display_name};
use crate::route::Route;
use crate::state::AppState;

/// Compact account widget for the sidebar footer: avatar + channel, or "Connect YouTube".
/// Clicking it opens the YouTube settings.
pub struct AccountBadge {
    service: Entity<YoutubeService>,
    _subscription: Subscription,
}

impl AccountBadge {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let service = YoutubeService::global(cx);
        let subscription = cx.observe(&service, |_, _, cx| cx.notify());
        Self { service, _subscription: subscription }
    }

    /// The badge's two lines: who (or what to do), then the state.
    fn lines(&self, cx: &Context<Self>) -> (String, Option<String>) {
        let service = self.service.read(cx);
        match service.connection() {
            Connection::Connected(account) => {
                let uploading = service.jobs().iter().filter_map(|job| service.transfer(job.id)).collect::<Vec<_>>();
                let detail = match uploading.as_slice() {
                    [] => account.channel_handle.clone().unwrap_or_else(|| account.email.clone()),
                    [transfer] => format!("Uploading · {}%", (transfer.fraction() * 100.0).floor()),
                    several => format!("Uploading {} videos", several.len()),
                };
                (display_name(account), Some(detail))
            }
            Connection::Connecting { .. } => ("Connecting YouTube…".to_owned(), Some("Finish in your browser".to_owned())),
            Connection::Failed(_) => ("Reconnect YouTube".to_owned(), Some("Sign-in needed".to_owned())),
            Connection::Disconnected => ("Connect YouTube".to_owned(), None),
        }
    }

    fn leading(&self, cx: &Context<Self>) -> AnyElement {
        let service = self.service.read(cx);
        let theme = cx.theme();
        let slot = || div().size_8().flex_shrink_0().flex().items_center().justify_center();
        match service.connection() {
            Connection::Connected(account) => div()
                .relative()
                .flex_shrink_0()
                .child(avatar(account, service.avatar().map(ToOwned::to_owned)))
                .child(
                    div()
                        .absolute()
                        .bottom_0()
                        .right_0()
                        .size_2p5()
                        .rounded_full()
                        .border_2()
                        .border_color(theme.sidebar)
                        .bg(theme.success),
                )
                .into_any_element(),
            Connection::Connecting { .. } => {
                slot().child(Spinner::new().small().color(theme.muted_foreground)).into_any_element()
            }
            Connection::Failed(_) => {
                slot().child(Icon::new(IconName::CircleAlert).text_color(theme.danger)).into_any_element()
            }
            Connection::Disconnected => {
                slot().child(Icon::new(IconName::CirclePlay).text_color(theme.muted_foreground)).into_any_element()
            }
        }
    }
}

impl Render for AccountBadge {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (title, detail) = self.lines(cx);
        let label: SharedString = match &detail {
            Some(detail) => format!("{title}, {detail}. Open YouTube settings").into(),
            None => format!("{title}. Open YouTube settings").into(),
        };
        let muted = cx.theme().muted_foreground;
        Button::new("account-badge")
            .ghost()
            .w_full()
            .h_auto()
            .px_2()
            .py_1p5()
            .accessibility_label(label)
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .child(self.leading(cx))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().font_weight(FontWeight::MEDIUM).truncate().child(title))
                            .when_some(detail, |this, detail| {
                                this.child(div().text_xs().text_color(muted).truncate().child(detail))
                            }),
                    ),
            )
            .on_click(|_, _, cx| {
                let state = AppState::global(cx);
                state.update(cx, |state, cx| state.navigate(Route::Settings, cx));
            })
    }
}
