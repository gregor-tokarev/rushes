//! The footage card of the AI edit panel: the raw takes in the video's footage folder.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

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
    AnyElement, App, AppContext as _, ClipboardItem, Context, Div, Entity, EventEmitter, ExternalPaths, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, PathPromptOptions, Render, SharedString, Styled as _,
    Subscription, Task, TestSupportExt as _, Window, div,
};
use videocrm_core::VideoId;
use videocrm_core::input::display_user_path;
use videocrm_core::media::{self, MediaFile, MediaKind};
use videocrm_pipeline::{MediaInfo, probe_media};

use super::format;
use super::icons::media_icon;
use super::service::PipelineService;
use crate::state::AppState;
use crate::ui::{EmptyState, SectionHeader};

/// How often the folder is listed again, to pick up takes added outside the app.
const POLL_INTERVAL: Duration = Duration::from_secs(3);
const COPY_CHUNK: usize = 1 << 20;

/// Suffix of a take that is still being copied in.
const PARTIAL_SUFFIX: &str = ".part";
/// How often a copy reports its progress.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// The footage list changed (files added, removed or probed), or a copy started or ended.
pub(crate) struct FootageChanged;

struct Take {
    file: MediaFile,
    info: Option<MediaInfo>,
    /// ffprobe has run; `info` is `None` when it could not read the file.
    probed: bool,
}

/// Copy progress of an import.
#[derive(Clone)]
pub(crate) struct Import {
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

/// The footage copies of every video. [`PipelineService`] owns it, so a copy
/// outlives the card that started it: a card shown again follows its progress, and
/// nothing else touches its file.
pub(crate) struct FootageImports {
    running: HashMap<VideoId, Import>,
    /// Why the last copy into a video's folder failed, until the creator dismisses it.
    errors: HashMap<VideoId, SharedString>,
    /// The hidden files running copies write; sweeps leave them alone.
    partials: Arc<Mutex<HashSet<PathBuf>>>,
    /// Videos whose footage folder was cleared of copies an earlier session left.
    swept: HashSet<VideoId>,
}

impl FootageImports {
    pub(crate) fn new() -> Self {
        Self { running: HashMap::new(), errors: HashMap::new(), partials: Arc::default(), swept: HashSet::new() }
    }

    /// The copy into `video_id`'s footage folder, while one runs.
    pub(crate) fn running(&self, video_id: VideoId) -> Option<&Import> {
        self.running.get(&video_id)
    }

    pub(crate) fn error(&self, video_id: VideoId) -> Option<&SharedString> {
        self.errors.get(&video_id)
    }

    pub(crate) fn dismiss_error(&mut self, video_id: VideoId, cx: &mut Context<Self>) {
        if self.errors.remove(&video_id).is_some() {
            cx.notify();
        }
    }

    /// Removes the hidden `.<take>.<n>.part` files of copies an earlier session left
    /// unfinished (it quit mid-copy), once per video and session. A video without a
    /// folder yet counts as swept too: whatever is copied into it later is this
    /// session's. Files of running copies are never removed.
    pub(crate) fn sweep(&mut self, video_id: VideoId, footage_dir: Option<PathBuf>, cx: &mut Context<Self>) {
        if !self.swept.insert(video_id) {
            return;
        }
        let Some(dir) = footage_dir else { return };
        let partials = self.partials.clone();
        cx.background_spawn(async move { remove_partial_copies(&dir, &partials) }).detach();
    }

