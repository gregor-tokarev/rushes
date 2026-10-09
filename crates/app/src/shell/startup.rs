//! Before the workspace opens: one VideoCRM per data folder, and a small
//! window that says why VideoCRM can't start when it can't.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Icon, StyledExt as _, TitleBar, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Bounds, Context, FocusHandle, Focusable, Global, InteractiveElement as _, IntoElement,
    KeyBinding, ParentElement as _, Render, SharedString, Styled as _, TitlebarOptions, Window, WindowBounds,
    WindowOptions, actions, div, px, size,
};
use rusqlite::ErrorCode;
use videocrm_core::StoreError;
use videocrm_core::input::display_user_path;
use videocrm_core::settings::DATABASE_FILE;

actions!(
    startup_notice,
    [
        /// Close the notice and quit.
        Dismiss
    ]
);

const KEY_CONTEXT: &str = "StartupNotice";

/// File in the data folder that the running VideoCRM keeps locked.
const LOCK_FILE: &str = "videocrm.lock";

/// The data folder's lock, held for as long as the process runs. Without it a
/// second VideoCRM on the same folder would save its stale copies of documents
/// and videos over this one's edits, and mark this one's running uploads and
/// AI edits as interrupted.
pub struct InstanceLock {
    _file: Option<File>,
}

impl Global for InstanceLock {}

impl InstanceLock {
    /// Locks `data_dir` for this process. Fails only when another process holds
    /// the lock; a folder that can't hold a lock file (read-only, or a file
    /// system without locks) is left to the database to report.
    pub fn acquire(data_dir: &Path) -> Result<Self, StartupProblem> {
        let path = data_dir.join(LOCK_FILE);
        let file = fs::create_dir_all(data_dir)
            .and_then(|()| OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path));
        let file = match file {
            Ok(file) => file,
            Err(error) => {
                eprintln!("videocrm: cannot create the lock file {}: {error}", path.display());
                return Ok(Self { _file: None });
            }
        };
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: Some(file) }),
            Err(TryLockError::WouldBlock) => Err(StartupProblem::AlreadyRunning),
            Err(TryLockError::Error(error)) => {
                eprintln!("videocrm: cannot lock {}: {error}", path.display());
                Ok(Self { _file: None })
            }
        }
    }
}

/// Why the workspace can't open.
#[derive(Debug)]
pub enum StartupProblem {
    /// Another VideoCRM holds the data folder.
    AlreadyRunning,
    /// The database could not be opened or read.
    Database(StoreError),
}

impl StartupProblem {
    /// Title and message for the notice about `data_dir`.
    fn copy(&self, data_dir: &Path) -> (String, String) {
        let folder = display_user_path(data_dir);
        let file = display_user_path(&data_dir.join(DATABASE_FILE));
        let unwritable = || {
            (
                "VideoCRM can’t use its data folder".to_string(),
                format!(
                    "It needs to create and change files in {folder}. Check that the folder belongs to you and isn’t \
                     read-only, then open VideoCRM again."
                ),
            )
        };
        match self {
            StartupProblem::AlreadyRunning => (
                "VideoCRM is already open".to_string(),
                "Switch to its window to keep working. Only one VideoCRM can use a data folder at a time, so their \
                 edits can’t overwrite each other."
                    .to_string(),
            ),
            StartupProblem::Database(StoreError::CreateDir { .. } | StoreError::CreateFile { .. }) => unwritable(),
            StartupProblem::Database(StoreError::UnsupportedSchema { .. }) => (
                "This database needs a newer VideoCRM".to_string(),
                format!("A newer version of VideoCRM changed {file}. Install that version to keep using your videos."),
            ),
            StartupProblem::Database(StoreError::Sqlite(error)) => match error.sqlite_error_code() {
                Some(ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt) => (
                    "VideoCRM can’t read its database".to_string(),
                    format!(
                        "{file} is damaged or isn’t a VideoCRM database. Move it out of the data folder to start \
                         with an empty database, or put back a copy from a backup."
                    ),
                ),
                Some(ErrorCode::ReadOnly | ErrorCode::PermissionDenied | ErrorCode::CannotOpen) => unwritable(),
                Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => (
                    "VideoCRM can’t open its database".to_string(),
                    format!("Another program is using {file}. Close it, then open VideoCRM again."),
                ),
                _ => (
                    "VideoCRM can’t open its database".to_string(),
                    format!("Something went wrong reading {file}. Open VideoCRM again to retry."),
                ),
            },
            StartupProblem::Database(StoreError::NotFound { .. }) => (
                "VideoCRM can’t open its database".to_string(),
                format!("Something in {file} is missing. Open VideoCRM again to retry."),
            ),
        }
    }

    /// Whether the data folder holds something the creator can act on.
    fn shows_data_folder(&self) -> bool {
        !matches!(self, StartupProblem::AlreadyRunning | StartupProblem::Database(StoreError::UnsupportedSchema { .. }))
    }
}

