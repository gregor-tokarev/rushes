//! The footage card of the AI edit panel: the raw takes in the video's footage folder.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::StreamExt as _;
use futures::channel::mpsc::unbounded;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Div, Entity, EventEmitter, ExternalPaths, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, PathPromptOptions, Render, SharedString, Styled as _, Task, Window, div,
};
use videocrm_core::VideoId;
use videocrm_core::media::{self, MediaFile, MediaKind};
use videocrm_pipeline::{MediaInfo, probe_media};

use super::format;
use super::icons::media_icon;
use crate::state::AppState;

/// How often the folder is listed again, to pick up takes added outside the app.
const POLL_INTERVAL: Duration = Duration::from_secs(3);
const COPY_CHUNK: usize = 1 << 20;

/// The footage list changed (files added, removed or probed).
pub(crate) struct FootageChanged;

struct Take {
    file: MediaFile,
    info: Option<MediaInfo>,
}

/// Copy progress of an import.
struct Import {
    total_files: usize,
    file_ix: usize,
    name: String,
    copied: u64,
    total_bytes: u64,
}

enum ImportMessage {
    File { ix: usize, name: String },
    Copied(u64),
    Done(Result<(), String>),
}

/// Lists, probes and imports the raw takes video-use edits.
pub(crate) struct FootageCard {
    video_id: VideoId,
    state: Entity<AppState>,
    takes: Vec<Take>,
    loaded: bool,
    /// ffprobe results by path, valid while the file's modification time matches.
    probes: HashMap<PathBuf, (Option<DateTime<Utc>>, Option<MediaInfo>)>,
    import: Option<Import>,
    error: Option<SharedString>,
    refresh_task: Option<Task<()>>,
    _poll: Task<()>,
}

impl EventEmitter<FootageChanged> for FootageCard {}

