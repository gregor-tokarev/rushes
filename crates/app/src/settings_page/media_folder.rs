//! The media folder field shared by Settings and onboarding: a typed or chosen
//! folder is expanded (`~/…`), created and checked for writing before it is
//! saved, and the field always shows the folder in effect or says why the
//! typed one isn't.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, ElementId, Entity, EventEmitter, Focusable as _, InteractiveElement as _,
    IntoElement, ParentElement as _, PathPromptOptions, Render, Role, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, TestSupportExt as _, Window, div,
};
use videocrm_core::input::{display_user_path, expand_user_path};
use videocrm_core::media::default_media_root;

use crate::route::SettingsSection;
use crate::state::{AppEvent, AppState, Problem, Remedy, folder_error_reason};

/// Emitted after a typed folder was saved or turned down.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MediaFolderEvent {
    Saved,
    Rejected,
}

/// What a commit does when the typed folder can't be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Commit {
    /// The field is still shown: keep the text and explain under it.
    Keep,
    /// The field is going away: show the folder in effect again and say what
    /// happened in a notification.
    Revert,
}

pub struct MediaFolderField {
    /// Element id of the input; the Choose… button and the error derive theirs from it.
    id: ElementId,
    state: Entity<AppState>,
    input: Entity<InputState>,
    error: Option<SharedString>,
    /// Bumped by each commit; a check that finishes after a newer commit started is dropped.
    revision: usize,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<MediaFolderEvent> for MediaFolderField {}

impl MediaFolderField {
    pub fn new(id: impl Into<ElementId>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = AppState::global(cx);
        let shown = display_user_path(&state.read(cx).settings().resolved_media_root());
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(display_user_path(&default_media_root())).default_value(shown)
        });
        let subscriptions = vec![
            cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
                // GPUI reports focus changes only while the window is active,
                // so leaving the page and quitting commit on their own too.
                InputEvent::Blur | InputEvent::PressEnter { .. } => {
                    this.commit(Commit::Keep, false, window, cx).detach();
                }
                InputEvent::Change => {
                    if this.error.take().is_some() {
                        cx.notify();
                    }
                }
                _ => {}
            }),
            cx.subscribe_in(&state, window, |this, _, event: &AppEvent, window, cx| {
                // Settings changed elsewhere (another field, onboarding): follow them
                // unless the creator is editing this one.
                if *event == AppEvent::SettingsChanged && !this.is_editing(window, cx) {
                    this.show_effective(window, cx);
                }
            }),
        ];
        Self { id: id.into(), state, input, error: None, revision: 0, _subscriptions: subscriptions }
    }

    /// The inline error under the field, if the typed folder was turned down.
    pub fn error(&self) -> Option<&SharedString> {
        self.error.as_ref()
    }

    fn is_editing(&self, window: &Window, cx: &App) -> bool {
        self.input.read(cx).focus_handle(cx).is_focused(window) || self.error.is_some()
    }

    /// The folder the typed text names: `None` for the platform default (an
    /// empty field resets to it), or why the text isn't a usable path.
    fn typed_root(&self, cx: &App) -> Result<Option<PathBuf>, SharedString> {
        let text = self.input.read(cx).value();
        if text.trim().is_empty() {
            return Ok(None);
        }
        let root = expand_user_path(&text).map_err(|error| SharedString::from(error.to_string()))?;
        Ok((root != default_media_root()).then_some(root))
    }

    /// Saves the typed folder once it exists and can be written to. `check_unchanged`
    /// also checks a folder that is already in effect. Resolves to whether the
    /// folder in effect is now the typed one.
    pub fn commit(
        &mut self,
        on_error: Commit,
        check_unchanged: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        self.revision += 1;
        let revision = self.revision;
        let root = match self.typed_root(cx) {
            Ok(root) => root,
            Err(message) => {
                self.reject(message, on_error, window, cx);
                return Task::ready(false);
            }
        };
        if root == self.state.read(cx).settings().media_root && !check_unchanged {
            self.error = None;
            self.show_effective(window, cx);
            return Task::ready(true);
        }
        let folder = root.clone().unwrap_or_else(default_media_root);
        let check = cx.background_spawn({
            let folder = folder.clone();
            async move { prepare_media_folder(&folder) }
        });
        cx.spawn_in(window, async move |this, cx| {
            let checked = check.await;
            this.update_in(cx, |this, window, cx| {
                if revision != this.revision {
                    return false;
                }
                match checked {
                    Ok(()) => {
                        this.error = None;
                        this.state
                            .update(cx, |state, cx| state.update_settings(cx, |settings| settings.media_root = root));
                        this.show_effective(window, cx);
                        cx.emit(MediaFolderEvent::Saved);
                        true
                    }
                    Err(error) => {
                        eprintln!("videocrm: media folder {} can't be used: {error}", folder.display());
                        let reason = folder_error_reason(&folder, &error);
                        this.reject(format!("{reason} Choose another folder.").into(), on_error, window, cx);
                        false
                    }
                }
            })
            .unwrap_or(false)
        })
    }

    /// Saves the typed folder right away, for quitting: the app is going away,
    /// so there is no window to report to and no time for a background check.
    pub fn commit_before_quit(&mut self, cx: &mut Context<Self>) {
        let Ok(root) = self.typed_root(cx) else { return };
        if root == self.state.read(cx).settings().media_root {
            return;
        }
        let folder = root.clone().unwrap_or_else(default_media_root);
        match prepare_media_folder(&folder) {
            Ok(()) => {
                self.state.update(cx, |state, cx| state.update_settings(cx, |settings| settings.media_root = root))
            }
            Err(error) => eprintln!("videocrm: media folder {} not saved: {error}", folder.display()),
        }
    }

    fn reject(&mut self, message: SharedString, on_error: Commit, window: &mut Window, cx: &mut Context<Self>) {
        match on_error {
            Commit::Keep => self.error = Some(message),
            Commit::Revert => {
                self.error = None;
                self.show_effective(window, cx);
                let problem = Problem::new("Media folder not changed", message)
                    .with_remedy(Remedy::OpenSettings(SettingsSection::General));
                self.state.update(cx, |state, cx| state.report_problem(problem, cx));
            }
        }
        cx.emit(MediaFolderEvent::Rejected);
        cx.notify();
    }

    /// Shows the folder in effect.
    fn show_effective(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let shown = display_user_path(&self.state.read(cx).settings().resolved_media_root());
        if self.input.read(cx).value() != shown {
            self.input.update(cx, |input, cx| input.set_value(shown, window, cx));
        }
        cx.notify();
    }

    /// Lets the creator pick the folder in the system's folder dialog, then saves it.
    pub fn choose(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let chosen = match paths.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Ok(None)) | Err(_) => None,
                Ok(Err(error)) => {
                    eprintln!("videocrm: folder picker failed: {error}");
                    let message = "Couldn’t open the folder picker. Type the path instead.";
                    _ = this.update(cx, |this, cx| this.state.update(cx, |state, cx| state.report_error(message, cx)));
                    None
                }
            };
            if let Some(path) = chosen {
                _ = this.update_in(cx, |this, window, cx| {
                    this.input.update(cx, |input, cx| input.set_value(display_user_path(&path), window, cx));
                    this.commit(Commit::Keep, false, window, cx).detach();
                });
            }
        })
        .detach();
    }
}

