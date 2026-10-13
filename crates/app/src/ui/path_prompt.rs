//! Asking for a file or folder by its path, for when the system chooser can't
//! open (on Linux it comes from xdg-desktop-portal; the log says why it
//! failed). Every place that chooses a path falls back to this one dialog: the
//! same title, one sentence on what to type, a field that takes `~/…`, and the
//! reason a path can't be used next to the field, so a typo is fixed in place.
//! Writing to a file that exists waits until the creator says to replace it.

use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogDescription, DialogFooter};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Render, Role,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task, TestSupportExt as _, Window, div,
};
use rushes_core::input::{PathInputError, display_user_path, expand_user_path};

use super::dialog::form_footer_with;

/// Element id of the path field.
pub const INPUT_ID: &str = "path-prompt-input";
/// Element id of the reason a typed path can't be used.
pub const ERROR_ID: &str = "path-prompt-error";
/// Element id of the question whether to replace an existing file.
pub const REPLACE_ID: &str = "path-prompt-replace";
/// Element id of the button that takes the path.
pub const COMMIT_ID: &str = "path-prompt-commit";
/// Element id of the request's own action, on the leading edge of the footer.
pub const SECONDARY_ID: &str = "path-prompt-secondary";

/// What the path names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathKind {
    /// An existing file, to read.
    File,
    /// An existing folder.
    Folder,
    /// A file to write. It may not exist yet; an existing one is replaced
    /// only once the creator says so.
    NewFile,
    /// A folder that may not exist yet; whoever takes the path creates it.
    NewFolder,
}

impl PathKind {
    fn is_folder(self) -> bool {
        matches!(self, PathKind::Folder | PathKind::NewFolder)
    }

    fn title(self) -> &'static str {
        if self.is_folder() { "Couldn’t open the folder chooser" } else { "Couldn’t open the file chooser" }
    }
}

/// What happens after a path was taken, once the dialog has closed.
pub type Finish = Box<dyn FnOnce(&mut Window, &mut App)>;

/// Taking a path: what to do once the dialog has closed, or why the path
/// can't be used, which the dialog shows next to the field.
pub type Accepted = Task<Result<Finish, SharedString>>;

/// Turns down a path before it is looked up.
type Check = Rc<dyn Fn(&Path) -> Result<(), SharedString>>;
/// Takes a path that names what was asked for.
type Accept = Rc<dyn Fn(PathBuf, &mut Window, &mut App) -> Accepted>;
/// The request's own action.
type Run = Rc<dyn Fn(&mut Window, &mut App)>;

/// Takes a path right away; nothing about it can fail any more.
pub fn accept_now(finish: impl FnOnce(&mut Window, &mut App) + 'static) -> Accepted {
    Task::ready(Ok(Box::new(finish)))
}

/// What a place asks for; [`open`] shows it.
pub struct PathRequest {
    kind: PathKind,
    hint: SharedString,
    commit: SharedString,
    example: SharedString,
    value: String,
    check: Option<Check>,
    secondary: Option<Secondary>,
}

/// Another way to finish, such as opening the folder to copy files into.
#[derive(Clone)]
struct Secondary {
    label: SharedString,
    icon: IconName,
    run: Run,
}

impl PathRequest {
    /// `hint` is the sentence under the title: what to type, and any other way
    /// in. `example` is a full path of the kind asked for, shown as the
    /// field's placeholder.
    pub fn new(kind: PathKind, hint: impl Into<SharedString>, example: impl Into<SharedString>) -> Self {
        let commit = match kind {
            PathKind::File => "Use file",
            PathKind::Folder | PathKind::NewFolder => "Use folder",
            PathKind::NewFile => "Save",
        };
        Self {
            kind,
            hint: hint.into(),
            commit: commit.into(),
            example: example.into(),
            value: String::new(),
            check: None,
            secondary: None,
        }
    }

    /// The label of the button that takes the path.
    pub fn commit(mut self, label: impl Into<SharedString>) -> Self {
        self.commit = label.into();
        self
    }

    /// The path the field starts with.
    pub fn value(mut self, value: impl Into<String>) -> Self {
        self.value = value.into();
        self
    }

    /// Turns down paths that don't fit whatever is there, such as a document
    /// where an image is expected. Runs before the path is looked up.
    pub fn check(mut self, check: impl Fn(&Path) -> Result<(), SharedString> + 'static) -> Self {
        self.check = Some(Rc::new(check));
        self
    }

