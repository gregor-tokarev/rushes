//! YouTube: account connection, publish form, uploads. Owned by the publish workstream.

use gpui_kit::*;
use videocrm_core::VideoId;

use crate::state::AppState;

/// App-wide YouTube connection: settings -> client, OAuth sign-in, credentials, uploads.
pub struct YoutubeService {
    state: Entity<AppState>,
}

struct GlobalYoutubeService(Entity<YoutubeService>);

impl Global for GlobalYoutubeService {}

impl YoutubeService {
    /// Creates the service and registers it globally. Call once at startup, after
    /// `AppState::set_global`.
    pub fn init(state: Entity<AppState>, cx: &mut App) {
        let service = cx.new(|_| YoutubeService { state });
        cx.set_global(GlobalYoutubeService(service));
    }

    pub fn global(cx: &App) -> Entity<YoutubeService> {
        cx.global::<GlobalYoutubeService>().0.clone()
    }

    pub fn state(&self) -> &Entity<AppState> {
        &self.state
    }
}

/// The Publish tab of a video page.
pub struct PublishPanel {
    video_id: VideoId,
}

impl PublishPanel {
    pub fn new(video_id: VideoId, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self { video_id }
    }

    pub fn video_id(&self) -> VideoId {
        self.video_id
    }
}

impl Render for PublishPanel {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().child("Publish")
    }
}

/// The Uploads page: every upload job, with resume for interrupted ones.
pub struct UploadsPage;

impl UploadsPage {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self
    }
}

impl Render for UploadsPage {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().child("Uploads")
    }
}

/// YouTube section used by the Settings page and the onboarding YouTube step:
/// backend choice, OAuth client fields, connect / disconnect.
pub struct YoutubeSettings;

impl YoutubeSettings {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self
    }
}

impl Render for YoutubeSettings {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().child("YouTube")
    }
}

/// Compact account widget for the sidebar footer: avatar + channel, or "Connect YouTube".
pub struct AccountBadge;

impl AccountBadge {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self
    }
}

impl Render for AccountBadge {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().child("Connect YouTube")
    }
}
