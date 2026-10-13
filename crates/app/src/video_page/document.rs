//! A video's Script or Notes document in the block editor, saved shortly after
//! the last edit, plus the Script tab's rail: stats, Markdown and outline.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::menu::DropdownMenu as _;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, App, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, InteractiveElement as _,
    IntoElement, ParentElement as _, PathPromptOptions, Rems, Render, ScrollHandle, SharedString, Styled as _,
    Subscription, Task, TestSupportExt as _, Window, div,
};
use rushes_core::document::{
    DEFAULT_WPM, OutlineItem, estimated_runtime_secs, from_markdown, outline, spoken_word_count, teleprompter_text,
    to_markdown,
};
use rushes_core::input::display_user_path;
use rushes_core::{Block, BlockId, BlockKind, DocumentId, DocumentKind, VideoId, media};

use super::saves::DocumentSaves;
use crate::database::format;
use crate::editor::{DocumentEditor, DocumentEditorEvent};
use crate::state::AppState;
use crate::ui::path_prompt::{self, Finish, PathKind, PathRequest, read_error_reason, write_error_reason};
use crate::ui::{EmptyState, RowButton, section_label};

/// How long after the last edit a document is written.
pub const SAVE_DELAY: Duration = Duration::from_millis(600);

/// Derived facts about a script, recomputed on every edit.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScriptStats {
    pub words: usize,
    pub seconds: u32,
    pub broll_cues: usize,
    pub onscreen_cues: usize,
    pub outline: Vec<OutlineItem>,
}

impl ScriptStats {
    pub fn of(blocks: &[Block]) -> Self {
        let cues = |kind| blocks.iter().filter(|block| block.kind == kind && !block.text.trim().is_empty()).count();
        Self {
            words: spoken_word_count(blocks),
            seconds: estimated_runtime_secs(blocks, DEFAULT_WPM),
            broll_cues: cues(BlockKind::BRoll),
            onscreen_cues: cues(BlockKind::OnScreen),
            outline: outline(blocks),
        }
    }

    /// Spoken words with thousands grouped, as the database's Script column shows them.
    pub fn words_label(&self) -> String {
        format::group_thousands(self.words as u64)
    }

    /// Estimated runtime; an empty script takes no time, rather than about none.
    pub fn runtime_label(&self) -> String {
        match self.seconds {
            0 => format::runtime(0),
            seconds => format!("~{}", format::runtime(seconds)),
        }
    }

    /// One line for narrow windows, e.g. "1,240 words · ~8m 16s".
    pub fn summary(&self) -> String {
        match self.words {
            0 => "No spoken words yet".to_string(),
            1 => format!("1 word · {}", self.runtime_label()),
            _ => format!("{} words · {}", self.words_label(), self.runtime_label()),
        }
    }
}

type TabCommand = fn(&mut DocumentTab, &mut Window, &mut Context<DocumentTab>);

/// The script's text commands, shown in the rail and, in narrow windows, in
/// the script menu next to the tabs.
const SCRIPT_COMMANDS: [(&str, IconName, &str, TabCommand); 3] = [
    ("copy-teleprompter", IconName::Copy, "Copy teleprompter text", DocumentTab::copy_teleprompter_text),
    ("export-markdown", IconName::FileDown, "Export Markdown…", DocumentTab::export_markdown),
    ("import-markdown", IconName::FileUp, "Import Markdown…", DocumentTab::import_markdown),
];

