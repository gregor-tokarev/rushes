//! Before the workspace opens: VideoCRM's app and data renamed to Rushes, one
//! copy of Rushes per data folder, and a small window that says why Rushes
//! can't start when it can't.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
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
use rushes_core::StoreError;
use rushes_core::input::display_user_path;
use rushes_core::settings::{DATABASE_FILE, LEGACY_DATABASE_FILE};
use rushes_update::Installation;
use rusqlite::ErrorCode;

actions!(
    startup_notice,
    [
        /// Close the notice and quit.
        Dismiss
    ]
);

const KEY_CONTEXT: &str = "StartupNotice";

/// File in the data folder that the running copy of Rushes keeps locked.
const LOCK_FILE: &str = "rushes.lock";
/// The lock file of VideoCRM, as Rushes was called before the rename.
const LEGACY_LOCK_FILE: &str = "videocrm.lock";

/// Gives the app Rushes's name where macOS shows VideoCRM's, which an update
/// from VideoCRM leaves: renames the `VideoCRM.app` this copy runs from to
/// `Rushes.app` and starts that once this process exits (returns true: this
/// process quits), and points the Dock's VideoCRM icon at `Rushes.app`.
pub fn adopt_rushes_app_name() -> bool {
    let Some(installation) = Installation::detect() else {
        return false;
    };
    match installation.rename_legacy_bundle() {
        Ok(Some(renamed)) => match renamed.start_after_exit() {
            Ok(()) => return true,
            Err(error) => log_line!("rushes: cannot open {}: {error}", renamed.path().display()),
        },
        Ok(None) => {}
        Err(error) => log_line!("rushes: cannot rename VideoCRM.app to Rushes.app: {error}"),
    }
    if let Err(error) = installation.adopt_legacy_dock_icon() {
        log_line!("rushes: cannot move VideoCRM's Dock icon to Rushes: {error}");
    }
    false
}

/// Moves the data of VideoCRM, as Rushes was called before the rename, from
/// its folder `legacy` to `data_dir`, once: while `data_dir` doesn't exist yet.
/// Returns whether it moved. The database takes Rushes's file name before the
/// folder moves, so a move cut short finishes at the next start.
pub fn adopt_legacy_data(legacy: Option<&Path>, data_dir: &Path) -> Result<bool, StartupProblem> {
    let Some(legacy) = legacy.filter(|legacy| legacy.is_dir() && !data_dir.exists()) else {
        return Ok(false);
    };
    // VideoCRM keeps this file locked while it runs; holding it keeps VideoCRM
    // from starting on the folder midway.
    let lock =
        OpenOptions::new().read(true).write(true).create(true).truncate(false).open(legacy.join(LEGACY_LOCK_FILE));
    match lock.as_ref().map(File::try_lock) {
        Ok(Ok(())) => {}
        Ok(Err(TryLockError::WouldBlock)) => return Err(StartupProblem::LegacyRunning),
        Ok(Err(TryLockError::Error(error))) => log_line!("rushes: cannot lock {}: {error}", legacy.display()),
        Err(error) => log_line!("rushes: cannot lock {}: {error}", legacy.display()),
    }
    let moved = rename_legacy_database(legacy).and_then(|()| fs::rename(legacy, data_dir));
    drop(lock);
    if let Err(error) = moved {
        return Err(StartupProblem::LegacyData { from: legacy.to_path_buf(), error });
    }
    _ = fs::remove_file(data_dir.join(LEGACY_LOCK_FILE));
    Ok(true)
}

/// Renames VideoCRM's database in `dir` to Rushes's name, its write-ahead log
/// and shared memory first: the database stays complete under either name.
fn rename_legacy_database(dir: &Path) -> io::Result<()> {
    if !dir.join(LEGACY_DATABASE_FILE).exists() || dir.join(DATABASE_FILE).exists() {
        return Ok(());
    }
    for suffix in ["-wal", "-shm", ""] {
        let from = dir.join(format!("{LEGACY_DATABASE_FILE}{suffix}"));
        match fs::rename(&from, dir.join(format!("{DATABASE_FILE}{suffix}"))) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            result => result?,
        }
    }
    Ok(())
}

/// The data folder's lock, held for as long as the process runs. Without it a
/// second copy of Rushes on the same folder would save its stale copies of
/// documents and videos over this one's edits, and mark this one's running
/// uploads and AI edits as interrupted.
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
                log_line!("rushes: cannot create the lock file {}: {error}", path.display());
                return Ok(Self { _file: None });
            }
        };
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: Some(file) }),
            Err(TryLockError::WouldBlock) => Err(StartupProblem::AlreadyRunning),
            Err(TryLockError::Error(error)) => {
                log_line!("rushes: cannot lock {}: {error}", path.display());
                Ok(Self { _file: None })
            }
        }
    }
}