    /// Another way to finish, on the leading edge of the footer. It closes the dialog.
    pub fn secondary(
        mut self,
        label: impl Into<SharedString>,
        icon: IconName,
        run: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        self.secondary = Some(Secondary { label: label.into(), icon, run: Rc::new(run) });
        self
    }
}

/// Asks for the path `request` describes. A path that names what was asked
/// for goes to `accept`; the dialog closes once that succeeds and then runs
/// what it returned, or stays open with the reason it gave.
pub fn open(
    request: PathRequest,
    window: &mut Window,
    cx: &mut App,
    accept: impl Fn(PathBuf, &mut Window, &mut App) -> Accepted + 'static,
) {
    let title = request.kind.title();
    let form = cx.new(|cx| PathForm::new(request, Rc::new(accept), window, cx));
    let input = form.read(cx).input.clone();
    let width = window.rem_size() * 30.;
    window.open_dialog(cx, move |dialog, _, cx| {
        let footer = form.read(cx).footer(&form);
        let (submit, closed) = (form.clone(), form.clone());
        dialog
            .title(title)
            .width(width)
            .child(form.clone())
            .footer(footer)
            .on_ok(move |_, window, cx| {
                submit.update(cx, |form, cx| form.submit(window, cx));
                false
            })
            .on_close(move |_, _, cx| closed.update(cx, |form, _| form.closed = true))
    });
    // The suggested path is selected, so typing another one replaces it; the
    // form brings its end into view once the field is laid out.
    input.update(cx, |input, cx| {
        input.focus(window, cx);
        input.select_all(window, cx);
    });
}

/// The dialog's content: the sentence, the field and what is wrong with it.
struct PathForm {
    kind: PathKind,
    hint: SharedString,
    commit: SharedString,
    example: SharedString,
    check: Option<Check>,
    secondary: Option<Secondary>,
    accept: Accept,
    input: Entity<InputState>,
    error: Option<SharedString>,
    /// The existing file the creator was asked about; taking the same path again replaces it.
    replace: Option<PathBuf>,
    /// A path is being looked up or taken.
    busy: bool,
    /// The dialog went away (Cancel, Escape); a lookup finishing late leaves it be.
    closed: bool,
    /// The suggested path waits to be scrolled to its end, where the file or
    /// folder name is, once the field has been laid out.
    reveal_end: bool,
    task: Option<Task<()>>,
    _subscription: Subscription,
}

impl PathForm {
    fn new(request: PathRequest, accept: Accept, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(request.example.clone()).default_value(request.value.clone())
        });
        // What was said about the path no longer holds once it changes.
        let subscription = cx.subscribe(&input, |this: &mut Self, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) && (this.error.is_some() || this.replace.is_some()) {
                this.error = None;
                this.replace = None;
                cx.notify();
            }
        });
        Self {
            kind: request.kind,
            hint: request.hint,
            commit: request.commit,
            example: request.example,
            check: request.check,
            secondary: request.secondary,
            accept,
            input,
            error: None,
            replace: None,
            busy: false,
            closed: false,
            reveal_end: !request.value.is_empty(),
            task: None,
            _subscription: subscription,
        }
    }

    fn footer(&self, form: &Entity<Self>) -> DialogFooter {
        let commit = Button::new(COMMIT_ID);
        let commit = if self.replace.is_some() {
            commit.danger().label("Replace")
        } else {
            commit.primary().label(self.commit.clone())
        };
        let secondary = self.secondary.clone().map(|Secondary { label, icon, run }| {
            let form = form.clone();
            Button::new(SECONDARY_ID)
                .ghost()
                .icon(icon)
                .label(label)
                .on_click(move |_, window, cx| {
                    form.update(cx, |form, _| form.closed = true);
                    window.close_dialog(cx);
                    run(window, cx);
                })
                .into_any_element()
        });
        form_footer_with("path-prompt-cancel", commit, secondary)
    }

    /// Looks the typed path up off the UI thread and hands it on, or says why it can't be used.
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let path = match expand_user_path(&self.input.read(cx).value()) {
            Ok(path) => path,
            Err(error) => return self.refuse(self.typing_error(error), cx),
        };
        if let Some(Err(reason)) = self.check.as_ref().map(|check| check(&path)) {
            return self.refuse(reason, cx);
        }
        let confirmed = self.replace.as_ref() == Some(&path);
        let kind = self.kind;
        let lookup = cx.background_spawn({
            let path = path.clone();
            async move { look_up(&path) }
        });
        self.busy = true;
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let found = lookup.await;
            let accepted = this.update_in(cx, |this, window, cx| match verdict(kind, &path, found) {
                Verdict::Refuse(reason) => {
                    this.refuse(reason, cx);
                    None
                }
                Verdict::AskReplace if !confirmed => {
                    this.busy = false;
                    this.replace = Some(path.clone());
                    cx.notify();
                    None
                }
                Verdict::Use | Verdict::AskReplace => Some((this.accept)(path.clone(), window, cx)),
            });
            let Ok(Some(accepted)) = accepted else { return };
            let accepted = accepted.await;
            _ = this.update_in(cx, |this, window, cx| match accepted {
                Ok(finish) => {
                    this.busy = false;
                    if !this.closed {
                        this.closed = true;
                        window.close_dialog(cx);
                        finish(window, cx);
                    }
                }
                Err(reason) => this.refuse(reason, cx),
            });
        }));
    }

    fn refuse(&mut self, reason: SharedString, cx: &mut Context<Self>) {
        self.busy = false;
        self.replace = None;
        self.error = Some(reason);
        cx.notify();
    }

    fn typing_error(&self, error: PathInputError) -> SharedString {
        match error {
            PathInputError::Empty if self.kind.is_folder() => "Enter the path of a folder.".into(),
            PathInputError::Empty => "Enter the path of a file.".into(),
            PathInputError::Relative => format!("Use a full path, like {}.", self.example).into(),
            PathInputError::NoHome => {
                "Rushes couldn’t find your home folder. Type the full path instead of ~.".into()
            }
        }
    }
}