/// One document of a video in a [`DocumentEditor`]. Edits are handed to
/// [`DocumentSaves`] [`SAVE_DELAY`] after the last change, or at once by
/// [`Self::flush`].
pub struct DocumentTab {
    state: Entity<AppState>,
    saves: Entity<DocumentSaves>,
    video_id: VideoId,
    kind: DocumentKind,
    document_id: Option<DocumentId>,
    scroll: ScrollHandle,
    editor: Option<Entity<DocumentEditor>>,
    /// A Tab stop in front of the editor: Tab from the controls above puts
    /// the caret in the first block.
    entry: FocusHandle,
    stats: ScriptStats,
    /// Edits not yet handed to the save queue.
    dirty: bool,
    save: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

/// Emitted when the document's derived stats change.
pub struct StatsChanged;

impl EventEmitter<StatsChanged> for DocumentTab {}

impl DocumentTab {
    /// Loads the video's document of `kind`, creating it when missing. `scroll`
    /// is the container the editor scrolls in, so the caret stays visible.
    pub fn new(
        video_id: VideoId,
        kind: DocumentKind,
        scroll: ScrollHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let saves = DocumentSaves::global(cx);
        let entry = cx.focus_handle().tab_stop(true);
        let mut tab = Self {
            state: AppState::global(cx),
            saves,
            video_id,
            kind,
            document_id: None,
            scroll,
            editor: None,
            entry,
            stats: ScriptStats::default(),
            dirty: false,
            save: None,
            _subscriptions: Vec::new(),
        };
        tab.load(window, cx);
        tab
    }

    /// (Re)loads the document; a failure leaves the tab in its error state.
    /// An edit still waiting in the save queue wins over the stored text.
    pub fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (video_id, kind) = (self.video_id, self.kind);
        let Some(stored) = self.state.update(cx, |state, cx| state.ensure_document(video_id, kind, cx)) else {
            return;
        };
        let mut document = self.saves.read(cx).pending(stored.id).unwrap_or(stored);
        // The caret needs a block to go to.
        if document.blocks.is_empty() {
            document.blocks.push(Block::paragraph(""));
        }
        self.document_id = Some(document.id);
        self.stats = ScriptStats::of(&document.blocks);
        let scroll = self.scroll.clone();
        let editor = cx.new(|cx| {
            let mut editor = DocumentEditor::new(document, window, cx);
            editor.set_scroll_handle(scroll);
            editor
        });
        self._subscriptions = vec![
            cx.subscribe(&editor, |this, _, event: &DocumentEditorEvent, cx| {
                if *event == DocumentEditorEvent::Changed {
                    this.on_changed(cx);
                }
            }),
            cx.on_focus(&self.entry, window, |this, window, cx| this.focus_start(window, cx)),
            cx.observe(&self.saves, |_, _, cx| cx.notify()),
        ];
        self.editor = Some(editor);
        cx.notify();
    }

    pub fn editor(&self) -> Option<&Entity<DocumentEditor>> {
        self.editor.as_ref()
    }

    pub fn stats(&self) -> &ScriptStats {
        &self.stats
    }

    /// Whether the last attempt to save this document failed; it is retried.
    pub fn is_save_failing(&self, cx: &App) -> bool {
        self.document_id.is_some_and(|id| self.saves.read(cx).is_failing(id))
    }