/// Why the workspace can't open.
#[derive(Debug)]
pub enum StartupProblem {
    /// Another copy of Rushes holds the data folder.
    AlreadyRunning,
    /// VideoCRM, as Rushes was called before the rename, runs on the data to move.
    LegacyRunning,
    /// VideoCRM's data folder, `from`, couldn't move to the data folder.
    LegacyData { from: PathBuf, error: io::Error },
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
                "Rushes can’t use its data folder".to_string(),
                format!(
                    "It needs to create and change files in {folder}. Check that the folder belongs to you and isn’t \
                     read-only, then open Rushes again."
                ),
            )
        };
        match self {
            StartupProblem::AlreadyRunning => (
                "Rushes is already open".to_string(),
                "Switch to its window to keep working. Only one copy of Rushes can use a data folder at a time, so \
                 their edits can’t overwrite each other."
                    .to_string(),
            ),
            StartupProblem::LegacyRunning => (
                "Quit VideoCRM to open Rushes".to_string(),
                "VideoCRM is called Rushes now, and Rushes takes over its videos. Quit VideoCRM so they can move, \
                 then open Rushes again."
                    .to_string(),
            ),
            StartupProblem::LegacyData { from, .. } => (
                "Rushes can’t move VideoCRM’s videos".to_string(),
                format!(
                    "VideoCRM is called Rushes now, and its data moves from {} to {folder}. Check that both folders \
                     belong to you and aren’t read-only, then open Rushes again.",
                    display_user_path(from)
                ),
            ),
            StartupProblem::Database(StoreError::CreateDir { .. } | StoreError::CreateFile { .. }) => unwritable(),
            StartupProblem::Database(StoreError::UnsupportedSchema { .. }) => (
                "This database needs a newer version of Rushes".to_string(),
                format!("A newer version of Rushes changed {file}. Install that version to keep using your videos."),
            ),
            StartupProblem::Database(StoreError::Sqlite(error)) => match error.sqlite_error_code() {
                Some(ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt) => (
                    "Rushes can’t read its database".to_string(),
                    format!(
                        "{file} is damaged or isn’t a Rushes database. Move it out of the data folder to start \
                         with an empty database, or put back a copy from a backup."
                    ),
                ),
                Some(ErrorCode::ReadOnly | ErrorCode::PermissionDenied | ErrorCode::CannotOpen) => unwritable(),
                Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => (
                    "Rushes can’t open its database".to_string(),
                    format!("Another program is using {file}. Close it, then open Rushes again."),
                ),
                _ => (
                    "Rushes can’t open its database".to_string(),
                    format!("Something went wrong reading {file}. Open Rushes again to retry."),
                ),
            },
            StartupProblem::Database(StoreError::NotFound { .. }) => (
                "Rushes can’t open its database".to_string(),
                format!("Something in {file} is missing. Open Rushes again to retry."),
            ),
        }
    }

    /// Whether the data folder holds something the creator can act on.
    fn shows_data_folder(&self) -> bool {
        !matches!(
            self,
            StartupProblem::AlreadyRunning
                | StartupProblem::LegacyRunning
                | StartupProblem::Database(StoreError::UnsupportedSchema { .. })
        )
    }
}

