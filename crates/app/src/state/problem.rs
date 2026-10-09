//! Errors as the creator reads them.
//!
//! Library errors are written for developers ("could not create folder …:
//! Permission denied (os error 13)"). [`Problem`] is what the app shows instead:
//! a short title, a sentence on what happened and how to recover, and where to
//! fix it. The library error itself goes to the log.

use std::io;
use std::path::Path;

use gpui_kit::SharedString;
use rusqlite::ErrorCode;
use videocrm_core::StoreError;
use videocrm_core::input::display_user_path;
use videocrm_core::media::MediaError;

use crate::route::SettingsSection;

/// Where the creator can fix the cause of a [`Problem`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Remedy {
    OpenSettings(SettingsSection),
}

impl Remedy {
    /// The label of the button that applies the remedy.
    pub fn label(self) -> &'static str {
        match self {
            Remedy::OpenSettings(_) => "Open settings",
        }
    }
}

/// A user-facing error: what happened, how to recover, and optionally where.
#[derive(Clone, Debug, PartialEq)]
pub struct Problem {
    title: SharedString,
    message: SharedString,
    remedy: Option<Remedy>,
}

impl Problem {
    /// `title` names what failed ("Couldn’t save your changes"); `message` says
    /// why and what to do next. Both are sentences in the app's voice.
    pub fn new(title: impl Into<SharedString>, message: impl Into<SharedString>) -> Self {
        Self { title: title.into(), message: message.into(), remedy: None }
    }

    pub fn with_remedy(mut self, remedy: Remedy) -> Self {
        self.remedy = Some(remedy);
        self
    }

    pub fn title(&self) -> &SharedString {
        &self.title
    }

    pub fn message(&self) -> &SharedString {
        &self.message
    }

    pub fn remedy(&self) -> Option<Remedy> {
        self.remedy
    }

    /// A database error while doing what `title` names. Logs the error.
    pub fn store(title: &str, error: &StoreError) -> Self {
        log(title, error);
        let data_folder = || Remedy::OpenSettings(SettingsSection::General);
        let (message, remedy) = match error {
            StoreError::Sqlite(error) => match error.sqlite_error_code() {
                Some(ErrorCode::DiskFull) => ("The disk is full. Free up some space, then try again.", None),
                Some(ErrorCode::ReadOnly | ErrorCode::PermissionDenied | ErrorCode::CannotOpen) => {
                    (DATA_FOLDER_UNWRITABLE, Some(data_folder()))
                }
                Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => {
                    ("The database is busy. Try again in a moment.", None)
                }
                Some(ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase) => (
                    "The database file is damaged. Quit VideoCRM and restore videocrm.db in the data folder from a backup.",
                    Some(data_folder()),
                ),
                _ => ("Try again. If it keeps happening, restart VideoCRM.", None),
            },
            StoreError::CreateDir { .. } | StoreError::CreateFile { .. } => {
                (DATA_FOLDER_UNWRITABLE, Some(data_folder()))
            }
            StoreError::UnsupportedSchema { .. } => {
                ("A newer version of VideoCRM changed the database. Update VideoCRM to keep using it.", None)
            }
            StoreError::NotFound { .. } => ("It was deleted in the meantime.", None),
        };
        let problem = Problem::new(title.to_string(), message);
        match remedy {
            Some(remedy) => problem.with_remedy(remedy),
            None => problem,
        }
    }

    /// A video folder that could not be created or read under the media folder `root`. Logs the error.
    pub fn media(root: &Path, error: &MediaError) -> Self {
        let (title, path, source) = match error {
            MediaError::CreateDir { path, source } => ("Couldn’t create the video’s folder", path, source),
            MediaError::ReadDir { path, source } => ("Couldn’t read the video’s folder", path, source),
        };
        log(title, error);
        // The folder under the media root is ours; the root is what the creator chose.
        let shown = if path.starts_with(root) { root } else { path.as_path() };
        let message = format!("{} Choose another media folder in Settings.", folder_error_reason(shown, source));
        Problem::new(title, message).with_remedy(Remedy::OpenSettings(SettingsSection::General))
    }
}

const DATA_FOLDER_UNWRITABLE: &str =
    "VideoCRM can’t write to its data folder. Check that the folder is yours and not read-only.";

/// One sentence on why a folder at or under `path` can't be created or used.
pub(crate) fn folder_error_reason(path: &Path, error: &io::Error) -> String {
    let path = display_user_path(path);
    match error.kind() {
        io::ErrorKind::PermissionDenied => format!("VideoCRM isn’t allowed to create folders in {path}."),
        io::ErrorKind::ReadOnlyFilesystem => format!("{path} is on a read-only drive."),
        io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded => format!("The drive with {path} is full."),
        io::ErrorKind::NotADirectory | io::ErrorKind::AlreadyExists => {
            format!("{path} or a folder in it is a file, not a folder.")
        }
        _ => format!("VideoCRM can’t create folders in {path}. Check that its drive is connected."),
    }
}

/// Keeps the technical detail out of the UI but in the log.
fn log(context: &str, error: &dyn std::error::Error) {
    eprintln!("videocrm: {context}: {error}");
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn media_problems_name_the_media_folder_and_the_fix() {
        let root = PathBuf::from("/srv/forbidden-media");
        let path = root.join("launch-day-0192f3a4/footage");
        let error = MediaError::CreateDir { path, source: io::Error::from(io::ErrorKind::PermissionDenied) };
        let problem = Problem::media(&root, &error);
        assert_eq!(problem.title(), "Couldn’t create the video’s folder");
        assert_eq!(
            problem.message(),
            "VideoCRM isn’t allowed to create folders in /srv/forbidden-media. Choose another media folder in Settings."
        );
        assert_eq!(problem.remedy(), Some(Remedy::OpenSettings(SettingsSection::General)));
    }

    #[test]
    fn store_problems_leave_out_the_library_error() {
        let error = StoreError::Sqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL),
            Some("database or disk is full".into()),
        ));
        let problem = Problem::store("Couldn’t save your changes", &error);
        assert_eq!(problem.message(), "The disk is full. Free up some space, then try again.");
        assert_eq!(problem.remedy(), None);

        let error = StoreError::NotFound { entity: "video", id: "0192f3a4".into() };
        let problem = Problem::store("Couldn’t save your changes", &error);
        assert!(!problem.message().contains("0192f3a4"));
    }
}