    fn on_changed(&mut self, cx: &mut Context<Self>) {
        self.dirty = true;
        self.save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DELAY).await;
            _ = this.update(cx, |this, cx| this.flush(cx));
        }));
        if let Some(editor) = &self.editor {
            let stats = ScriptStats::of(&editor.read(cx).document(cx).blocks);
            if stats != self.stats {
                self.stats = stats;
                cx.emit(StatsChanged);
            }
        }
        cx.notify();
    }

    /// Hands pending edits to the save queue now, which writes them in the
    /// background. Edits of a video that was deleted meanwhile are dropped.
    pub fn flush(&mut self, cx: &mut Context<Self>) {
        self.save = None;
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let Some(editor) = &self.editor else { return };
        if self.state.read(cx).video(self.video_id).is_none() {
            return;
        }
        let document = editor.read(cx).document(cx);
        self.saves.update(cx, |saves, cx| saves.save(document, cx));
    }

    /// Puts the caret at the start of the document.
    pub fn focus_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editor) = &self.editor {
            editor.update(cx, |editor, cx| editor.focus_first(window, cx));
        }
    }

    /// Puts the caret on an empty line after the last block, as a click below
    /// the document does.
    pub fn focus_end(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editor) = &self.editor {
            editor.update(cx, |editor, cx| editor.focus_end(window, cx));
        }
    }

    /// Jumps to a heading of the outline.
    pub fn focus_block(&mut self, id: BlockId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editor) = &self.editor {
            editor.update(cx, |editor, cx| editor.focus_block_start(id, window, cx));
        }
    }

    fn blocks(&self, cx: &mut Context<Self>) -> Vec<Block> {
        self.editor.as_ref().map(|editor| editor.read(cx).document(cx).blocks).unwrap_or_default()
    }

    pub fn copy_teleprompter_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = teleprompter_text(&self.blocks(cx));
        if text.is_empty() {
            window.push_notification("The script has no spoken text yet.", cx);
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        window.push_notification("Teleprompter text copied.", cx);
    }

    pub fn export_markdown(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let markdown = to_markdown(&self.blocks(cx));
        let (directory, name) = self.export_location(cx);
        let path = cx.prompt_for_new_path(&directory, Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let path = match path.await {
                Ok(Ok(Some(path))) => path,
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(error)) => {
                    log_line!("rushes: save dialog failed: {error}");
                    _ = this.update_in(cx, |this, window, cx| this.prompt_for_path(PathPurpose::Export, window, cx));
                    return;
                }
            };
            _ = this.update_in(cx, |this, window, cx| this.write_markdown(path, markdown, window, cx));
        })
        .detach();
    }

    pub fn import_markdown(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let path = match paths.await {
                Ok(Ok(Some(paths))) => match paths.into_iter().next() {
                    Some(path) => path,
                    None => return,
                },
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(error)) => {
                    log_line!("rushes: file dialog failed: {error}");
                    _ = this.update_in(cx, |this, window, cx| this.prompt_for_path(PathPurpose::Import, window, cx));
                    return;
                }
            };
            _ = this.update_in(cx, |this, window, cx| this.read_markdown(path, window, cx));
        })
        .detach();
    }

    /// Writes the document as Markdown to the path the file chooser returned,
    /// off the UI thread, and says where it went.
    fn write_markdown(&mut self, path: PathBuf, markdown: String, window: &mut Window, cx: &mut Context<Self>) {
        let written = cx.background_spawn(write_file(path.clone(), markdown));
        cx.spawn_in(window, async move |this, cx| {
            let written = written.await;
            _ = this.update_in(cx, |this, window, cx| match written {
                Ok(()) => exported(&path, window, cx),
                Err(error) => this.report(write_error_reason(&path, &error).into(), cx),
            });
        })
        .detach();
    }

    /// Reads the Markdown file the file chooser returned off the UI thread, then
    /// asks how to import it.
    fn read_markdown(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let read = cx.background_spawn(read_file(path.clone()));
        cx.spawn_in(window, async move |this, cx| {
            let read = read.await;
            _ = this.update_in(cx, |this, window, cx| match read {
                Ok(markdown) => this.confirm_import(&path, from_markdown(&markdown), window, cx),
                Err(error) => this.report(read_error_reason(&path, &error).into(), cx),
            });
        })
        .detach();
    }

    /// Asks for a path when the system file chooser is unavailable. Export
    /// suggests a file in the video's media folder and asks before replacing a
    /// file; the file is written or read before the dialog closes, so a path
    /// that doesn't work is explained next to the field.
    pub fn prompt_for_path(&mut self, purpose: PathPurpose, window: &mut Window, cx: &mut Context<Self>) {
        let tab = cx.weak_entity();
        match purpose {
            PathPurpose::Export => {
                let request = PathRequest::new(
                    PathKind::NewFile,
                    "Type where to save the Markdown file instead.",
                    "~/Videos/Rushes/script.md",
                )
                .commit("Export")
                .value(display_user_path(&self.fallback_export_path(cx)));
                let markdown = to_markdown(&self.blocks(cx));
                path_prompt::open(request, window, cx, move |path, _, cx| {
                    let written = cx.background_spawn(write_file(path.clone(), markdown.clone()));
                    cx.foreground_executor().spawn(async move {
                        written.await.map_err(|error| write_error_reason(&path, &error))?;
                        Ok(Box::new(move |window: &mut Window, cx: &mut App| exported(&path, window, cx)) as Finish)
                    })
                });
            }
            PathPurpose::Import => {
                let request = PathRequest::new(
                    PathKind::File,
                    "Type the path of the Markdown file instead.",
                    "~/Videos/Rushes/script.md",
                )
                .commit("Import");
                path_prompt::open(request, window, cx, move |path, _, cx| {
                    let read = cx.background_spawn(read_file(path.clone()));
                    let tab = tab.clone();
                    cx.foreground_executor().spawn(async move {
                        let markdown = read.await.map_err(|error| read_error_reason(&path, &error))?;
                        Ok(Box::new(move |window: &mut Window, cx: &mut App| {
                            _ = tab
                                .update(cx, |tab, cx| tab.confirm_import(&path, from_markdown(&markdown), window, cx));
                        }) as Finish)
                    })
                });
            }
        }
    }

    /// Asks whether imported blocks go after the document or replace it.
    pub fn confirm_import(&mut self, path: &Path, blocks: Vec<Block>, window: &mut Window, cx: &mut Context<Self>) {
        let title: SharedString = format!("Import “{}”?", file_name(path)).into();
        let tab = cx.weak_entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let import = |replace: bool| {
                let (tab, blocks) = (tab.clone(), blocks.clone());
                move |window: &mut Window, cx: &mut App| {
                    _ = tab.update(cx, |tab, cx| tab.apply_import(blocks.clone(), replace, window, cx));
                }
            };
            let (append, append_on_enter, replace) = (import(false), import(false), import(true));
            alert
                .title(title.clone())
                .description(
                    "Append adds it after the last block. Replace removes the current text, which can’t be undone.",
                )
                .footer(
                    DialogFooter::new()
                        // The irreversible choice stands apart from the default one.
                        .child(
                            // Drawn like the path dialog's Replace and every Delete.
                            Button::new("import-replace").danger().label("Replace").on_click(move |_, window, cx| {
                                replace(window, cx);
                                window.close_dialog(cx);
                            }),
                        )
                        .child(div().flex_1())
                        .child(
                            Button::new("import-cancel")
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(Button::new("import-append").primary().label("Append").on_click(
                            move |_, window, cx| {
                                append(window, cx);
                                window.close_dialog(cx);
                            },
                        )),
                )
                .on_ok(move |_, window, cx| {
                    append_on_enter(window, cx);
                    true
                })
        });
    }

    /// Appends `blocks` to the document or replaces it with them, and saves.
    pub fn apply_import(&mut self, blocks: Vec<Block>, replace: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.clone() else { return };
        let mut document = editor.read(cx).document(cx);
        if replace {
            document.blocks = blocks;
        } else {
            let trailing_empty =
                document.blocks.last().is_some_and(|block| block.kind == BlockKind::Paragraph && block.text.is_empty());
            if trailing_empty {
                document.blocks.pop();
            }
            document.blocks.extend(blocks);
        }
        if document.blocks.is_empty() {
            document.blocks.push(Block::paragraph(""));
        }
        self.stats = ScriptStats::of(&document.blocks);
        editor.update(cx, |editor, cx| editor.set_document(document, window, cx));
        self.dirty = true;
        self.flush(cx);
        cx.emit(StatsChanged);
        cx.notify();
    }

    fn export_location(&self, cx: &Context<Self>) -> (PathBuf, String) {
        let state = self.state.read(cx);
        let video = state.video(self.video_id);
        let directory = video
            .and_then(|video| video.folder.clone())
            .filter(|folder| folder.is_dir())
            .or_else(dirs_home)
            .unwrap_or_else(|| PathBuf::from("."));
        let title = video.map(|video| video.title.trim()).filter(|title| !title.is_empty()).unwrap_or("Untitled");
        (directory, format!("{} – {}.md", sanitize_file_name(title), self.kind_name()))
    }

    /// `<media folder>/<slug>.md`, offered when no file chooser is available.
    fn fallback_export_path(&self, cx: &Context<Self>) -> PathBuf {
        let state = self.state.read(cx);
        let Some(video) = state.video(self.video_id) else {
            return dirs_home().unwrap_or_default().join(format!("{}.md", self.kind_name()));
        };
        let name = media::video_folder_name(video);
        let folder = video.folder.clone().unwrap_or_else(|| state.settings().resolved_media_root().join(&name));
        let slug = name.strip_suffix(&format!("-{}", video.id.short())).unwrap_or(&name);
        let file = match self.kind {
            DocumentKind::Script => format!("{slug}.md"),
            _ => format!("{slug}-{}.md", self.kind_name()),
        };
        folder.join(file)
    }

    fn kind_name(&self) -> &'static str {
        match self.kind {
            DocumentKind::Script => "script",
            DocumentKind::Notes => "notes",
            DocumentKind::Custom => "document",
        }
    }

    fn report(&self, message: String, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| state.report_error(message, cx));
    }
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string())
}