impl FootageCard {
    pub(crate) fn new(video_id: VideoId, cx: &mut Context<Self>) -> Self {
        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                if this.update(cx, |this, cx| this.refresh(cx)).is_err() {
                    break;
                }
            }
        });
        let mut card = Self {
            video_id,
            state: AppState::global(cx),
            takes: Vec::new(),
            loaded: false,
            probes: HashMap::new(),
            import: None,
            error: None,
            refresh_task: None,
            _poll: poll,
        };
        card.refresh(cx);
        card
    }

    /// Whether the folder holds at least one video or audio take.
    pub(crate) fn has_takes(&self) -> bool {
        self.takes.iter().any(|take| matches!(take.file.kind, MediaKind::Video | MediaKind::Audio))
    }

    pub(crate) fn is_loaded(&self) -> bool {
        self.loaded
    }

    fn footage_dir(&self, cx: &App) -> Option<PathBuf> {
        self.state.read(cx).video(self.video_id).and_then(media::footage_dir)
    }

    /// Lists the folder again on a background thread and probes new or changed takes.
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.refresh_task.is_some() {
            return;
        }
        let Some(dir) = self.footage_dir(cx) else {
            self.set_files(Vec::new(), cx);
            return;
        };
        self.refresh_task = Some(cx.spawn(async move |this, cx| {
            let listed = cx.background_spawn(async move { media::list_footage(&dir) }).await;
            let Ok(to_probe) = this.update(cx, |this, cx| {
                this.refresh_task = None;
                match listed {
                    Ok(files) => this.set_files(files, cx),
                    Err(error) => {
                        this.error = Some(error.to_string().into());
                        cx.notify();
                        Vec::new()
                    }
                }
            }) else {
                return;
            };
            for (path, modified) in to_probe {
                let probe_path = path.clone();
                let info = cx.background_spawn(async move { probe_media(&probe_path) }).await;
                if this.update(cx, |this, cx| this.set_probe(path, modified, info, cx)).is_err() {
                    return;
                }
            }
        }));
    }

    /// Takes the new listing; returns the takes that still need probing.
    fn set_files(&mut self, files: Vec<MediaFile>, cx: &mut Context<Self>) -> Vec<(PathBuf, Option<DateTime<Utc>>)> {
        let changed = !self.loaded
            || files.len() != self.takes.len()
            || files.iter().zip(&self.takes).any(|(file, take)| *file != take.file);
        self.loaded = true;
        let mut to_probe = Vec::new();
        self.takes = files
            .into_iter()
            .map(|file| {
                let info = match self.probes.get(&file.path) {
                    Some((modified, info)) if *modified == file.modified => info.clone(),
                    _ => {
                        if file.kind != MediaKind::Image {
                            to_probe.push((file.path.clone(), file.modified));
                        }
                        None
                    }
                };
                Take { file, info }
            })
            .collect();
        if changed {
            cx.emit(FootageChanged);
            cx.notify();
        }
        to_probe
    }

    fn set_probe(
        &mut self,
        path: PathBuf,
        modified: Option<DateTime<Utc>>,
        info: Option<MediaInfo>,
        cx: &mut Context<Self>,
    ) {
        if let Some(take) = self.takes.iter_mut().find(|take| take.file.path == path && take.file.modified == modified)
        {
            take.info = info.clone();
            cx.notify();
        }
        self.probes.insert(path, (modified, info));
    }

    fn add_footage(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Add footage".into()),
        });
        cx.spawn(async move |this, cx| {
            let paths = match picked.await {
                Ok(Ok(Some(paths))) => paths,
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(error)) => {
                    _ = this.update(cx, |this, cx| {
                        this.error = Some(format!("Couldn't open the file picker: {error}").into());
                        cx.notify();
                    });
                    return;
                }
            };
            _ = this.update(cx, |this, cx| this.import(paths, cx));
        })
        .detach();
    }

    /// Copies media files into the footage folder on a background thread. Other files
    /// are skipped; existing names get a numbered suffix.
    pub(crate) fn import(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if self.import.is_some() {
            return;
        }
        let (media, skipped): (Vec<PathBuf>, Vec<PathBuf>) =
            paths.into_iter().partition(|path| MediaKind::from_path(path).is_some() && path.is_file());
        if media.is_empty() {
            self.error = Some("Only video, audio and image files can be added as footage.".into());
            cx.notify();
            return;
        }
        let Some(dir) = self
            .state
            .update(cx, |state, cx| state.ensure_video_folder(self.video_id, cx))
            .map(|folder| folder.join("footage"))
        else {
            self.error = Some("Couldn't create the video's media folder.".into());
            cx.notify();
            return;
        };
        self.error = (!skipped.is_empty()).then(|| {
            let names: Vec<String> = skipped.iter().map(|path| file_name(path)).collect();
            format!("Skipped {}: not a video, audio or image file.", names.join(", ")).into()
        });
        let total_bytes = media.iter().map(|path| fs::metadata(path).map_or(0, |meta| meta.len())).sum();
        self.import =
            Some(Import { total_files: media.len(), file_ix: 0, name: file_name(&media[0]), copied: 0, total_bytes });
        cx.notify();

        let (sender, mut receiver) = unbounded();
        let copy = cx.background_spawn(async move {
            let report = |message| _ = sender.unbounded_send(message);
            let result = media.iter().enumerate().try_for_each(|(ix, source)| {
                report(ImportMessage::File { ix, name: file_name(source) });
                copy_into(source, &dir, &mut |bytes| report(ImportMessage::Copied(bytes)))
                    .map_err(|error| format!("Couldn't copy {}: {error}", file_name(source)))
            });
            report(ImportMessage::Done(result));
        });
        cx.spawn(async move |this, cx| {
            let _copy = copy;
            while let Some(message) = receiver.next().await {
                let applied = this.update(cx, |this, cx| {
                    match message {
                        ImportMessage::File { ix, name } => {
                            if let Some(import) = &mut this.import {
                                import.file_ix = ix;
                                import.name = name;
                            }
                        }
                        ImportMessage::Copied(bytes) => {
                            if let Some(import) = &mut this.import {
                                import.copied += bytes;
                            }
                        }
                        ImportMessage::Done(result) => {
                            this.import = None;
                            if let Err(error) = result {
                                this.error = Some(error.into());
                            }
                            this.refresh(cx);
                        }
                    }
                    cx.notify();
                });
                if applied.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn open_folder(&mut self, cx: &mut Context<Self>) {
        let folder = self.state.update(cx, |state, cx| state.ensure_video_folder(self.video_id, cx));
        if let Some(folder) = folder {
            cx.open_with_system(&folder.join("footage"));
        }
    }

    fn render_header(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let durations: Vec<f64> =
            self.takes.iter().filter_map(|take| take.info.as_ref()).map(|info| info.duration_secs).collect();
        let summary = match self.takes.len() {
            0 => "video-use reads the raw takes in this folder and never changes them.".to_owned(),
            count => {
                let files = if count == 1 { "1 file".to_owned() } else { format!("{count} files") };
                if durations.is_empty() {
                    files
                } else {
                    format!("{files} · {} of footage", format::media_duration(durations.iter().sum()))
                }
            }
        };
        h_flex()
            .gap_3()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(div().text_base().font_weight(FontWeight::SEMIBOLD).child("Footage"))
                    .child(div().text_sm().text_color(theme.muted_foreground).child(summary)),
            )
            .child(
                Button::new("footage-open-folder")
                    .ghost()
                    .small()
                    .icon(IconName::FolderOpen)
                    .label("Open folder")
                    .on_click(cx.listener(|this, _, _, cx| this.open_folder(cx))),
            )
            .child(
                Button::new("footage-add")
                    .outline()
                    .small()
                    .icon(IconName::Plus)
                    .label("Add footage…")
                    .disabled(self.import.is_some())
                    .on_click(cx.listener(|this, _, _, cx| this.add_footage(cx))),
            )
    }

    fn render_list(&self, cx: &App) -> Div {
        let theme = cx.theme();
        let count = self.takes.len();
        v_flex().children(self.takes.iter().enumerate().map(|(ix, take)| {
            let meta = match (&take.info, take.file.kind) {
                (Some(info), _) => format::media_info(info),
                (None, MediaKind::Image) => "Image".to_owned(),
                (None, _) => String::new(),
            };
            h_flex()
                .gap_3()
                .px_3()
                .py_2()
                .when(ix + 1 < count, |this| this.border_b_1().border_color(theme.border))
                .child(Icon::new(media_icon(take.file.kind)).small().text_color(theme.muted_foreground))
                .child(div().flex_1().min_w_0().truncate().text_sm().child(take.file.name.clone()))
                .child(
                    div()
                        .flex_shrink_0()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .when(meta.is_empty() && take.file.kind != MediaKind::Image, |this| {
                            this.child(Spinner::new().xsmall().color(theme.muted_foreground))
                        })
                        .child(meta),
                )
                .child(
                    div()
                        .w_20()
                        .flex_shrink_0()
                        .text_right()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(format::bytes(take.file.size)),
                )
        }))
    }

    fn render_empty(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        v_flex()
            .items_center()
            .gap_2()
            .px_6()
            .py_8()
            .child(Icon::new(IconName::Clapperboard).large().text_color(theme.muted_foreground))
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("No footage yet"))
            .child(
                div()
                    .max_w_96()
                    .text_center()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("Drop your raw takes here, or add them from disk. The AI edit cuts them into one video."),
            )
    }

    fn render_import(&self, import: &Import, cx: &App) -> Div {
        let theme = cx.theme();
        let value =
            if import.total_bytes == 0 { 0.0 } else { import.copied as f32 / import.total_bytes as f32 * 100.0 };
        v_flex()
            .gap_2()
            .px_3()
            .py_2p5()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_2()
                    .text_sm()
                    .child(Spinner::new().small())
                    .child(div().flex_1().min_w_0().truncate().child(format!("Copying {}", import.name)))
                    .child(div().text_color(theme.muted_foreground).child(format!(
                        "{} of {}",
                        import.file_ix + 1,
                        import.total_files
                    ))),
            )
            .child(Progress::new("footage-import-progress").small().value(value))
    }
}