    /// Copies `media` into `dir`, the footage folder of `video_id`, on a background
    /// thread; existing names get a numbered suffix. One copy per video at a time.
    pub(crate) fn start(&mut self, video_id: VideoId, media: Vec<PathBuf>, dir: PathBuf, cx: &mut Context<Self>) {
        if media.is_empty() || self.running.contains_key(&video_id) {
            return;
        }
        let total_bytes = media.iter().map(|path| fs::metadata(path).map_or(0, |meta| meta.len())).sum();
        let import =
            Import { total_files: media.len(), file_ix: 0, name: file_name(&media[0]), copied: 0, total_bytes };
        self.running.insert(video_id, import);
        self.errors.remove(&video_id);
        cx.notify();

        let partials = self.partials.clone();
        let (sender, mut receiver) = unbounded();
        let copy = cx.background_spawn(async move {
            let report = |message| _ = sender.unbounded_send(message);
            let result = media.iter().enumerate().try_for_each(|(ix, source)| {
                report(ImportMessage::File { ix, name: file_name(source) });
                let (mut pending, mut reported) = (0, Instant::now());
                let copied = copy_into(source, &dir, &partials, &mut |bytes| {
                    pending += bytes;
                    if reported.elapsed() >= PROGRESS_INTERVAL {
                        report(ImportMessage::Copied(std::mem::take(&mut pending)));
                        reported = Instant::now();
                    }
                });
                report(ImportMessage::Copied(pending));
                copied.map(drop).map_err(|error| {
                    eprintln!("videocrm: couldn't copy {} into {}: {error}", source.display(), dir.display());
                    format!(
                        "Couldn’t copy {}. Check that the disk has room and the file can be read, then add it again.",
                        file_name(source)
                    )
                })
            });
            report(ImportMessage::Done(result));
        });
        cx.spawn(async move |this, cx| {
            let _copy = copy;
            while let Some(message) = receiver.next().await {
                let applied = this.update(cx, |this, cx| {
                    match message {
                        ImportMessage::File { ix, name } => {
                            if let Some(import) = this.running.get_mut(&video_id) {
                                import.file_ix = ix;
                                import.name = name;
                            }
                        }
                        ImportMessage::Copied(bytes) => {
                            if let Some(import) = this.running.get_mut(&video_id) {
                                import.copied += bytes;
                            }
                        }
                        ImportMessage::Done(result) => {
                            this.running.remove(&video_id);
                            if let Err(error) = result {
                                this.errors.insert(video_id, error.into());
                            }
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
}

/// Lists, probes and imports the raw takes video-use edits.
pub(crate) struct FootageCard {
    video_id: VideoId,
    state: Entity<AppState>,
    imports: Entity<FootageImports>,
    takes: Vec<Take>,
    loaded: bool,
    /// ffprobe results by path, valid while the file's modification time matches.
    probes: HashMap<PathBuf, (Option<DateTime<Utc>>, Option<MediaInfo>)>,
    /// A copy into the folder was running when the card last looked.
    importing: bool,
    /// What went wrong adding footage, before a copy started.
    error: Option<SharedString>,
    /// The system file chooser could not open; the card shows how to add takes
    /// without it until some arrive.
    chooser_failed: bool,
    /// Why the folder can't be read; cleared once it can.
    list_error: Option<SharedString>,
    /// A listing (and the probes it started) is in flight.
    refreshing: bool,
    refresh_task: Option<Task<()>>,
    _poll: Task<()>,
    _subscription: Subscription,
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
        let imports = PipelineService::global(cx).read(cx).footage_imports().clone();
        let subscription = cx.observe(&imports, |this: &mut Self, _, cx| this.follow_import(cx));
        let mut card = Self {
            video_id,
            state: AppState::global(cx),
            importing: imports.read(cx).running(video_id).is_some(),
            imports,
            takes: Vec::new(),
            loaded: false,
            probes: HashMap::new(),
            error: None,
            chooser_failed: false,
            list_error: None,
            refreshing: false,
            refresh_task: None,
            _poll: poll,
            _subscription: subscription,
        };
        let footage_dir = card.footage_dir(cx);
        card.imports.update(cx, |imports, cx| imports.sweep(video_id, footage_dir, cx));
        card.refresh(cx);
        card
    }

    /// Lists the folder again once a copy into it ends.
    fn follow_import(&mut self, cx: &mut Context<Self>) {
        let importing = self.imports.read(cx).running(self.video_id).is_some();
        if importing != self.importing {
            self.importing = importing;
            if !importing {
                self.refresh(cx);
            }
            cx.emit(FootageChanged);
        }
        cx.notify();
    }

    /// Whether the folder holds at least one video or audio take.
    pub(crate) fn has_takes(&self) -> bool {
        self.takes.iter().any(|take| matches!(take.file.kind, MediaKind::Video | MediaKind::Audio))
    }

    pub(crate) fn is_loaded(&self) -> bool {
        self.loaded
    }

    /// Files are being copied in; the folder holds partial takes until they finish.
    pub(crate) fn is_importing(&self) -> bool {
        self.importing
    }

    fn footage_dir(&self, cx: &App) -> Option<PathBuf> {
        self.state.read(cx).video(self.video_id).and_then(media::footage_dir)
    }

    /// Lists the folder again on a background thread and probes new or changed takes.
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.refreshing {
            return;
        }
        let Some(dir) = self.footage_dir(cx) else {
            self.set_files(Vec::new(), cx);
            return;
        };
        self.refreshing = true;
        self.refresh_task = Some(cx.spawn(async move |this, cx| {
            let listed = cx.background_spawn(async move { media::list_footage(&dir) }).await;
            let Ok(to_probe) = this.update(cx, |this, cx| match listed {
                Ok(files) => {
                    if this.list_error.take().is_some() {
                        cx.notify();
                    }
                    this.set_files(files, cx)
                }
                Err(error) => {
                    this.list_error = Some(error.to_string().into());
                    cx.notify();
                    Vec::new()
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
            _ = this.update(cx, |this, _| this.refreshing = false);
        }));
    }

    /// Takes the new listing; returns the takes that still need probing.
    fn set_files(&mut self, files: Vec<MediaFile>, cx: &mut Context<Self>) -> Vec<(PathBuf, Option<DateTime<Utc>>)> {
        let changed = !self.loaded
            || files.len() != self.takes.len()
            || files.iter().zip(&self.takes).any(|(file, take)| *file != take.file);
        if self.loaded && files.len() > self.takes.len() {
            // Takes arrived without the chooser (copied into the folder or dropped).
            self.chooser_failed = false;
        }
        self.loaded = true;
        let mut to_probe = Vec::new();
        self.takes = files
            .into_iter()
            .map(|file| {
                let (info, probed) = match self.probes.get(&file.path) {
                    Some((modified, info)) if *modified == file.modified => (info.clone(), true),
                    _ if file.kind == MediaKind::Image => (None, true),
                    _ => {
                        to_probe.push((file.path.clone(), file.modified));
                        (None, false)
                    }
                };
                Take { file, info, probed }
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
            take.probed = true;
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
            let Ok(picked) = picked.await else { return };
            _ = this.update(cx, |this, cx| this.on_picked(picked, cx));
        })
        .detach();
    }

    /// Imports what the file chooser returned, or explains how to add takes without it
    /// when it could not open (no desktop portal, for example).
    fn on_picked(&mut self, picked: gpui_kit::Result<Option<Vec<PathBuf>>>, cx: &mut Context<Self>) {
        match picked {
            Ok(Some(paths)) => self.import(paths, cx),
            Ok(None) => {}
            Err(error) => {
                eprintln!("videocrm: file chooser failed: {error}");
                // The hint names the folder to copy takes into, so it has to exist.
                self.state.update(cx, |state, cx| state.ensure_video_folder(self.video_id, cx));
                self.chooser_failed = true;
                self.refresh(cx);
                cx.notify();
            }
        }
    }

    /// Copies media files into the footage folder on a background thread. Other files
    /// are skipped; existing names get a numbered suffix.
    pub(crate) fn import(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if self.importing {
            return;
        }
        self.chooser_failed = false;
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
            self.error = Some("Couldn’t create the video’s media folder. Check the media folder in Settings.".into());
            cx.notify();
            return;
        };
        self.error = (!skipped.is_empty()).then(|| {
            let names: Vec<String> = skipped.iter().map(|path| file_name(path)).collect();
            format!("Skipped {}: not a video, audio or image file.", names.join(", ")).into()
        });
        let video_id = self.video_id;
        self.imports.update(cx, |imports, cx| imports.start(video_id, media, dir, cx));
        cx.notify();
    }

    fn copy_folder_path(&mut self, cx: &mut Context<Self>) {
        if let Some(dir) = self.footage_dir(cx) {
            cx.write_to_clipboard(ClipboardItem::new_string(dir.display().to_string()));
        }
    }

    fn open_folder(&mut self, cx: &mut Context<Self>) {
        let folder = self.state.update(cx, |state, cx| state.ensure_video_folder(self.video_id, cx));
        if let Some(folder) = folder {
            cx.open_with_system(&folder.join("footage"));
        }
    }

    fn render_header(&self, cx: &mut Context<Self>) -> SectionHeader {
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
        SectionHeader::new("Footage")
            .description(summary)
            .action(
                Button::new("footage-open-folder")
                    .ghost()
                    .small()
                    .icon(IconName::FolderOpen)
                    .label("Open folder")
                    .on_click(cx.listener(|this, _, _, cx| this.open_folder(cx))),
            )
            .action(
                Button::new("footage-add")
                    .outline()
                    .small()
                    .icon(IconName::Plus)
                    .label("Add footage…")
                    .disabled(self.importing)
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
                (None, _) if take.probed => "Unreadable".to_owned(),
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
                        .when(!take.probed, |this| this.child(Spinner::new().xsmall().color(theme.muted_foreground)))
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

    /// The empty drop zone; "Add footage…" is right above it in the header.
    fn render_empty(&self) -> EmptyState {
        EmptyState::new(
            "footage-empty",
            IconName::Clapperboard,
            "No footage yet",
            "Drop your raw takes here, or add them from disk. The AI edit cuts them into one video.",
        )
    }

    /// How to add takes when the file chooser can't open.
    fn render_chooser_failed(&self, cx: &mut Context<Self>) -> AnyElement {
        let folder = self.footage_dir(cx).map(|dir| display_user_path(&dir)).unwrap_or_default();
        let theme = cx.theme();
        h_flex()
            .id("footage-chooser-failed")
            .test_support()
            .items_start()
            .gap_3()
            .px_3()
            .py_2p5()
            .rounded(theme.radius_lg)
            .bg(theme.muted)
            .child(div().pt_0p5().child(Icon::new(IconName::Info).small().text_color(theme.muted_foreground)))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Couldn’t open the file chooser"))
                    .child(
                        div().text_sm().text_color(theme.muted_foreground).child(
                            "Drop your takes on the footage list, or copy them into the video’s footage folder:",
                        ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.foreground)
                            .truncate()
                            .child(folder),
                    )
                    .child(
                        h_flex()
                            .pt_1()
                            .gap_2()
                            .child(
                                Button::new("footage-copy-path")
                                    .outline()
                                    .small()
                                    .icon(IconName::Copy)
                                    .label("Copy path")
                                    .on_click(cx.listener(|this, _, _, cx| this.copy_folder_path(cx))),
                            )
                            .child(
                                Button::new("footage-open-folder-instead")
                                    .outline()
                                    .small()
                                    .icon(IconName::FolderOpen)
                                    .label("Open footage folder")
                                    .on_click(cx.listener(|this, _, _, cx| this.open_folder(cx))),
                            ),
                    ),
            )
            .child(
                Button::new("footage-chooser-dismiss").ghost().xsmall().icon(IconName::X).tooltip("Dismiss").on_click(
                    cx.listener(|this, _, _, cx| {
                        this.chooser_failed = false;
                        cx.notify();
                    }),
                ),
            )
            .into_any_element()
    }

    fn render_import(&self, import: &Import, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        let value =
            if import.total_bytes == 0 { 0.0 } else { import.copied as f32 / import.total_bytes as f32 * 100.0 };
        v_flex()
            .id("footage-import")
            .test_support()
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
        let body = if self.takes.is_empty() && self.loaded {
            self.render_empty().into_any_element()
        } else {
            self.render_list(cx).into_any_element()
        };
        let chooser_failed = self.chooser_failed.then(|| self.render_chooser_failed(cx));
        let import = self.imports.read(cx).running(self.video_id).cloned();
        let error = self.error.clone().or_else(|| self.imports.read(cx).error(self.video_id).cloned());
        let dismiss_error = error.is_some().then(|| {
            Button::new("footage-dismiss-error").ghost().xsmall().icon(IconName::X).tooltip("Dismiss").on_click(
                cx.listener(|this, _, _, cx| {
                    this.error = None;
                    let video_id = this.video_id;
                    this.imports.update(cx, |imports, cx| imports.dismiss_error(video_id, cx));
                    cx.notify();
                }),
            )
        });
        let theme = cx.theme();
        let drop_border = theme.primary;
        let drop_fill = theme.drop_target;

        v_flex()
            .gap_3()
            .child(header)
            .child(
                v_flex()
                    .id("footage-drop")
                    .test_support()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .overflow_hidden()
                    .when(self.takes.is_empty(), |this| this.border_dashed())
                    .drag_over::<ExternalPaths>(move |style, _, _, _| style.border_color(drop_border).bg(drop_fill))
                    .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| this.import(paths.paths().to_vec(), cx)))
                    .when_some(import.as_ref(), |this, import| this.child(self.render_import(import, cx)))
                    .child(body),
            )
            .children(chooser_failed)
            .children(self.list_error.clone().map(|error| div().text_sm().text_color(theme.danger).child(error)))
            .children(error.map(|error| {
                h_flex()
                    .gap_2()
                    .child(div().flex_1().min_w_0().text_sm().text_color(theme.danger).child(error))
                    .children(dismiss_error)
            }))
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned())
}

/// Copies `source` into `dir` under a free name, reporting copied bytes. The copy is
/// written to a hidden `.<name>.<n>.part` file of its own, listed in `partials` while
/// it is written, and gets its name only once complete: a copy cut short (the app
/// quit) never shows up as a shorter take, and two copies of same-named files never
/// share a file. A failed copy is removed.
fn copy_into(
    source: &Path,
    dir: &Path,
    partials: &Mutex<HashSet<PathBuf>>,
    progress: &mut dyn FnMut(u64),
) -> io::Result<PathBuf> {
    if source.parent() == Some(dir) {
        return Ok(source.to_owned());
    }
    let (partial, mut out) = create_partial(source, dir, partials)?;
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
        out.sync_all()?;
        drop(out);
        name_free(&partial, dir, source)
    })();
    if result.is_err() {
        _ = fs::remove_file(&partial);
    }
    lock(partials).remove(&partial);
    result
}

/// A new hidden file in `dir` to copy `source` into, with its path, listed in
/// `partials` before it exists so a sweep never takes it for a leftover.
fn create_partial(source: &Path, dir: &Path, partials: &Mutex<HashSet<PathBuf>>) -> io::Result<(PathBuf, File)> {
    for n in 1.. {
        let partial = dir.join(format!(".{}.{n}{PARTIAL_SUFFIX}", file_name(source)));
        if !lock(partials).insert(partial.clone()) {
            continue;
        }
        match File::create_new(&partial) {
            Ok(file) => return Ok((partial, file)),
            // Another copy's, or one an earlier session left.
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                lock(partials).remove(&partial);
            }
            Err(error) => {
                lock(partials).remove(&partial);
                return Err(error);
            }
        }
    }
    unreachable!("an unbounded range always yields a free name")
}

fn lock(partials: &Mutex<HashSet<PathBuf>>) -> std::sync::MutexGuard<'_, HashSet<PathBuf>> {
    partials.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Gives the finished copy `partial` the name `dir/<source name>`, or `name (2).ext`,
/// `name (3).ext`, ... when taken; never replaces a take.
fn name_free(partial: &Path, dir: &Path, source: &Path) -> io::Result<PathBuf> {
    let stem = source.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
    let extension = source.extension().map(|ext| format!(".{}", ext.to_string_lossy())).unwrap_or_default();
    for n in 1.. {
        let name = if n == 1 { format!("{stem}{extension}") } else { format!("{stem} ({n}){extension}") };
        let dest = dir.join(name);
        // A hard link fails instead of replacing an existing file.
        match fs::hard_link(partial, &dest) {
            Ok(()) => {
                fs::remove_file(partial)?;
                return Ok(dest);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            // A file system without hard links (exFAT, for example).
            Err(_) if !dest.exists() => {
                fs::rename(partial, &dest)?;
                return Ok(dest);
            }
            Err(_) => continue,
        }
    }
    unreachable!("an unbounded range always yields a free name")
}

/// Removes the hidden `.part` files of unfinished copies from `dir`, except those of
/// copies running now (`partials`).
fn remove_partial_copies(dir: &Path, partials: &Mutex<HashSet<PathBuf>>) {
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let (name, path) = (entry.file_name().to_string_lossy().into_owned(), entry.path());
        if name.starts_with('.') && name.ends_with(PARTIAL_SUFFIX) && path.is_file() && !lock(partials).contains(&path)
        {
            _ = fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{Bounds, TestAppContext, WindowBounds, WindowOptions, px, size};
    use videocrm_core::{NewVideo, Store};

    use super::*;

    #[gpui_kit::test]
    fn without_a_file_chooser_the_card_explains_how_to_add_takes(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let media = tempfile::tempdir().unwrap();
        let state = cx.new(|_| AppState::new(Store::open_in_memory().unwrap()).unwrap());
        let video = cx.update(|cx| {
            AppState::set_global(state.clone(), cx);
            PipelineService::init(state.clone(), cx);
            state.update(cx, |state, cx| {
                state.update_settings(cx, |settings| settings.media_root = Some(media.path().to_owned()));
                state.create_video(NewVideo { title: "Desk setup".into(), ..NewVideo::default() }, cx).unwrap()
            })
        });
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(Default::default(), size(px(900.), px(700.))))),
            ..WindowOptions::default()
        };
        let (window, card) = cx
            .update(|cx| gpui_kit::open_window(options, cx, |_, cx| cx.new(|cx| FootageCard::new(video, cx))))
            .unwrap();
        let footage = cx.read(|cx| card.read(cx).footage_dir(cx)).expect("footage folder");
        cx.run_until_parked();
        assert!(cx.read(|cx| card.read(cx).is_loaded()));
        let shows = |cx: &mut TestAppContext, id: &str| {
            let id = gpui_kit::ElementId::Name(id.to_owned().into());
            cx.update_window(window, move |_, window, cx| {
                window.render_frame(cx);
                window.try_find(id).is_some_and(|element| element.visible())
            })
            .unwrap()
        };

        cx.update(|cx| {
            card.update(cx, |card, cx| card.on_picked(Err(io::Error::other("ZBus Error: no bus").into()), cx))
        });
        assert!(shows(cx, "footage-chooser-failed"));
        assert_eq!(cx.read(|cx| card.read(cx).error.clone()), None, "the raw error is only logged");
        cx.update_window(window, |_, window, cx| window.click("footage-copy-path", cx)).unwrap();
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(footage.display().to_string()),
            "the folder path can be pasted into a file manager"
        );

        // Takes copied in by hand: the notice has done its job.
        fs::create_dir_all(&footage).unwrap();
        fs::write(footage.join("take1.mp4"), b"x").unwrap();
        cx.update(|cx| card.update(cx, |card, cx| card.refresh(cx)));
        cx.run_until_parked();
        assert!(!shows(cx, "footage-chooser-failed"));
    }

    #[gpui_kit::test]
    fn a_card_shown_again_follows_the_copy_an_earlier_one_started(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let media = tempfile::tempdir().unwrap();
        // A video without a media folder yet, like a sample video.
        let mut store = Store::open_in_memory().unwrap();
        let video = videocrm_core::Video::new("Sample");
        store.insert_video(&video).unwrap();
        let state = cx.new(|_| AppState::new(store).unwrap());
        cx.update(|cx| {
            AppState::set_global(state.clone(), cx);
            PipelineService::init(state.clone(), cx);
            state.update(cx, |state, cx| {
                state.update_settings(cx, |settings| settings.media_root = Some(media.path().to_owned()))
            });
        });
        let take = media.path().join("C0001.MP4");
        fs::write(&take, vec![9u8; 3 * COPY_CHUNK]).unwrap();
        let options = || WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(Default::default(), size(px(900.), px(700.))))),
            ..WindowOptions::default()
        };
        let open = |cx: &mut TestAppContext| {
            cx.update(|cx| gpui_kit::open_window(options(), cx, |_, cx| cx.new(|cx| FootageCard::new(video.id, cx))))
                .unwrap()
        };