/// Opens a small window explaining `problem`; closing it quits. The library
/// error goes to the log.
pub fn open_startup_notice(problem: StartupProblem, data_dir: PathBuf, cx: &mut App) {
    if let StartupProblem::Database(error) = &problem {
        eprintln!("videocrm: cannot open the database in {}: {error}", data_dir.display());
    }
    cx.bind_keys([
        KeyBinding::new("escape", Dismiss, Some(KEY_CONTEXT)),
        KeyBinding::new("enter", Dismiss, Some(KEY_CONTEXT)),
    ]);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(520.), px(240.)), cx))),
        is_resizable: false,
        titlebar: Some(TitlebarOptions { title: Some("VideoCRM".into()), ..TitleBar::title_bar_options() }),
        app_id: Some("videocrm".into()),
        #[cfg(target_os = "linux")]
        window_decorations: Some(gpui_kit::WindowDecorations::Client),
        ..TitleBar::window_options()
    };
    let opened = gpui_kit::open_window(options, cx, move |window, cx| {
        window.set_window_title("VideoCRM");
        window.on_window_should_close(cx, |_, cx| {
            cx.quit();
            true
        });
        let notice = cx.new(|cx| StartupNotice::new(problem, data_dir, cx));
        let focus = notice.focus_handle(cx);
        window.defer(cx, move |window, cx| focus.focus(window, cx));
        notice
    });
    if let Err(error) = opened {
        eprintln!("videocrm: cannot open a window: {error}");
        cx.quit();
    }
}

/// The content of the startup window.
pub struct StartupNotice {
    title: SharedString,
    message: SharedString,
    data_dir: Option<PathBuf>,
    focus_handle: FocusHandle,
}

impl StartupNotice {
    pub fn new(problem: StartupProblem, data_dir: PathBuf, cx: &mut Context<Self>) -> Self {
        let (title, message) = problem.copy(&data_dir);
        Self {
            title: title.into(),
            message: message.into(),
            data_dir: (problem.shows_data_folder() && data_dir.is_dir()).then_some(data_dir),
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn title(&self) -> &SharedString {
        &self.title
    }

    pub fn message(&self) -> &SharedString {
        &self.message
    }
}

impl Focusable for StartupNotice {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for StartupNotice {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .id("startup-notice")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(|_: &Dismiss, _, cx| cx.quit())
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .font_family(theme.font_family.clone())
            .child(
                TitleBar::new()
                    .child(div().text_sm().font_medium().text_color(theme.muted_foreground).child("VideoCRM")),
            )
            .child(
                h_flex()
                    .flex_1()
                    .items_start()
                    .gap_4()
                    .px_6()
                    .pt_5()
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .size_10()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .bg(theme.warning.opacity(0.15))
                            .child(Icon::new(IconName::TriangleAlert).text_color(theme.warning)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1p5()
                            .child(div().text_lg().font_semibold().child(self.title.clone()))
                            .child(div().text_sm().text_color(theme.muted_foreground).child(self.message.clone())),
                    ),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .px_6()
                    .pb_5()
                    .when_some(self.data_dir.clone(), |this, dir| {
                        this.child(
                            Button::new("startup-open-data-folder")
                                .outline()
                                .icon(IconName::FolderOpen)
                                .label("Open data folder")
                                .on_click(move |_, _, cx| cx.open_with_system(&dir)),
                        )
                    })
                    .child(
                        Button::new("startup-quit")
                            .primary()
                            .label(if self.data_dir.is_some() { "Quit" } else { "OK" })
                            .on_click(|_, _, cx| cx.quit()),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_lock_on_the_same_folder_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let first = InstanceLock::acquire(dir.path()).expect("first lock");
        assert!(matches!(InstanceLock::acquire(dir.path()), Err(StartupProblem::AlreadyRunning)));
        drop(first);
        assert!(InstanceLock::acquire(dir.path()).is_ok());
    }

    #[test]
    fn startup_copy_names_the_problem_without_library_text() {
        let dir = PathBuf::from("/srv/videocrm");
        let corrupt = StoreError::Sqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_NOTADB),
            Some("file is not a database".into()),
        ));
        let (title, message) = StartupProblem::Database(corrupt).copy(&dir);
        assert_eq!(title, "VideoCRM can’t read its database");
        assert!(message.starts_with("/srv/videocrm/videocrm.db is damaged"));
        assert!(!message.contains("file is not a database"));

        let denied = StoreError::CreateFile {
            path: dir.join(DATABASE_FILE),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        };
        let (title, message) = StartupProblem::Database(denied).copy(&dir);
        assert_eq!(title, "VideoCRM can’t use its data folder");
        assert!(!message.contains("os error"));
    }
}
