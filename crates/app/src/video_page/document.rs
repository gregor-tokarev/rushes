//! A video's Script or Notes document in the block editor, saved shortly after
//! the last edit, plus the Script tab's rail: stats, outline and Markdown.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, StyledExt as _, WindowExt as _, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, ClipboardItem, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement, ParentElement as _, PathPromptOptions,
    Render, Rems, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window,
    div,
};
use videocrm_core::document::{
    DEFAULT_WPM, OutlineItem, estimated_runtime_secs, format_runtime, from_markdown, outline, spoken_word_count,
    teleprompter_text, to_markdown,
};
use videocrm_core::{Block, BlockId, BlockKind, DocumentKind, VideoId};

use crate::editor::{DocumentEditor, DocumentEditorEvent};
use crate::shell::elements::{RowButton, empty_state, section_label};
use crate::state::AppState;

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
}

/// One document of a video in a [`DocumentEditor`]. Edits are saved
/// [`SAVE_DELAY`] after the last change, or at once by [`Self::flush`].
pub struct DocumentTab {
    state: Entity<AppState>,
    video_id: VideoId,
    kind: DocumentKind,
    scroll: ScrollHandle,
    editor: Option<Entity<DocumentEditor>>,
    stats: ScriptStats,
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
        let mut tab = Self {
            state: AppState::global(cx),
            video_id,
            kind,
            scroll,
            editor: None,
            stats: ScriptStats::default(),
            dirty: false,
            save: None,
            _subscriptions: Vec::new(),
        };
        tab.load(window, cx);
        tab
    }

    /// (Re)loads the document; a failure leaves the tab in its error state.
    pub fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (video_id, kind) = (self.video_id, self.kind);
        let Some(document) = self.state.update(cx, |state, cx| state.ensure_document(video_id, kind, cx)) else {
            return;
        };
        self.stats = ScriptStats::of(&document.blocks);
        let scroll = self.scroll.clone();
        let editor = cx.new(|cx| {
            let mut editor = DocumentEditor::new(document, window, cx);
            editor.set_scroll_handle(scroll);
            editor
        });
        self._subscriptions = vec![cx.subscribe(&editor, |this, _, event: &DocumentEditorEvent, cx| {
            if *event == DocumentEditorEvent::Changed {
                this.on_changed(cx);
            }
        })];
        self.editor = Some(editor);
        cx.notify();
    }

    pub fn editor(&self) -> Option<&Entity<DocumentEditor>> {
        self.editor.as_ref()
    }

    pub fn stats(&self) -> &ScriptStats {
        &self.stats
    }

    /// Whether edits are waiting to be saved.
    pub fn is_dirty(&self) -> bool {
        self.dirty
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

    /// Saves pending edits now.
    pub fn flush(&mut self, cx: &mut Context<Self>) {
        self.save = None;
        if !self.dirty {
            return;
        }
        let Some(editor) = &self.editor else { return };
        self.dirty = false;
        let document = editor.read(cx).document(cx);
        self.state.update(cx, |state, cx| state.save_document(document, cx));
    }

    /// Puts the caret at the start of the document.
    pub fn focus_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editor) = &self.editor {
            editor.update(cx, |editor, cx| editor.focus_first(window, cx));
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
                    _ = this.update(cx, |this, cx| this.report(format!("Couldn’t open the save dialog: {error}"), cx));
                    return;
                }
            };
            let target = path.clone();
            let written = cx.background_spawn(async move { std::fs::write(&target, markdown) }).await;
            _ = this.update_in(cx, |this, window, cx| match written {
                Ok(()) => window.push_notification(format!("Exported to {}.", file_name(&path)), cx),
                Err(error) => this.report(format!("Couldn’t write {}: {error}", path.display()), cx),
            });
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
                    _ = this.update(cx, |this, cx| this.report(format!("Couldn’t open the file dialog: {error}"), cx));
                    return;
                }
            };
            let source = path.clone();
            let read = cx.background_spawn(async move { std::fs::read_to_string(&source) }).await;
            _ = this.update_in(cx, |this, window, cx| match read {
                Ok(markdown) => this.confirm_import(&path, from_markdown(&markdown), window, cx),
                Err(error) => this.report(format!("Couldn’t read {}: {error}", path.display()), cx),
            });
        })
        .detach();
    }

    /// Asks whether imported blocks go after the document or replace it.
    pub fn confirm_import(&mut self, path: &Path, blocks: Vec<Block>, window: &mut Window, cx: &mut Context<Self>) {
        let title: SharedString = format!("Import “{}”?", file_name(path)).into();
        let tab = cx.weak_entity();
        let replace_blocks = blocks.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            let (append_tab, replace_tab) = (tab.clone(), tab.clone());
            let (append_blocks, replace_blocks) = (blocks.clone(), replace_blocks.clone());
            dialog
                .title(title.clone())
                .child(
                    "Append adds it after the last block. Replace removes the current text, which can’t be undone.",
                )
                .footer(
                    DialogFooter::new()
                        .child(DialogClose::new().child(Button::new("import-cancel").outline().label("Cancel")))
                        .child(Button::new("import-replace").outline().with_variant(ButtonVariant::Danger).label("Replace").on_click(
                            move |_, window, cx| {
                                let blocks = replace_blocks.clone();
                                _ = replace_tab.update(cx, |tab, cx| tab.apply_import(blocks, true, window, cx));
                                window.close_dialog(cx);
                            },
                        ))
                        .child(DialogAction::new().child(Button::new("import-append").primary().label("Append"))),
                )
                .on_ok(move |_, window, cx| {
                    let blocks = append_blocks.clone();
                    _ = append_tab.update(cx, |tab, cx| tab.apply_import(blocks, false, window, cx));
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
            let trailing_empty = document
                .blocks
                .last()
                .is_some_and(|block| block.kind == BlockKind::Paragraph && block.text.is_empty());
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
        let kind = match self.kind {
            DocumentKind::Script => "script",
            DocumentKind::Notes => "notes",
            DocumentKind::Custom => "document",
        };
        (directory, format!("{} – {kind}.md", sanitize_file_name(title)))
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

/// Replaces characters that are not allowed in file names.
fn sanitize_file_name(name: &str) -> String {
    name.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '-' } else { c }).collect()
}

impl Render for DocumentTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match &self.editor {
            Some(editor) => div().w_full().child(editor.clone()).into_any_element(),
            None => v_flex()
                .w_full()
                .py_8()
                .child(
                    empty_state(
                        IconName::FileExclamationPoint,
                        "This document couldn’t be opened",
                        "The database didn’t return it. Try again, or restart VideoCRM if it keeps happening.",
                    )
                    .child(Button::new("document-retry").outline().small().label("Try again").on_click(cx.listener(
                        |this, _, window, cx| this.load(window, cx),
                    ))),
                )
                .into_any_element(),
        }
    }
}