        let (_, first) = open(cx);
        cx.update(|cx| first.update(cx, |card, cx| card.import(vec![take.clone()], cx)));
        let footage = cx.read(|cx| first.read(cx).footage_dir(cx)).expect("the copy created the folder");

        // Another video and back: a new card, while the copy has not finished.
        let (window, second) = open(cx);
        assert!(cx.read(|cx| second.read(cx).is_importing()), "the new card shows the running copy");
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("footage-import").visible());
        })
        .unwrap();

        cx.run_until_parked();
        assert_eq!(fs::read(footage.join("C0001.MP4")).unwrap().len(), 3 * COPY_CHUNK, "the take arrived whole");
        assert!(!cx.read(|cx| second.read(cx).is_importing()));
        assert!(cx.read(|cx| second.read(cx).has_takes()), "the card lists it");
    }

    #[test]
    fn the_agent_is_told_about_every_take_the_card_lists() {
        let listed: HashSet<&str> = media::VIDEO_EXTENSIONS.iter().chain(media::AUDIO_EXTENSIONS).copied().collect();
        let prompted: HashSet<&str> = videocrm_pipeline::FOOTAGE_EXTENSIONS.into_iter().collect();
        assert_eq!(listed, prompted);
    }

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

        let partials = Mutex::default();
        let mut copied = 0;
        let first = copy_into(&source, &footage, &partials, &mut |bytes| copied += bytes).unwrap();
        copy_into(&source, &footage, &partials, &mut |_| {}).unwrap();
        assert_eq!(copied, COPY_CHUNK as u64 + 10);
        assert_eq!(first, footage.join("take1 (2).mp4"));
        assert_eq!(fs::read(footage.join("take1.mp4")).unwrap(), b"existing");
        assert_eq!(fs::read(footage.join("take1 (2).mp4")).unwrap().len(), COPY_CHUNK + 10);
        assert!(footage.join("take1 (3).mp4").is_file());

        // A file already in the folder is left alone.
        copy_into(&footage.join("take1.mp4"), &footage, &partials, &mut |_| {}).unwrap();
        assert_eq!(fs::read_dir(&footage).unwrap().count(), 3, "no partial copies are left behind");
    }

    #[test]
    fn copies_are_written_under_a_hidden_name_until_complete() {
        let root = tempfile::tempdir().unwrap();
        let footage = root.path().join("footage");
        fs::create_dir_all(&footage).unwrap();
        let source = root.path().join("take2.mov");
        fs::write(&source, vec![1u8; 3 * COPY_CHUNK]).unwrap();

        let partials = Mutex::default();
        let mut seen = Vec::new();
        copy_into(&source, &footage, &partials, &mut |_| {
            seen.push(fs::read_dir(&footage).unwrap().map(|entry| entry.unwrap().file_name()).collect::<Vec<_>>());
            // A sweep while it copies leaves the copy alone.
            remove_partial_copies(&footage, &partials);
        })
        .unwrap();
        // While copying, the folder holds only the hidden partial file.
        assert!(seen.iter().all(|names| names == &[".take2.mov.1.part"]), "{seen:?}");
        assert!(footage.join("take2.mov").is_file() && !footage.join(".take2.mov.1.part").exists());
        assert!(lock(&partials).is_empty());

        // A copy an earlier session left unfinished is removed; takes stay.
        fs::write(footage.join(".take3.mov.1.part"), b"half").unwrap();
        remove_partial_copies(&footage, &partials);
        let names: Vec<_> = fs::read_dir(&footage).unwrap().map(|entry| entry.unwrap().file_name()).collect();
        assert_eq!(names, ["take2.mov"]);
    }

    #[test]
    fn copies_of_same_named_takes_never_share_a_file() {
        let root = tempfile::tempdir().unwrap();
        let footage = root.path().join("footage");
        let (card_a, card_b) = (root.path().join("a"), root.path().join("b"));
        for dir in [&footage, &card_a, &card_b] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(card_a.join("C0001.MP4"), vec![b'a'; 3 * COPY_CHUNK]).unwrap();
        fs::write(card_b.join("C0001.MP4"), vec![b'b'; 2 * COPY_CHUNK]).unwrap();
        // A leftover of an earlier session with the first name a copy would take.
        fs::write(footage.join(".C0001.MP4.1.part"), b"old").unwrap();

        // Card B's copy starts while card A's is half way.
        let partials = Mutex::default();
        let mut second = None;
        let first = copy_into(&card_a.join("C0001.MP4"), &footage, &partials, &mut |_| {
            if second.is_none() {
                second = Some(copy_into(&card_b.join("C0001.MP4"), &footage, &partials, &mut |_| {}).unwrap());
            }
        })
        .unwrap();
        let second = second.unwrap();
        assert_eq!((second, first), (footage.join("C0001.MP4"), footage.join("C0001 (2).MP4")));
        assert_eq!(fs::read(footage.join("C0001.MP4")).unwrap(), vec![b'b'; 2 * COPY_CHUNK]);
        assert_eq!(fs::read(footage.join("C0001 (2).MP4")).unwrap(), vec![b'a'; 3 * COPY_CHUNK]);
        assert_eq!(fs::read(footage.join(".C0001.MP4.1.part")).unwrap(), b"old", "not overwritten");
    }
}
