//! AI edit pipelines (video-use through a coding agent). Owned by the ai-edit workstream.

use gpui_kit::*;
use videocrm_core::VideoId;

use crate::state::AppState;

/// App-wide pipeline runner: starts phases on background threads, persists runs
/// and events, survives navigation, stops agents on quit.
pub struct PipelineService {
    state: Entity<AppState>,
}

struct GlobalPipelineService(Entity<PipelineService>);

impl Global for GlobalPipelineService {}

impl PipelineService {
    /// Creates the service and registers it globally. Call once at startup, after
    /// `AppState::set_global`.
    pub fn init(state: Entity<AppState>, cx: &mut App) {
        let service = cx.new(|_| PipelineService { state });
        cx.set_global(GlobalPipelineService(service));
    }

    pub fn global(cx: &App) -> Entity<PipelineService> {
        cx.global::<GlobalPipelineService>().0.clone()
    }

    pub fn state(&self) -> &Entity<AppState> {
        &self.state
    }
}

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