/// Writes `text` to `path`, creating its folder first: a typed path may name
/// one that doesn't exist yet, such as the video's media folder before its first use.
async fn write_file(path: PathBuf, text: String) -> std::io::Result<()> {
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)?;
    }
    std::fs::write(&path, text)
}

async fn read_file(path: PathBuf) -> std::io::Result<String> {
    std::fs::read_to_string(&path)
}

fn exported(path: &Path, window: &mut Window, cx: &mut App) {
    window.push_notification(format!("Exported to {}.", display_user_path(path)), cx);
}

/// Replaces characters that are not allowed in file names.
fn sanitize_file_name(name: &str) -> String {
    name.chars()
        .map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '-' } else { c })
        .collect()
}

impl Render for DocumentTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match &self.editor {
            Some(editor) => div()
                .w_full()
                .child(div().id("document-entry").size_0().track_focus(&self.entry))
                .child(editor.clone())
                .into_any_element(),
            None => div()
                .w_full()
                .child(
                    EmptyState::new(
                        "document-unavailable",
                        IconName::FileExclamationPoint,
                        "This document couldn’t be opened",
                        "The database didn’t return it. Try again, or restart Rushes if it keeps happening.",
                    )
                    .action(
                        Button::new("document-retry")
                            .outline()
                            .small()
                            .label("Try again")
                            .on_click(cx.listener(|this, _, window, cx| this.load(window, cx))),
                    ),
                )
                .into_any_element(),
        }
    }
}

