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

pub use brief::{COLOR_GRADES, color_grade_label, default_brief, with_preferences};
pub use doctor::DoctorChecklist;
pub use icons::{AiEditIcons, WithAiEditIcons};
pub use panel::AiEditPanel;
pub use pipelines_page::PipelinesPage;
pub use service::{INTERRUPTED, LivePhase, PipelineService, RunError, split_failure};
pub use settings::AiSettings;
pub use timeline::{
    LogEvent, MarkerKind, PhaseKind, PhaseStarted, RunMarker, TimelineEntry, TimelineItem, ToolOutput, fold_timeline,
};