impl Render for PathForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.reveal_end) {
            reveal_end(self.input.clone(), window);
        }
        let theme = cx.theme();
        let replace = self
            .replace
            .as_deref()
            .map(|path| -> SharedString { format!("“{}” already exists. Replace it?", file_name(path)).into() });
        v_flex()
            .gap_2()
            .child(DialogDescription::new().child(self.hint.clone()))
            .child(Input::new(&self.input).id(INPUT_ID))
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .id(ERROR_ID)
                        .test_support()
                        .role(Role::Alert)
                        .aria_label(error.clone())
                        .text_sm()
                        .text_color(theme.danger)
                        .child(error),
                )
            })
            .when_some(replace, |this, question| {
                this.child(
                    div()
                        .id(REPLACE_ID)
                        .test_support()
                        .role(Role::Alert)
                        .aria_label(question.clone())
                        .text_sm()
                        .text_color(theme.foreground)
                        .child(question),
                )
            })
    }
}

/// Scrolls the suggested path in `input` to its end once the field has been
/// laid out (the frame after the first one), keeping it selected: a long path
/// shows its file or folder name rather than its first folders. The field only
/// follows the caret of a partial selection, so the caret visits the end first.
fn reveal_end(input: Entity<InputState>, window: &mut Window) {
    window.on_next_frame(move |window, cx| {
        input.update(cx, |input, cx| {
            let end = input.value().len();
            // The creator may have typed or moved the caret meanwhile.
            if end > 0 && input.selected_range() == (0..end) {
                input.set_selected_range(end..end, cx);
                input.select_all(window, cx);
            }
        });
    });
}

/// What is at a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Found {
    Nothing,
    File,
    Folder,
}

fn look_up(path: &Path) -> io::Result<Found> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(Found::Folder),
        Ok(_) => Ok(Found::File),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Found::Nothing),
        Err(error) => Err(error),
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Verdict {
    Use,
    /// A file to write exists already.
    AskReplace,
    Refuse(SharedString),
}

