//! AI video-editing pipelines for Rushes.
//!
//! Drives a headless coding agent (Codex: `codex exec --json`, or Claude Code:
//! `claude -p --output-format stream-json`) that edits with Rushes' own tools (`rushes-edit`, over MCP) or with
//! the [video-use](https://github.com/browser-use/video-use) skill. The same agent
//! writes a video's YouTube details ([`write_details`]), and Codex draws its
//! thumbnails in a chat that remembers earlier turns ([`generate_thumbnails`]).
//! Synchronous and blocking: run it on a background thread with an event callback and
//! a cancellation flag. See `docs/pipeline.md`.

mod agent;
mod codex;
mod details;
mod doctor;
mod error;
mod event;
mod exec;
mod media;
mod pipeline;
mod process;
mod thumbnails;

pub use agent::{AgentConfig, AgentKind, DEFAULT_ALLOWED_TOOLS, DEFAULT_COMMAND, DEFAULT_PERMISSION_MODE};
pub use details::{
    DETAILS_SCHEMA, DetailsRequest, VideoDetails, WrittenDetails, details_prompt, has_transcript, parse_details,
    read_edit_notes, read_transcript, write_details,
};
pub use doctor::{Check, Doctor, default_skill_dir, doctor, doctor_rushes};
pub use error::PipelineError;
pub use event::{AgentEvent, RUSHES_TOOL_PREFIX, parse_codex_line, parse_line};
pub use media::{FOOTAGE_EXTENSIONS, FootageFile, MediaInfo, list_footage, probe_media};
pub use pipeline::{
    Harness, Phase, PhaseOutcome, RunContext, ToolServer, build_prompt, build_rushes_prompt, keep_final_cut, kept_cuts,
    run_phase, set_aside_final_cut,
};
pub use process::{AgentProcess, stop_leftover_agent};
pub use thumbnails::{
    CodexConfig, DEFAULT_CODEX_COMMAND, DEFAULT_IDEAS, MAX_THUMBNAILS, THUMBNAIL_HEIGHT, THUMBNAIL_WIDTH,
    ThumbnailOutcome, ThumbnailRequest, follow_up_prompt, generate_thumbnails, list_thumbnails, thumbnail_prompt,
};