/// What a typed path is for when the system file chooser is unavailable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathPurpose {
    Export,
    Import,
}

/// Adds the script's text commands and outline to `menu`, for narrow windows
/// where the rail is hidden.
pub fn script_menu(menu: PopupMenu, tab: &Entity<DocumentTab>, cx: &App) -> PopupMenu {
    let stats = tab.read(cx).stats().clone();
    let cues = format!("{} B-roll · {} on-screen cues", stats.broll_cues, stats.onscreen_cues);
    let menu = SCRIPT_COMMANDS.iter().fold(menu.label(cues).separator(), |menu, (_, icon, label, run)| {
        let (tab, run) = (tab.clone(), *run);
        menu.item(
            PopupMenuItem::new(*label)
                .icon(Icon::new(*icon))
                .on_click(move |_, window, cx| tab.update(cx, |tab, cx| run(tab, window, cx))),
        )
    });
    if stats.outline.is_empty() {
        return menu;
    }
    let top_level = stats.outline.iter().map(|item| item.level).min().unwrap_or(1);
    stats.outline.into_iter().fold(menu.separator().label("Outline"), |menu, item| {
        let (tab, id) = (tab.clone(), item.block_id);
        let indent = "   ".repeat(usize::from(item.level - top_level));
        menu.item(
            PopupMenuItem::new(format!("{indent}{}", item.text))
                .on_click(move |_, window, cx| tab.update(cx, |tab, cx| tab.focus_block(id, window, cx))),
        )
    })
}

/// The Script tab's rail: spoken length, cues, Markdown actions and outline.
/// Only the outline scrolls, so the actions stay put however long it gets.
pub struct ScriptRail {
    tab: Entity<DocumentTab>,
    _subscription: Subscription,
}