/// Whether what was `found` at `path` is what `kind` asks for.
fn verdict(kind: PathKind, path: &Path, found: io::Result<Found>) -> Verdict {
    let shown = display_user_path(path);
    let found = match found {
        Ok(found) => found,
        Err(error) => {
            log_line!("rushes: can't look up {}: {error}", path.display());
            // A path to write to is refused for the folder it would go in.
            let folder = || path.parent().map_or_else(|| shown.clone(), display_user_path);
            return Verdict::Refuse(
                match (error.kind(), kind) {
                    (io::ErrorKind::PermissionDenied, PathKind::NewFile) => {
                        format!("Rushes isn’t allowed to save files in {}.", folder())
                    }
                    (io::ErrorKind::PermissionDenied, PathKind::NewFolder) => {
                        format!("Rushes isn’t allowed to create folders in {}.", folder())
                    }
                    (io::ErrorKind::PermissionDenied, _) => format!("Rushes isn’t allowed to open {shown}."),
                    (io::ErrorKind::NotADirectory, _) => format!("Part of {shown} is a file, not a folder."),
                    _ => format!("Rushes couldn’t open {shown}. Check that its drive is connected."),
                }
                .into(),
            );
        }
    };
    let refuse = |reason: String| Verdict::Refuse(reason.into());
    match (kind, found) {
        (PathKind::File, Found::File)
        | (PathKind::Folder | PathKind::NewFolder, Found::Folder)
        | (PathKind::NewFile | PathKind::NewFolder, Found::Nothing) => Verdict::Use,
        (PathKind::NewFile, Found::File) => Verdict::AskReplace,
        (PathKind::File, Found::Nothing) => refuse(format!("There’s no file at {shown}.")),
        (PathKind::Folder, Found::Nothing) => refuse(format!("There’s no folder at {shown}.")),
        (PathKind::File | PathKind::NewFile, Found::Folder) => refuse(format!("{shown} is a folder, not a file.")),
        (PathKind::Folder | PathKind::NewFolder, Found::File) => refuse(format!("{shown} is a file, not a folder.")),
    }
}

/// One sentence on why the file at `path` couldn't be read. Logs the error.
pub fn read_error_reason(path: &Path, error: &io::Error) -> SharedString {
    log_line!("rushes: can't read {}: {error}", path.display());
    let shown = display_user_path(path);
    match error.kind() {
        io::ErrorKind::NotFound => format!("There’s no file at {shown}."),
        io::ErrorKind::PermissionDenied => format!("Rushes isn’t allowed to read {shown}."),
        io::ErrorKind::IsADirectory => format!("{shown} is a folder, not a file."),
        io::ErrorKind::InvalidData => format!("“{}” isn’t a text file.", file_name(path)),
        _ => format!("Rushes couldn’t read {shown}. Check that its drive is connected."),
    }
    .into()
}

