//! AI edit pipelines: video-use driven through a headless coding agent.
//!
//! [`PipelineService`] (a global entity) runs and records edits; the views read it:
//! [`AiEditPanel`] is the "Footage & AI edit" tab of a video, [`PipelinesPage`] lists
//! every run, [`AiSettings`] is the settings section and [`DoctorChecklist`] shows
//! what is missing on this machine. Register [`WithAiEditIcons`] around the
//! application's asset source unless it serves the complete Lucide set.

mod brief;
mod doctor;
mod footage;
mod format;
mod icons;
mod panel;
mod pipelines_page;
mod run_view;
mod service;
mod settings;
mod timeline;

pub use brief::{COLOR_GRADES, color_grade_label, default_brief, preference_edits, with_preferences};
pub use doctor::DoctorChecklist;
pub use icons::{AiEditIcons, WithAiEditIcons};
pub use panel::AiEditPanel;
pub use pipelines_page::PipelinesPage;
pub use service::{INTERRUPTED, LivePhase, PipelineService, RENDERED_WITH_ERROR, RunError, Setback, split_failure};
pub use settings::AiSettings;
pub use timeline::{
    LogEvent, MarkerKind, PhaseKind, PhaseStarted, RunMarker, TimelineEntry, TimelineItem, ToolOutput, fold_timeline,
};

use gpui_kit::App;
use rushes_core::VideoId;

/// A sentence for delete confirmations when an AI edit of one of `video_ids` is
/// running: deleting a video stops its agent. `None` when none is.
pub fn delete_run_notice(video_ids: &[VideoId], cx: &App) -> Option<&'static str> {
    let service = PipelineService::try_global(cx)?;
    let running = video_ids.iter().any(|id| service.read(cx).live_run_of(*id).is_some());
    running.then_some(if video_ids.len() == 1 {
        "Its AI edit is running right now: deleting it stops the agent mid-edit."
    } else {
        "Some of them have an AI edit running right now: deleting them stops those edits mid-way."
    })
}
