//! First-run onboarding. Owned by the shell workstream; embeds `publish::YoutubeSettings`
//! and `ai_edit::DoctorChecklist`.

use gpui_kit::*;

pub struct Onboarding;

impl Onboarding {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self
    }
}

impl Render for Onboarding {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child("Welcome to VideoCRM")
    }
}