/// The Script tab's rail: spoken length, cues, outline and Markdown actions.
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
        let outline: Vec<RowButton> = stats.outline.into_iter().map(|item| {
            let tab = tab.clone();
            let id = item.block_id;
            RowButton::new(SharedString::from(format!("outline-{id}")), item.text)
                .indent(Rems(0.75 * f32::from(item.level - top_level)))
                .muted(item.level > top_level)
                .on_click(move |_, window, cx| tab.update(cx, |tab, cx| tab.focus_block(id, window, cx)))
        }).collect();
        let action = |id: &'static str, icon: IconName, label: &'static str, run: fn(&mut DocumentTab, &mut Window, &mut Context<DocumentTab>)| {
            let tab = self.tab.clone();
            RowButton::new(id, label).icon(icon).on_click(move |_, window, cx| tab.update(cx, |tab, cx| run(tab, window, cx)))
        };

        v_flex()
            .id("script-rail")
            .size_full()
            .overflow_y_scroll()
            .px_4()
            .py_6()
            .gap_6()
            .child(
                v_flex()
                    .gap_3()
                    .child(section_label("Script", cx))
                    .child(
                        div()
                            .grid()
                            .grid_cols(2)
                            .gap_4()
                            .child(stat(stats.words.to_string(), "spoken words", cx))
                            .child(stat(format!("~{}", format_runtime(stats.seconds)), "runtime", cx))
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
                    .gap_1()
                    .child(div().pb_1().child(section_label("Outline", cx)))
                    .map(|this| {
                        if outline.is_empty() {
                            this.child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Headings in the script show up here."),
                            )
                        } else {
                            this.children(outline)
                        }
                    }),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(div().pb_1().child(section_label("Script text", cx)))
                    .child(action(
                        "copy-teleprompter",
                        IconName::Copy,
                        "Copy teleprompter text",
                        DocumentTab::copy_teleprompter_text,
                    ))
                    .child(action("export-markdown", IconName::FileDown, "Export Markdown…", DocumentTab::export_markdown))
                    .child(action("import-markdown", IconName::FileUp, "Import Markdown…", DocumentTab::import_markdown)),
            )
    }
}