/// Opens a small window explaining `problem`; closing it quits. The library
/// error goes to the log.
pub fn open_startup_notice(problem: StartupProblem, data_dir: PathBuf, cx: &mut App) {
    match &problem {
        StartupProblem::Database(error) => {
            log_line!("rushes: cannot open the database in {}: {error}", data_dir.display());
        }
        StartupProblem::LegacyData { from, error } => {
            log_line!("rushes: cannot move {} to {}: {error}", from.display(), data_dir.display());
        }
        StartupProblem::AlreadyRunning | StartupProblem::LegacyRunning => {}
    }
    cx.bind_keys([
        KeyBinding::new("escape", Dismiss, Some(KEY_CONTEXT)),
        KeyBinding::new("enter", Dismiss, Some(KEY_CONTEXT)),
    ]);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(520.), px(240.)), cx))),
        is_resizable: false,
        titlebar: Some(TitlebarOptions { title: Some("Rushes".into()), ..TitleBar::title_bar_options() }),
        app_id: Some("rushes".into()),
        #[cfg(target_os = "linux")]
        window_decorations: Some(gpui_kit::WindowDecorations::Client),
        ..TitleBar::window_options()
    };
    let opened = gpui_kit::open_window(options, cx, move |window, cx| {
        window.set_window_title("Rushes");
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
        log_line!("rushes: cannot open a window: {error}");
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
                TitleBar::new().child(div().text_sm().font_medium().text_color(theme.muted_foreground).child("Rushes")),
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
    fn videocrms_data_moves_to_rushes_with_its_database() {
        let root = tempfile::tempdir().expect("temp dir");
        let legacy = root.path().join("videocrm");
        let data_dir = root.path().join("rushes");
        fs::create_dir_all(legacy.join("youtube-accounts")).expect("legacy folder");
        fs::write(legacy.join("youtube-accounts/0.json"), b"{}").expect("sign-in");
        fs::write(legacy.join(LEGACY_LOCK_FILE), b"").expect("VideoCRM's lock file");
        // VideoCRM quit without checkpointing: the newest video is only in the log.
        let mut store = rushes_core::Store::open(legacy.join(LEGACY_DATABASE_FILE)).expect("VideoCRM's database");
        store.insert_video(&rushes_core::Video::new("Kept through the move")).expect("video");
        std::mem::forget(store);
        assert!(legacy.join("videocrm.db-wal").is_file());

        assert!(adopt_legacy_data(Some(&legacy), &data_dir).expect("moves"));

        assert!(!legacy.exists());
        assert!(data_dir.join("youtube-accounts/0.json").is_file());
        assert!(!data_dir.join(LEGACY_LOCK_FILE).exists(), "VideoCRM's lock file goes");
        assert!(!data_dir.join(LEGACY_DATABASE_FILE).exists());
        let store = rushes_core::Store::open(data_dir.join(DATABASE_FILE)).expect("the moved database");
        let titles: Vec<String> = store.list_videos().expect("videos").into_iter().map(|video| video.title).collect();
        assert_eq!(titles, ["Kept through the move"]);

        // Once Rushes has its folder, VideoCRM's (made again by an old copy) stays.
        fs::create_dir_all(&legacy).expect("a new VideoCRM folder");
        assert!(!adopt_legacy_data(Some(&legacy), &data_dir).expect("nothing to do"));
        assert!(legacy.is_dir());
        assert!(!adopt_legacy_data(None, &root.path().join("chosen")).expect("no legacy folder"));
    }

    #[test]
    fn videocrms_data_stays_while_videocrm_runs() {
        let root = tempfile::tempdir().expect("temp dir");
        let legacy = root.path().join("videocrm");
        let data_dir = root.path().join("rushes");
        fs::create_dir_all(&legacy).expect("legacy folder");
        let videocrm = File::create(legacy.join(LEGACY_LOCK_FILE)).expect("lock file");
        videocrm.try_lock().expect("VideoCRM holds its folder");

        assert!(matches!(adopt_legacy_data(Some(&legacy), &data_dir), Err(StartupProblem::LegacyRunning)));
        assert!(legacy.is_dir() && !data_dir.exists());

        drop(videocrm);
        assert!(adopt_legacy_data(Some(&legacy), &data_dir).expect("moves once VideoCRM quit"));
    }

    #[test]
    fn a_database_rename_cut_short_finishes() {
        let dir = tempfile::tempdir().expect("temp dir");
        fs::write(dir.path().join("rushes.db-wal"), b"log").expect("renamed log");
        fs::write(dir.path().join(LEGACY_DATABASE_FILE), b"database").expect("database");

        rename_legacy_database(dir.path()).expect("renames");

        assert_eq!(fs::read(dir.path().join(DATABASE_FILE)).expect("database"), b"database");
        assert_eq!(fs::read(dir.path().join("rushes.db-wal")).expect("log"), b"log");
        assert!(!dir.path().join(LEGACY_DATABASE_FILE).exists());
    }

    #[test]
    fn startup_copy_names_the_problem_without_library_text() {
        let dir = PathBuf::from("/srv/rushes");
        let corrupt = StoreError::Sqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_NOTADB),
            Some("file is not a database".into()),
        ));
        let (title, message) = StartupProblem::Database(corrupt).copy(&dir);
        assert_eq!(title, "Rushes can’t read its database");
        assert!(message.starts_with("/srv/rushes/rushes.db is damaged"));
        assert!(!message.contains("file is not a database"));

        let denied = StoreError::CreateFile {
            path: dir.join(DATABASE_FILE),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        };
        let (title, message) = StartupProblem::Database(denied).copy(&dir);
        assert_eq!(title, "Rushes can’t use its data folder");
        assert!(!message.contains("os error"));
    }
}