impl Render for FootageCard {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self.render_header(cx);
        let body = if self.takes.is_empty() && self.loaded { self.render_empty(cx) } else { self.render_list(cx) };
        let theme = cx.theme();
        let drop_border = theme.primary;
        let drop_fill = theme.drop_target;

        v_flex()
            .gap_3()
            .child(header)
            .child(
                v_flex()
                    .id("footage-drop")
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .overflow_hidden()
                    .when(self.takes.is_empty(), |this| this.border_dashed())
                    .drag_over::<ExternalPaths>(move |style, _, _, _| style.border_color(drop_border).bg(drop_fill))
                    .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| this.import(paths.paths().to_vec(), cx)))
                    .when_some(self.import.as_ref(), |this, import| this.child(self.render_import(import, cx)))
                    .child(body),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(div().text_sm().text_color(theme.danger).child(error))
            })
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned())
}

/// Copies `source` into `dir` under a free name, reporting copied bytes. A partial
/// copy is removed.
fn copy_into(source: &Path, dir: &Path, progress: &mut dyn FnMut(u64)) -> io::Result<()> {
    if source.parent() == Some(dir) {
        return Ok(());
    }
    let (dest, mut out) = create_free(dir, source)?;
    let result = (|| {
        let mut input = File::open(source)?;
        let mut buffer = vec![0; COPY_CHUNK];
        loop {
            let read = input.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            out.write_all(&buffer[..read])?;
            progress(read as u64);
        }
        out.sync_all()
    })();
    if result.is_err() {
        _ = fs::remove_file(&dest);
    }
    result
}

