use std::io;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Network-level failure (DNS, connect, reset, timeout, TLS).
    #[error("network error: {0}")]
    Http(#[from] ureq::Error),

    /// GitHub answered with an error status. 403 and 429 mean it limits how
    /// often this address may ask.
    #[error("GitHub answered {status}{}", if .message.is_empty() { String::new() } else { format!(": {}", .message) })]
    Status { status: u16, message: String },

    /// An answer that isn't a release, or a release without a version.
    #[error("unexpected answer from GitHub: {0}")]
    Protocol(String),

    /// The download isn't the file the release lists: cut short or changed.
    #[error("the download is damaged: {0}")]
    Corrupt(String),

    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: io::Error,
    },

    /// A tool the installation needs (`hdiutil`, `ditto`, `codesign`) failed.
    #[error("{0}")]
    Tool(String),

    /// Stopped through the cancel flag.
    #[error("cancelled")]
    Cancelled,
}

impl Error {
    pub(crate) fn io(context: impl Into<String>, source: io::Error) -> Self {
        Error::Io { context: context.into(), source }
    }

    /// Whether GitHub refused to answer because this address asked too often.
    pub fn is_rate_limited(&self) -> bool {
        matches!(self, Error::Status { status: 403 | 429, .. })
    }

    /// Whether the cause is the connection rather than GitHub or this computer.
    pub fn is_network(&self) -> bool {
        matches!(self, Error::Http(_))
    }

    /// Whether this computer refused a file operation: the installation's
    /// folder can't be changed by this user.
    pub fn is_permission_denied(&self) -> bool {
        matches!(self, Error::Io { source, .. } if source.kind() == io::ErrorKind::PermissionDenied)
    }
}