impl ScriptRail {
    pub fn new(tab: Entity<DocumentTab>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&tab, |_, _, _: &StatsChanged, cx| cx.notify());
        Self { tab, _subscription: subscription }
    }
}

impl Render for ScriptRail {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let stats = self.tab.read(cx).stats().clone();
        let stat = |value: String, label: &'static str, cx: &Context<Self>| {
            v_flex()
                .gap_0p5()
                .child(div().text_lg().font_semibold().child(value))
                .child(div().text_xs().text_color(cx.theme().muted_foreground).child(label))
        };
        let tab = self.tab.clone();
        let top_level = stats.outline.iter().map(|item| item.level).min().unwrap_or(1);
        let outline: Vec<RowButton> = stats
            .outline
            .iter()
            .map(|item| {
                let tab = tab.clone();
                let id = item.block_id;
                RowButton::new(SharedString::from(format!("outline-{id}")), item.text.clone())
                    .indent(Rems(0.75 * f32::from(item.level - top_level)))
                    .muted(item.level > top_level)
                    .on_click(move |_, window, cx| tab.update(cx, |tab, cx| tab.focus_block(id, window, cx)))
            })
            .collect();
        let actions = SCRIPT_COMMANDS.iter().map(|(id, icon, label, run)| {
            let (tab, run) = (self.tab.clone(), *run);
            RowButton::new(*id, *label)
                .icon(*icon)
                .on_click(move |_, window, cx| tab.update(cx, |tab, cx| run(tab, window, cx)))
        });

        v_flex()
            .id("script-rail")
            .test_support()
            .size_full()
            .pt_6()
            .gap_6()
            .child(
                v_flex()
                    .flex_none()
                    .px_4()
                    .gap_3()
                    .child(section_label("Script", cx))
                    .child(
                        div()
                            .grid()
                            .grid_cols(2)
                            .gap_4()
                            .child(stat(stats.words_label(), "spoken words", cx))
                            .child(stat(stats.runtime_label(), "runtime", cx))
                            .child(stat(stats.broll_cues.to_string(), "B-roll cues", cx))
                            .child(stat(stats.onscreen_cues.to_string(), "on-screen cues", cx)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("Runtime at {DEFAULT_WPM} words per minute.")),
                    ),
            )
            .child(
                v_flex()
                    .flex_none()
                    .px_4()
                    .gap_2()
                    .child(section_label("Script text", cx))
                    .child(v_flex().mx_neg_2().gap_1().children(actions)),
            )
            .child(v_flex().flex_1().min_h_0().gap_2().child(div().px_4().child(section_label("Outline", cx))).map(
                |this| {
                    if outline.is_empty() {
                        this.child(
                            div()
                                .px_4()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("Headings in the script show up here."),
                        )
                    } else {
                        this.child(
                            v_flex()
                                .id("script-outline")
                                .flex_1()
                                .min_h_0()
                                .px_2()
                                .pb_6()
                                .gap_1()
                                .children(outline)
                                .overflow_y_scrollbar(),
                        )
                    }
                },
            ))
    }
}

/// A one-line summary of the script with its stats, commands and outline in a
/// menu: the rail's content where the window is too narrow for the rail.
pub fn script_menu_button(tab: &Entity<DocumentTab>, cx: &App) -> impl IntoElement {
    let summary = tab.read(cx).stats().summary();
    let tab = tab.clone();
    Button::new("script-menu")
        .ghost()
        .small()
        .label(summary)
        .dropdown_caret(true)
        .tooltip("Script stats, Markdown and outline")
        .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, window, cx| {
            script_menu(menu.max_h(window.rem_size() * 28.).scrollable(true), &tab, cx)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_read_like_the_database() {
        let stats = ScriptStats { words: 4177, seconds: 1671, ..ScriptStats::default() };
        assert_eq!(stats.words_label(), "4,177");
        assert_eq!(stats.runtime_label(), "~27m 51s");
        assert_eq!(stats.summary(), "4,177 words · ~27m 51s");

        let empty = ScriptStats::default();
        assert_eq!(empty.runtime_label(), "0s");
        assert_eq!(empty.summary(), "No spoken words yet");
    }
}