/// Creates `dir/<source name>`, or `name (2).ext`, `name (3).ext`, ... when taken.
fn create_free(dir: &Path, source: &Path) -> io::Result<(PathBuf, File)> {
    let stem = source.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
    let extension = source.extension().map(|ext| format!(".{}", ext.to_string_lossy())).unwrap_or_default();
    for n in 1.. {
        let name = if n == 1 { format!("{stem}{extension}") } else { format!("{stem} ({n}){extension}") };
        let dest = dir.join(name);
        match File::create_new(&dest) {
            Ok(file) => return Ok((dest, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    unreachable!("an unbounded range always yields a free name")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_never_overwrite_existing_takes() {
        let root = tempfile::tempdir().unwrap();
        let source_dir = root.path().join("card");
        let footage = root.path().join("footage");
        fs::create_dir_all(&source_dir).unwrap();
        fs::create_dir_all(&footage).unwrap();
        let source = source_dir.join("take1.mp4");
        fs::write(&source, vec![7u8; COPY_CHUNK + 10]).unwrap();
        fs::write(footage.join("take1.mp4"), b"existing").unwrap();

        let mut copied = 0;
        copy_into(&source, &footage, &mut |bytes| copied += bytes).unwrap();
        copy_into(&source, &footage, &mut |_| {}).unwrap();
        assert_eq!(copied, COPY_CHUNK as u64 + 10);
        assert_eq!(fs::read(footage.join("take1.mp4")).unwrap(), b"existing");
        assert_eq!(fs::read(footage.join("take1 (2).mp4")).unwrap().len(), COPY_CHUNK + 10);
        assert!(footage.join("take1 (3).mp4").is_file());

        // A file already in the folder is left alone.
        copy_into(&footage.join("take1.mp4"), &footage, &mut |_| {}).unwrap();
        assert_eq!(fs::read_dir(&footage).unwrap().count(), 3);
    }
}
