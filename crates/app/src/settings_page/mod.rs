//! Settings page. Owned by the shell workstream; embeds `publish::YoutubeSettings` and
//! `ai_edit::AiSettings` as sections.

use gpui_kit::*;

pub struct SettingsPage;

impl SettingsPage {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self
    }
}

impl Render for SettingsPage {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child("Settings")
    }
}
