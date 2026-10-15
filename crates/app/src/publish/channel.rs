//! Which YouTube account a video uploads to, as the videos table, the video
//! page's properties and the Publish tab show it and let it be changed.

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::avatar::Avatar;
use gpui_kit::component::menu::PopupMenu;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{App, Div, ParentElement as _, Styled as _, div};
use rushes_core::Video;

use super::service::YoutubeService;
use crate::ui::menu_item;

/// Called with the key of the account chosen in [`ChannelChoices::menu`].
pub type ChooseChannel = Rc<dyn Fn(&str, &mut App)>;

/// The account a video uploads to and the accounts it can choose instead.
pub struct ChannelChoices {
    /// Key of the account it uploads to: its choice, else the default account.
    current: Option<String>,
    /// What to show for it, `None` without any account.
    shown: Option<Shown>,
    /// The signed-in accounts that have a channel, by name.
    accounts: Vec<Choice>,
    /// The video uploads now: the upload keeps its account.
    locked: bool,
}

struct Shown {
    name: String,
    picture: Option<PathBuf>,
    /// Only the default account, not the video's choice; or a choice that
    /// isn't signed in anymore.
    muted: bool,
}

struct Choice {
    key: String,
    /// The name, with the handle or email when another account has the same one.
    label: String,
}

impl ChannelChoices {
    pub fn of(video: &Video, cx: &App) -> Self {
        let Some(service) = YoutubeService::try_global(cx) else {
            return Self { current: None, shown: None, accounts: Vec::new(), locked: false };
        };
        let youtube = service.read(cx);
        let chosen = video.publish.channel.as_ref();
        let current = youtube.channel_for(video);
        let account = current.as_deref().and_then(|key| youtube.find_account(key));
        let shown = match (chosen, account) {
            (_, Some(account)) => Some(Shown {
                name: account.name(),
                picture: account.avatar().map(ToOwned::to_owned),
                muted: chosen.is_none(),
            }),
            // Signed out since: the name it was chosen under.
            (Some(channel), None) => {
                Some(Shown { name: format!("{} (not connected)", channel.name), picture: None, muted: true })
            }
            (None, None) => None,
        };
        let default = youtube.default_account().map(|account| account.key().to_owned());
        let choosable: Vec<_> = youtube.accounts().iter().filter(|account| account.has_channel()).collect();
        let accounts = choosable
            .iter()
            .map(|account| {
                let name = account.name();
                let mut label = if choosable.iter().filter(|other| other.name() == name).count() > 1 {
                    let detail = account.account().channel_handle.as_deref().unwrap_or(&account.account().email);
                    format!("{name} ({detail})")
                } else {
                    name
                };
                if default.as_deref() == Some(account.key()) {
                    label.push_str(" · Default");
                }
                Choice { key: account.key().to_owned(), label }
            })
            .collect();
        Self { current, shown, accounts, locked: youtube.is_uploading_any(&[video.id]) }
    }

    /// Whether another account can be chosen now.
    pub fn is_choosable(&self) -> bool {
        !self.locked && !self.accounts.is_empty()
    }

    /// The account's picture and name; muted while it is only the default.
    /// `None` without any account.
    pub fn label(&self, cx: &App) -> Option<Div> {
        let shown = self.shown.as_ref()?;
        Some(
            h_flex()
                .min_w_0()
                .gap_1p5()
                .child(
                    Avatar::new()
                        .name(shown.name.clone())
                        .xsmall()
                        .when_some(shown.picture.clone(), |this, path| this.src(path)),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .when(shown.muted, |this| this.text_color(cx.theme().muted_foreground))
                        .child(shown.name.clone()),
                ),
        )
    }

    /// One item per account, named so screen readers can tell them apart, the
    /// current one checked and the default one marked; choosing one calls
    /// `choose` with its key.
    pub fn menu(&self, menu: PopupMenu, choose: ChooseChannel) -> PopupMenu {
        self.accounts.iter().fold(menu, |menu, choice| {
            let (key, choose) = (choice.key.clone(), choose.clone());
            menu.item(
                menu_item(choice.label.clone())
                    .checked(self.current.as_deref() == Some(choice.key.as_str()))
                    .on_click(move |_, _, cx| choose(&key, cx)),
            )
        })
    }
}
