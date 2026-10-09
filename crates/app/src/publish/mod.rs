//! YouTube: account connection, publish form, uploads. Owned by the publish workstream.
//!
//! [`YoutubeService`] is the one owner of the connection and of upload jobs; the
//! views read it and call its methods. Blocking network work runs on worker
//! threads (sign-in, uploads) or the background executor (short API calls).

mod chapters;
mod icons;
mod panel;
mod rules;
mod service;
mod transfer;
mod ui;
mod upload;

use gpui_kit::*;

pub use chapters::{CHAPTERS_HEADING, Chapter, chapters, chapters_text, insert_chapters};
pub use icons::{PublishIcons, WithPublishIcons};
pub use panel::PublishPanel;
pub use rules::{BlockingReason, CATEGORIES, Category, blocking_reasons, tags_length};
pub use service::{Connection, YoutubeEvent, YoutubeService, backend_config};
pub use transfer::Transfer;
pub use upload::{INTERRUPTED, INTERRUPTED_RESUMABLE, ProcessingOutcome, UploadError};

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
