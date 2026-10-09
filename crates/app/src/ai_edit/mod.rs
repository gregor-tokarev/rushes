//! AI edit pipelines (video-use through a coding agent). Owned by the ai-edit workstream.

mod brief;
mod service;
mod timeline;

use gpui_kit::*;
use videocrm_core::VideoId;

pub use brief::{COLOR_GRADES, color_grade_label, default_brief, with_preferences};
pub use service::{INTERRUPTED, LivePhase, PipelineService, RunError, split_failure};
pub use timeline::{LogEvent, PhaseKind, PhaseStarted, TimelineEntry, TimelineItem, ToolOutput, fold_timeline};

/// The "Footage & AI edit" tab of a video page.
pub struct AiEditPanel {
    video_id: VideoId,
}

impl AiEditPanel {
    pub fn new(video_id: VideoId, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self { video_id }
    }

    pub fn video_id(&self) -> VideoId {
        self.video_id
    }
}

impl Render for AiEditPanel {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().child("Footage & AI edit")
    }
}

/// The Pipelines page: runs across all videos.
pub struct PipelinesPage;

impl PipelinesPage {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self
    }
}

impl Render for PipelinesPage {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().child("Pipelines")
    }
}

/// AI editing section of the Settings page.
pub struct AiSettings;

impl AiSettings {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self
    }
}

impl Render for AiSettings {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().child("AI editing")
    }
}

/// Requirement checklist (agent CLI, video-use skill, ffmpeg, ElevenLabs key, Python)
/// with fix hints. Used by onboarding and settings.
pub struct DoctorChecklist;

impl DoctorChecklist {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self
    }
}

impl Render for DoctorChecklist {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().child("Requirements")
    }
}