/// One sentence on why the file at `path` couldn't be written. Logs the error.
pub fn write_error_reason(path: &Path, error: &io::Error) -> SharedString {
    log_line!("rushes: can't write {}: {error}", path.display());
    let shown = display_user_path(path);
    let folder = path.parent().map_or_else(|| shown.clone(), display_user_path);
    match error.kind() {
        io::ErrorKind::PermissionDenied => format!("Rushes isn’t allowed to save files in {folder}."),
        io::ErrorKind::ReadOnlyFilesystem => format!("{folder} is on a read-only drive."),
        io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded => format!("The drive with {folder} is full."),
        io::ErrorKind::IsADirectory => format!("{shown} is a folder, not a file."),
        io::ErrorKind::NotADirectory | io::ErrorKind::AlreadyExists => {
            format!("Part of {folder} is a file, not a folder.")
        }
        _ => format!("Rushes couldn’t save {shown}. Check that its drive is connected."),
    }
    .into()
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{AnyWindowHandle, Bounds, TestAppContext, WindowBounds, WindowOptions, px, size};

    use super::*;

    #[test]
    fn a_path_must_name_what_was_asked_for() {
        let path = Path::new("/srv/media/notes.md");
        assert_eq!(verdict(PathKind::File, path, Ok(Found::File)), Verdict::Use);
        assert_eq!(
            verdict(PathKind::File, path, Ok(Found::Nothing)),
            Verdict::Refuse("There’s no file at /srv/media/notes.md.".into())
        );
        assert_eq!(
            verdict(PathKind::File, path, Ok(Found::Folder)),
            Verdict::Refuse("/srv/media/notes.md is a folder, not a file.".into())
        );
        assert_eq!(
            verdict(PathKind::Folder, path, Ok(Found::Nothing)),
            Verdict::Refuse("There’s no folder at /srv/media/notes.md.".into())
        );
        assert_eq!(verdict(PathKind::NewFolder, path, Ok(Found::Nothing)), Verdict::Use);
        assert_eq!(verdict(PathKind::NewFile, path, Ok(Found::Nothing)), Verdict::Use);
        assert_eq!(verdict(PathKind::NewFile, path, Ok(Found::File)), Verdict::AskReplace);
        assert_eq!(
            verdict(PathKind::File, path, Err(io::ErrorKind::PermissionDenied.into())),
            Verdict::Refuse("Rushes isn’t allowed to open /srv/media/notes.md.".into())
        );
        // Saving into a folder Rushes can't enter says so, not that it can't open the file.
        assert_eq!(
            verdict(PathKind::NewFile, path, Err(io::ErrorKind::PermissionDenied.into())),
            Verdict::Refuse("Rushes isn’t allowed to save files in /srv/media.".into())
        );
        assert_eq!(
            verdict(PathKind::NewFolder, Path::new("/root/media"), Err(io::ErrorKind::PermissionDenied.into())),
            Verdict::Refuse("Rushes isn’t allowed to create folders in /root.".into())
        );
    }

    #[test]
    fn file_errors_read_as_sentences() {
        let path = Path::new("/srv/media/notes.md");
        assert_eq!(read_error_reason(path, &io::ErrorKind::NotFound.into()), "There’s no file at /srv/media/notes.md.");
        assert_eq!(
            write_error_reason(path, &io::ErrorKind::PermissionDenied.into()),
            "Rushes isn’t allowed to save files in /srv/media."
        );
        let raw = io::Error::from_raw_os_error(2);
        assert!(!read_error_reason(path, &raw).contains("os error"));
        assert!(!write_error_reason(path, &raw).contains("os error"));
    }

    /// A window with nothing but the dialog layer, and the paths the dialog handed on.
    fn window(cx: &mut TestAppContext) -> AnyWindowHandle {
        cx.update(gpui_kit::init);
        cx.update(|cx| cx.set_reduce_motion(true));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(Default::default(), size(px(900.), px(700.))))),
            ..WindowOptions::default()
        };
        cx.update(|cx| gpui_kit::open_window(options, cx, |_, cx| cx.new(|_| gpui_kit::EmptyView)))
            .expect("open test window")
            .0
    }

    fn step(cx: &mut TestAppContext, window: AnyWindowHandle, interact: impl FnOnce(&mut Window, &mut App)) {
        cx.update_window(window, |_, window, cx| interact(window, cx)).unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
    }

    fn shows(cx: &mut TestAppContext, window: AnyWindowHandle, id: &'static str) -> Option<String> {
        cx.update_window(window, |_, window, _| {
            window.try_find(id).map(|found| found.label().unwrap_or_default().to_owned())
        })
        .unwrap()
    }

    fn has_dialog(cx: &mut TestAppContext, window: AnyWindowHandle) -> bool {
        cx.update_window(window, |_, window, cx| window.has_active_dialog(cx)).unwrap()
    }

    fn type_path(cx: &mut TestAppContext, window: AnyWindowHandle, path: &str) {
        let path = path.to_owned();
        step(cx, window, |window, cx| window.press("secondary-a", cx));
        step(cx, window, move |window, cx| window.input(&path, cx));
        step(cx, window, |window, cx| window.press("enter", cx));
    }

    #[gpui_kit::test]
    fn a_missing_file_is_explained_in_place(cx: &mut TestAppContext) {
        let window = window(cx);
        let dir = tempfile::tempdir().unwrap();
        let notes = dir.path().join("notes.md");
        std::fs::write(&notes, "# Notes").unwrap();
        let taken = Rc::new(RefCell::new(Vec::<PathBuf>::new()));
        let record = taken.clone();
        step(cx, window, move |window, cx| {
            let request = PathRequest::new(PathKind::File, "Type the path of the Markdown file instead.", "~/notes.md")
                .commit("Import");
            open(request, window, cx, move |path, _, _| {
                let record = record.clone();
                accept_now(move |_, _| record.borrow_mut().push(path))
            });
        });
        assert!(has_dialog(cx, window));

        let missing = dir.path().join("nope/missing.md");
        type_path(cx, window, &missing.display().to_string());
        assert!(has_dialog(cx, window), "a typo keeps the dialog open");
        let reason = shows(cx, window, ERROR_ID).expect("the reason is next to the field");
        assert_eq!(reason, format!("There’s no file at {}.", display_user_path(&missing)));

        // Typing again clears it, and a file that exists is taken.
        type_path(cx, window, "notes.md");
        assert_eq!(shows(cx, window, ERROR_ID).as_deref(), Some("Use a full path, like ~/notes.md."));
        type_path(cx, window, &notes.display().to_string());
        assert!(!has_dialog(cx, window));
        assert_eq!(*taken.borrow(), vec![notes]);
    }

    #[gpui_kit::test]
    fn typing_replaces_the_suggested_path(cx: &mut TestAppContext) {
        let window = window(cx);
        let dir = tempfile::tempdir().unwrap();
        let take = dir.path().join("take 1.mp4");
        std::fs::write(&take, "x").unwrap();
        let taken = Rc::new(RefCell::new(Vec::<PathBuf>::new()));
        let record = taken.clone();
        let suggested = dir.path().join("footage").display().to_string();
        step(cx, window, move |window, cx| {
            let request =
                PathRequest::new(PathKind::File, "Type the path of the video instead.", "~/take.mp4").value(suggested);
            open(request, window, cx, move |path, _, _| {
                let record = record.clone();
                accept_now(move |_, _| record.borrow_mut().push(path))
            });
        });
        // No select-all first: the suggestion is selected when the dialog opens.
        let location = format!("file://{}", take.display().to_string().replace(' ', "%20"));
        step(cx, window, move |window, cx| window.input(&location, cx));
        step(cx, window, |window, cx| window.press("enter", cx));
        assert_eq!(shows(cx, window, ERROR_ID), None, "a file:// location is a full path");
        assert!(!has_dialog(cx, window));
        assert_eq!(*taken.borrow(), vec![take]);
    }

    #[gpui_kit::test]
    fn saving_over_a_file_asks_first(cx: &mut TestAppContext) {
        let window = window(cx);
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("important.md");
        std::fs::write(&existing, "MY IMPORTANT NOTES").unwrap();
        let fail_once = Rc::new(RefCell::new(true));
        step(cx, window, move |window, cx| {
            let request =
                PathRequest::new(PathKind::NewFile, "Type where to save the Markdown file instead.", "~/a.md")
                    .commit("Export");
            open(request, window, cx, move |path, _, _| {
                if fail_once.replace(false) {
                    return Task::ready(Err("Rushes isn’t allowed to save files in ~.".into()));
                }
                Task::ready(match std::fs::write(&path, "## Hook") {
                    Ok(()) => Ok(Box::new(|_: &mut Window, _: &mut App| {}) as Finish),
                    Err(error) => Err(write_error_reason(&path, &error)),
                })
            });
        });

        type_path(cx, window, &existing.display().to_string());
        assert!(has_dialog(cx, window));
        assert_eq!(shows(cx, window, REPLACE_ID).as_deref(), Some("“important.md” already exists. Replace it?"));
        assert_eq!(std::fs::read_to_string(&existing).unwrap(), "MY IMPORTANT NOTES", "nothing is written yet");

        // A failure to write stays in the dialog; asking again goes through.
        step(cx, window, |window, cx| window.press("enter", cx));
        assert!(has_dialog(cx, window));
        assert_eq!(shows(cx, window, ERROR_ID).as_deref(), Some("Rushes isn’t allowed to save files in ~."));
        step(cx, window, |window, cx| window.press("enter", cx));
        assert!(shows(cx, window, REPLACE_ID).is_some(), "asks again");
        step(cx, window, |window, cx| window.click(COMMIT_ID, cx));
        assert!(!has_dialog(cx, window));
        assert_eq!(std::fs::read_to_string(&existing).unwrap(), "## Hook");
    }

    #[gpui_kit::test]
    fn a_folder_request_takes_folders_and_offers_its_own_way_out(cx: &mut TestAppContext) {
        let window = window(cx);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("take.mp4");
        std::fs::write(&file, "x").unwrap();
        let ran = Rc::new(RefCell::new(false));
        let secondary = ran.clone();
        step(cx, window, move |window, cx| {
            let request = PathRequest::new(PathKind::Folder, "Type the path of the folder instead.", "~/Videos")
                .secondary("Open footage folder", IconName::FolderOpen, move |_, _| *secondary.borrow_mut() = true);
            open(request, window, cx, |_, _, _| accept_now(|_, _| {}));
        });
        type_path(cx, window, &file.display().to_string());
        assert_eq!(shows(cx, window, ERROR_ID), Some(format!("{} is a file, not a folder.", display_user_path(&file))));
        step(cx, window, |window, cx| window.click(SECONDARY_ID, cx));
        assert!(!has_dialog(cx, window));
        assert!(*ran.borrow());
    }
}
