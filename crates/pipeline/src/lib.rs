//! AI video-editing pipelines for VideoCRM.
//!
//! Drives the [video-use](https://github.com/browser-use/video-use) skill through a
//! headless coding agent (Claude Code: `claude -p --output-format stream-json`).
//! Synchronous and blocking: run it on a background thread with an event callback and
//! a cancellation flag. See `docs/pipeline.md`.

mod agent;
mod doctor;
mod error;
mod event;
mod exec;
mod media;
mod pipeline;
mod process;

pub use agent::{AgentConfig, DEFAULT_ALLOWED_TOOLS, DEFAULT_COMMAND, DEFAULT_PERMISSION_MODE};
pub use doctor::{Check, Doctor, default_skill_dir, doctor};
pub use error::PipelineError;
pub use event::{AgentEvent, parse_line};
pub use media::{FOOTAGE_EXTENSIONS, FootageFile, MediaInfo, list_footage, probe_media};
pub use pipeline::{
    Phase, PhaseOutcome, RunContext, build_prompt, keep_final_cut, kept_cuts, run_phase, set_aside_final_cut,
};
pub use process::{AgentProcess, stop_leftover_agent};
