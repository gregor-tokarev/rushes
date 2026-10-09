//! A video's page: icon, title, properties and the Script / Notes / Footage / Publish tabs.
//! Owned by the shell workstream; the Footage tab embeds `ai_edit::AiEditPanel` and the
//! Publish tab embeds `publish::PublishPanel`.

use gpui_kit::*;
use videocrm_core::VideoId;

use crate::route::VideoTab;

pub struct VideoPage {
    video_id: VideoId,
    tab: VideoTab,
}

impl VideoPage {
    pub fn new(video_id: VideoId, tab: VideoTab, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self { video_id, tab }
    }

    pub fn video_id(&self) -> VideoId {
        self.video_id
    }

    pub fn set_tab(&mut self, tab: VideoTab, _window: &mut Window, cx: &mut Context<Self>) {
        self.tab = tab;
        cx.notify();
    }
}

impl Render for VideoPage {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.tab.label())
    }
}