impl Render for MediaFolderField {
    /// The input, the Choose… button and the inline error, filling the width given.
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let child_id = |name: &'static str| ElementId::NamedChild(Arc::new(self.id.clone()), name.into());
        v_flex()
            .w_full()
            .min_w_0()
            .gap_1p5()
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .child(Input::new(&self.input).id(self.id.clone()).flex_1().min_w_0())
                    .child(
                        Button::new(child_id("choose"))
                            .outline()
                            .label("Choose…")
                            .on_click(cx.listener(|this, _, window, cx| this.choose(window, cx))),
                    ),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .id(child_id("error"))
                        .test_support()
                        .role(Role::Alert)
                        .aria_label(error.clone())
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
    }
}

/// Creates `folder` (and its parents) and checks that files can be written in it.
fn prepare_media_folder(folder: &Path) -> io::Result<()> {
    fs::create_dir_all(folder)?;
    let probe = folder.join(format!(".videocrm-write-check-{}", std::process::id()));
    File::create_new(&probe)?;
    // Leaving the probe behind is harmless; failing to write it is what matters.
    _ = fs::remove_file(&probe);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::prepare_media_folder;

    #[test]
    fn a_media_folder_is_created_and_checked() {
        let dir = tempfile::tempdir().expect("temp dir");
        let folder = dir.path().join("Videos/VideoCRM");
        prepare_media_folder(&folder).expect("folder is usable");
        assert!(folder.is_dir());
        assert_eq!(std::fs::read_dir(&folder).expect("read folder").count(), 0, "the probe is removed");

        let file = dir.path().join("notes.txt");
        std::fs::write(&file, "").expect("write file");
        assert!(prepare_media_folder(&file.join("VideoCRM")).is_err());
    }
}
