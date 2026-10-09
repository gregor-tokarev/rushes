use std::io;
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error("agent command `{}` not found", .0.display())]
    AgentNotFound(PathBuf),
    /// Path of the missing `SKILL.md`.
    #[error("video-use skill not found: {} does not exist", .0.display())]
    SkillNotFound(PathBuf),
    #[error("footage folder {} does not exist", .0.display())]
    FootageNotFound(PathBuf),
    #[error("failed to start `{}`: {source}", .command.display())]
    Spawn {
        command: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cancelled")]
    Cancelled,
    /// The agent exited unsuccessfully or without reporting a result.
    #[error("agent failed: {message}")]
    AgentFailed {
        message: String,
        /// Last lines the agent wrote to stderr.
        stderr_tail: Vec<String>,
        /// Session to resume for a retry, when the agent got far enough to open one.
        session_id: Option<String>,
        /// The agent exited without reporting a result: it crashed or was killed, or the
        /// command is not Claude Code at all.
        no_result: bool,
    },
    #[error(transparent)]
    Io(#[from] io::Error),
}
