use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum EditError {
    /// A required program (ffmpeg, ffprobe) is not on `PATH`.
    #[error("{0} not found on PATH")]
    ToolMissing(&'static str),
    /// An external program failed; `detail` is the end of its error output.
    #[error("{program} failed: {detail}")]
    ToolFailed { program: String, detail: String },
    #[error("no media files in {0}")]
    NoFootage(PathBuf),
    /// Something the agent asked for does not exist or does not fit (an unknown phrase
    /// id, a range outside a take): the message says what to change.
    #[error("{0}")]
    Invalid(String),
    #[error("transcription failed: {0}")]
    Transcription(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl EditError {
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }
}

pub type Result<T, E = EditError> = std::result::Result<T, E>;
