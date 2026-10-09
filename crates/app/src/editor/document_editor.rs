//! The document editor entity: block state, keyboard behavior and commands.
//! Presentation lives in [`render`].

mod geometry;
mod render;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::component::input::{
    Backspace, Delete, Enter, Escape, IndentInline, InputEvent, MoveDown, MoveLeft, MoveRight, MoveUp, OutdentInline,
    Paste, TextareaState,
};
use gpui_kit::component::menu::PopupMenu;
use gpui_kit::{
    App, AppContext as _, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, Pixels, Point,
    ScrollHandle, SharedString, Subscription, Window, point,
};
use videocrm_core::{Block, BlockId, BlockKind, Document};

use self::geometry::Geometry;
use super::blocks::{self, Merge};
use super::inline::{Marker, toggle_marker};
use super::markdown::{Shortcut, detect_quote_marker, detect_shortcut, is_list, parse_pasted};
use super::slash::{self, BlockType};
use super::{
    ClearBlockSelection, DeleteSelectedBlock, InsertParagraphBelow, OpenBlockMenu, SelectNextBlock,
    SelectPreviousBlock, ToggleBold, ToggleCode, ToggleItalic,
};

/// Events a [`DocumentEditor`] emits to its owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocumentEditorEvent {
    /// Text or structure changed. Emitted for every edit; the owner decides
    /// how to debounce saving.
    Changed,
    /// The block with keyboard focus changed (`None` once focus leaves the
    /// editor).
    FocusChanged(Option<BlockId>),
}

/// Edits the blocks of one [`Document`].
///
/// The editor owns the document while it is shown: read the current state
/// with [`Self::document`] and replace it with [`Self::set_document`]. Text
/// inputs are created lazily for blocks that receive focus and are keyed by
/// [`BlockId`], so reordering never resets a block's editing state.
pub struct DocumentEditor {
    document: Document,
    inputs: HashMap<BlockId, BlockInput>,
    /// Block with keyboard focus: a text block whose input is focused or a
    /// selected divider. Kept while a menu inside the editor takes focus.
    focused: Option<BlockId>,
    /// Selected non-text block (a divider); the editor's own handle is focused.
    selected: Option<BlockId>,
    slash: Option<SlashMenu>,
    block_menu: Option<BlockMenu>,
    drop_target: Option<DropTarget>,
    /// Row under the pointer; only it shows the gutter buttons.
    hovered: Option<BlockId>,
    /// Row measurements from the last frame, written during prepaint.
    geometry: Rc<RefCell<Geometry>>,
    /// The owner's scroll container, used to keep the caret in view.
    scroll_handle: Option<ScrollHandle>,
    focus_handle: FocusHandle,
    _focus_out: Subscription,
    _focus_root: Subscription,
}

struct BlockInput {
    state: Entity<TextareaState>,
    _subscription: Subscription,
}

/// The slash command menu, open while the text after a typed `/` filters it.
struct SlashMenu {
    block: BlockId,
    /// Byte offset of the `/` in the block text.
    start: usize,
    query: String,
    /// Index into the filtered block types.
    selected: usize,
    /// Keeps the keyboard selection in view when the list scrolls.
    scroll: ScrollHandle,
}

impl SlashMenu {
    fn new(block: BlockId, start: usize) -> Self {
        Self { block, start, query: String::new(), selected: 0, scroll: ScrollHandle::new() }
    }

    fn items(&self) -> Vec<&'static BlockType> {
        slash::filter(&self.query)
    }

    fn select(&mut self, ix: usize) {
        self.selected = ix;
        // The list's first child is its heading; show it with the first item.
        self.scroll.scroll_to_item(if ix == 0 { 0 } else { ix + 1 });
    }
}

struct BlockMenu {
    block: BlockId,
    menu: Entity<PopupMenu>,
    position: Point<Pixels>,
    /// Focus before the menu opened, restored when it is dismissed.
    restore_focus: Option<FocusHandle>,
    _subscription: Subscription,
}

/// Where a dragged block would land: before or after `block`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DropTarget {
    block: BlockId,
    after: bool,
}

/// Payload of a block drag started on its handle.
#[derive(Clone)]
struct DraggedBlock {
    id: BlockId,
    label: SharedString,
}

#[derive(Clone, Copy, Debug)]
enum Caret {
    Start,
    End,
    At(usize),
}

impl EventEmitter<DocumentEditorEvent> for DocumentEditor {}

impl Focusable for DocumentEditor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl DocumentEditor {
    pub fn new(document: Document, window: &mut Window, cx: &mut Context<Self>) -> Self {
        super::bind_keys(cx);
        let focus_handle = cx.focus_handle();
        let focus_out = cx.on_focus_out(&focus_handle, window, |this, _, _, cx| {
            this.slash = None;
            this.selected = None;
            this.set_focused(None, cx);
            cx.notify();
        });
        // The editor itself takes focus for a selected divider, and for clicks
        // between blocks, which leave no block focused.
        let focus_root = cx.on_focus(&focus_handle, window, |this, _, cx| {
            if this.selected.is_none() {
                this.set_focused(None, cx);
            }
        });
        Self {
            document,
            inputs: HashMap::new(),
            focused: None,
            selected: None,
            slash: None,
            block_menu: None,
            drop_target: None,
            hovered: None,
            geometry: Rc::default(),
            scroll_handle: None,
            focus_handle,
            _focus_out: focus_out,
            _focus_root: focus_root,
        }
    }

    /// The document with every edit applied.
    pub fn document(&self, cx: &App) -> Document {
        let mut document = self.document.clone();
        // Input events are delivered after the keystroke that caused them,
        // so read the focused input directly in case one is still pending.
        if let Some(id) = self.focused
            && let Some(input) = self.inputs.get(&id)
            && let Some(block) = document.blocks.iter_mut().find(|block| block.id == id)
        {
            block.text = input.state.read(cx).value().to_string();
        }
        document
    }

    /// Replaces the edited document, e.g. when the owner switches tabs.
    /// Does not emit [`DocumentEditorEvent::Changed`].
    pub fn set_document(&mut self, document: Document, window: &mut Window, cx: &mut Context<Self>) {
        let had_focus = self.focus_handle.contains_focused(window, cx);
        self.document = document;
        self.inputs.clear();
        self.slash = None;
        self.block_menu = None;
        self.drop_target = None;
        self.selected = None;
        self.set_focused(None, cx);
        if had_focus {
            self.focus_handle.focus(window, cx);
        }
        cx.notify();
    }

    /// Shares the scroll handle of the container the editor is placed in, so
    /// keyboard navigation and typing keep the focused block visible.
    pub fn set_scroll_handle(&mut self, handle: ScrollHandle) {
        self.scroll_handle = Some(handle);
    }

    /// The block with keyboard focus, if the editor has focus.
    pub fn focused_block(&self) -> Option<BlockId> {
        self.focused
    }

    /// Puts the caret at the start of the first block.
    pub fn focus_first(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.document.blocks.first() {
            Some(block) => self.focus_block(block.id, Caret::Start, window, cx),
            None => self.focus_end(window, cx),
        }
    }

    /// Puts the caret at the end of the last block.
    pub fn focus_last(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.document.blocks.last() {
            Some(block) => self.focus_block(block.id, Caret::End, window, cx),
            None => self.focus_end(window, cx),
        }
    }

    /// Puts the caret at the start of block `id` and scrolls the block to the
    /// top of the view; a divider is selected instead. Used to jump to an
    /// outline heading.
    pub fn focus_block_start(&mut self, id: BlockId, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_block(id, Caret::Start, window, cx);
        self.geometry.borrow_mut().request_reveal_at_top();
    }

    /// Focuses a trailing empty paragraph, adding one unless the document
    /// already ends with it. Used for clicks below the last block.
    pub fn focus_end(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let trailing = self
            .document
            .blocks
            .last()
            .filter(|block| block.kind == BlockKind::Paragraph && block.text.is_empty())
            .map(|block| block.id);
        let id = match trailing {
            Some(id) => id,
            None => {
                let block = Block::paragraph("");
                let id = block.id;
                self.document.blocks.push(block);
                self.changed(cx);
                id
            }
        };
        self.focus_block(id, Caret::Start, window, cx);
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        cx.emit(DocumentEditorEvent::Changed);
        cx.notify();
    }

    fn set_focused(&mut self, id: Option<BlockId>, cx: &mut Context<Self>) {
        if self.focused != id {
            self.focused = id;
            cx.emit(DocumentEditorEvent::FocusChanged(id));
        }
    }

    fn index(&self, id: BlockId) -> Option<usize> {
        blocks::position(&self.document.blocks, id)
    }

    /// The focused text block and its input, when an input has focus.
    fn editing(&self, window: &Window, cx: &App) -> Option<(usize, Entity<TextareaState>)> {
        let id = self.focused?;
        let input = self.inputs.get(&id)?;
        if !input.state.focus_handle(cx).is_focused(window) {
            return None;
        }
        Some((self.index(id)?, input.state.clone()))
    }

    /// The input of a text block, created on first use and synchronized
    /// with the block's text and kind.
    fn input(&mut self, id: BlockId, window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<TextareaState>> {
        let block = &self.document.blocks[self.index(id)?];
        let placeholder = render::input_placeholder(block, self.document.blocks.len() == 1);
        if let Some(input) = self.inputs.get(&id) {
            let text = block.text.clone();
            input.state.update(cx, |state, cx| {
                if state.value().as_ref() != text {
                    state.set_value(text, window, cx);
                }
                state.set_placeholder(placeholder.clone(), window, cx);
            });
            return Some(input.state.clone());
        }
        let text = block.text.clone();
        let state = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, usize::MAX)
                .soft_wrap(true)
                .placeholder(placeholder)
                .default_value(text)
        });
        let subscription = cx.subscribe_in(&state, window, move |this, state, event, window, cx| {
            this.on_input_event(id, state, event, window, cx)
        });
        self.inputs.insert(id, BlockInput { state: state.clone(), _subscription: subscription });
        Some(state)
    }

    fn focus_block(&mut self, id: BlockId, caret: Caret, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.index(id) else { return };
        let block = &self.document.blocks[ix];
        if !block.kind.has_text() {
            self.select_block(id, window, cx);
            return;
        }
        let offset = match caret {
            Caret::Start => 0,
            Caret::End => block.text.len(),
            Caret::At(offset) => offset.min(block.text.len()),
        };
        let Some(input) = self.input(id, window, cx) else { return };
        input.update(cx, |state, cx| {
            state.set_selected_range(offset..offset, cx);
            state.focus(window, cx);
        });
        self.selected = None;
        self.set_focused(Some(id), cx);
        self.geometry.borrow_mut().request_reveal();
        cx.notify();
    }

    fn select_block(&mut self, id: BlockId, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = Some(id);
        self.slash = None;
        self.focus_handle.focus(window, cx);
        self.set_focused(Some(id), cx);
        self.geometry.borrow_mut().request_reveal();
        cx.notify();
    }

    fn on_input_event(
        &mut self,
        id: BlockId,
        state: &Entity<TextareaState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => self.on_text_changed(id, state, window, cx),
            InputEvent::Focus => {
                self.selected = None;
                self.set_focused(Some(id), cx);
                cx.notify();
            }
            InputEvent::Blur => {
                if self.slash.as_ref().is_some_and(|menu| menu.block == id) {
                    self.slash = None;
                }
                cx.notify();
            }
            InputEvent::PressEnter { .. } => {}
        }
    }

    fn on_text_changed(
        &mut self,
        id: BlockId,
        state: &Entity<TextareaState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.index(id) else { return };
        let (text, cursor) = {
            let state = state.read(cx);
            (state.value().to_string(), state.cursor())
        };
        if self.document.blocks[ix].text == text {
            return;
        }
        let previous = std::mem::replace(&mut self.document.blocks[ix].text, text);
        self.geometry.borrow_mut().request_reveal();
        self.update_slash(ix, &previous, cursor);
        let block = &self.document.blocks[ix];
        let shortcut = match block.kind {
            _ if self.slash.is_some() => None,
            BlockKind::Paragraph => detect_shortcut(&block.text, cursor),
            // `> [!broll] ` typed in one go: the quote shortcut fires first.
            BlockKind::Quote => detect_quote_marker(&block.text, cursor),
            _ => None,
        };
        if let Some(shortcut) = shortcut {
            self.apply_shortcut(ix, shortcut, window, cx);
        }
        self.changed(cx);
    }

    /// Opens the slash menu after a `/` typed at the start of a block or
    /// after whitespace, and keeps its query in sync while it is open.
    fn update_slash(&mut self, ix: usize, previous: &str, cursor: usize) {
        let block = &self.document.blocks[ix];
        let text = block.text.as_str();
        if let Some(menu) = self.slash.as_mut().filter(|menu| menu.block == block.id) {
            let query = text
                .get(menu.start..cursor)
                .and_then(|typed| typed.strip_prefix('/'))
                .filter(|query| !query.starts_with(char::is_whitespace) && !query.contains('\n'));
            match query {
                Some(query) if !(query.ends_with(' ') && slash::filter(query).is_empty()) => {
                    if menu.query != query {
                        menu.query = query.to_owned();
                        menu.select(0);
                    }
                }
                _ => self.slash = None,
            }
            return;
        }
        if block.kind != BlockKind::Code
            && let Some(start) = slash::typed_slash(previous, text, cursor)
        {
            self.slash = Some(SlashMenu::new(block.id, start));
        }
    }

    fn apply_shortcut(&mut self, ix: usize, shortcut: Shortcut, window: &mut Window, cx: &mut Context<Self>) {
        let block = &mut self.document.blocks[ix];
        let id = block.id;
        block.text.replace_range(..shortcut.marker_len, "");
        blocks::convert(block, shortcut.kind);
        block.checked = shortcut.checked;
        if shortcut.kind == BlockKind::Divider {
            self.inputs.remove(&id);
            let next = Block::paragraph("");
            let next_id = next.id;
            self.document.blocks.insert(ix + 1, next);
            self.focus_block(next_id, Caret::Start, window, cx);
        } else {
            self.focus_block(id, Caret::Start, window, cx);
        }
    }

    /// Converts a block in place, used by the slash menu and "Turn into".
    /// An empty block becomes a divider followed by a new line; a block with
    /// text keeps it and gets a divider inserted below.
    fn turn_into(&mut self, id: BlockId, kind: BlockKind, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.index(id) else { return };
        let caret = self.caret_offset(id, cx).unwrap_or(Caret::End);
        let block = &mut self.document.blocks[ix];
        if kind == BlockKind::Divider && block.kind != BlockKind::Divider {
            if block.text.is_empty() {
                blocks::convert(block, BlockKind::Divider);
                self.inputs.remove(&id);
                self.insert_after(ix, Block::paragraph(""), window, cx);
            } else {
                self.document.blocks.insert(ix + 1, Block::new(BlockKind::Divider, ""));
                self.focus_block(id, caret, window, cx);
                self.changed(cx);
            }
            return;
        }
        blocks::convert(block, kind);
        self.focus_block(id, caret, window, cx);
        self.changed(cx);
    }

    fn caret_offset(&self, id: BlockId, cx: &App) -> Option<Caret> {
        let input = self.inputs.get(&id)?;
        Some(Caret::At(input.state.read(cx).cursor()))
    }

    fn choose_slash_item(&mut self, kind: BlockKind, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = self.slash.take() else { return };
        let Some(ix) = self.index(menu.block) else { return };
        let block = &mut self.document.blocks[ix];
        let end = (menu.start + 1 + menu.query.len()).min(block.text.len());
        if block.text.get(menu.start..end).is_some_and(|typed| typed.starts_with('/')) {
            block.text.replace_range(menu.start..end, "");
        }
        self.focus_block(menu.block, Caret::At(menu.start), window, cx);
        self.turn_into(menu.block, kind, window, cx);
    }

    fn move_slash_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if let Some(menu) = self.slash.as_mut() {
            let count = menu.items().len();
            if count > 0 {
                menu.select((menu.selected as isize + delta).rem_euclid(count as isize) as usize);
            }
            cx.notify();
        }
    }

    fn slash_is_open(&self, ix: usize) -> bool {
        self.slash.as_ref().is_some_and(|menu| menu.block == self.document.blocks[ix].id)
    }

    // Keyboard. The input binds Enter, Backspace, arrows and Tab itself; the
    // editor intercepts those actions in the capture phase and only stops
    // them when it turns the key into a block-level edit.

    fn on_enter(&mut self, action: &Enter, window: &mut Window, cx: &mut Context<Self>) {
        let Some((ix, input)) = self.editing(window, cx) else { return };
        if self.slash_is_open(ix) {
            let chosen = self.slash.as_ref().and_then(|menu| menu.items().get(menu.selected).map(|ty| ty.kind));
            if let Some(kind) = chosen {
                cx.stop_propagation();
                self.choose_slash_item(kind, window, cx);
                cx.notify();
                return;
            }
            // Nothing to choose: close the menu and let Enter be Enter.
            self.slash = None;
            cx.notify();
        }
        let kind = self.document.blocks[ix].kind;
        if action.secondary {
            cx.stop_propagation();
            if kind == BlockKind::Todo {
                let id = self.document.blocks[ix].id;
                let checked = !self.document.blocks[ix].checked;
                self.set_checked(id, checked, cx);
            } else {
                self.insert_after(ix, Block::paragraph(""), window, cx);
            }
            return;
        }
        if action.shift || kind == BlockKind::Code {
            return;
        }
        cx.stop_propagation();

        let (text, selection) = {
            let state = input.read(cx);
            (state.value().to_string(), state.selected_range())
        };
        let block = &mut self.document.blocks[ix];
        block.text = [&text[..selection.start], &text[selection.end..]].concat();
        let at = selection.start;

        if block.text.is_empty() && blocks::exits_when_empty(kind) {
            if is_list(kind) && block.indent > 0 {
                block.indent -= 1;
            } else {
                blocks::convert(block, BlockKind::Paragraph);
            }
            let id = block.id;
            self.focus_block(id, Caret::Start, window, cx);
            self.changed(cx);
            return;
        }

        if at == 0 && !block.text.is_empty() {
            // Enter at the start opens a line above and keeps the caret here.
            let above = blocks::continuation(block, "");
            let id = block.id;
            self.document.blocks.insert(ix, above);
            self.focus_block(id, Caret::Start, window, cx);
            self.changed(cx);
            return;
        }

        let id = block.id;
        let new_ix = blocks::split(&mut self.document.blocks, ix, at);
        let new_id = self.document.blocks[new_ix].id;
        self.sync_input(id, window, cx);
        self.focus_block(new_id, Caret::Start, window, cx);
        self.changed(cx);
    }

    fn on_backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        let Some((ix, input)) = self.editing(window, cx) else { return };
        if !at_offset(&input, 0, cx) {
            return;
        }
        cx.stop_propagation();
        let block = &mut self.document.blocks[ix];
        let id = block.id;
        if block.kind != BlockKind::Paragraph {
            blocks::convert(block, BlockKind::Paragraph);
            self.focus_block(id, Caret::Start, window, cx);
            self.changed(cx);
            return;
        }
        match blocks::merge_into_previous(&mut self.document.blocks, ix) {
            Some(Merge::Joined { into, offset }) => {
                self.inputs.remove(&id);
                let target = self.document.blocks[into].id;
                self.focus_block(target, Caret::At(offset), window, cx);
            }
            Some(Merge::RemovedDivider { .. }) => {}
            None => return,
        }
        self.changed(cx);
    }

    fn on_delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        let Some((ix, input)) = self.editing(window, cx) else { return };
        let end = input.read(cx).text().len();
        if !at_offset(&input, end, cx) {
            return;
        }
        let Some(next_id) = self.document.blocks.get(ix + 1).map(|block| block.id) else { return };
        cx.stop_propagation();
        if let Some(Merge::Joined { into, offset }) = blocks::merge_next_into(&mut self.document.blocks, ix) {
            self.inputs.remove(&next_id);
            let id = self.document.blocks[into].id;
            self.focus_block(id, Caret::At(offset), window, cx);
        }
        self.changed(cx);
    }

    fn on_move_up(&mut self, _: &MoveUp, window: &mut Window, cx: &mut Context<Self>) {
        let Some((ix, input)) = self.editing(window, cx) else { return };
        if self.slash_is_open(ix) {
            cx.stop_propagation();
            self.move_slash_selection(-1, cx);
            return;
        }
        if ix == 0 || !on_edge_line(&input, Edge::First, cx) {
            return;
        }
        cx.stop_propagation();
        let previous = self.document.blocks[ix - 1].id;
        self.focus_block(previous, Caret::End, window, cx);
    }

    fn on_move_down(&mut self, _: &MoveDown, window: &mut Window, cx: &mut Context<Self>) {
        let Some((ix, input)) = self.editing(window, cx) else { return };
        if self.slash_is_open(ix) {
            cx.stop_propagation();
            self.move_slash_selection(1, cx);
            return;
        }
        if ix + 1 >= self.document.blocks.len() || !on_edge_line(&input, Edge::Last, cx) {
            return;
        }
        cx.stop_propagation();
        let next = self.document.blocks[ix + 1].id;
        self.focus_block(next, Caret::Start, window, cx);
    }

    fn on_move_left(&mut self, _: &MoveLeft, window: &mut Window, cx: &mut Context<Self>) {
        let Some((ix, input)) = self.editing(window, cx) else { return };
        if ix == 0 || !at_offset(&input, 0, cx) {
            return;
        }
        cx.stop_propagation();
        let previous = self.document.blocks[ix - 1].id;
        self.focus_block(previous, Caret::End, window, cx);
    }

    fn on_move_right(&mut self, _: &MoveRight, window: &mut Window, cx: &mut Context<Self>) {
        let Some((ix, input)) = self.editing(window, cx) else { return };
        let end = input.read(cx).text().len();
        if ix + 1 >= self.document.blocks.len() || !at_offset(&input, end, cx) {
            return;
        }
        cx.stop_propagation();
        let next = self.document.blocks[ix + 1].id;
        self.focus_block(next, Caret::Start, window, cx);
    }

    fn on_indent(&mut self, _: &IndentInline, window: &mut Window, cx: &mut Context<Self>) {
        self.indent_focused(1, window, cx);
    }

    fn on_outdent(&mut self, _: &OutdentInline, window: &mut Window, cx: &mut Context<Self>) {
        self.indent_focused(-1, window, cx);
    }

    /// Tab indents list items; code blocks keep Tab for indentation and
    /// other blocks ignore it rather than inserting a tab character.
    fn indent_focused(&mut self, delta: i8, window: &mut Window, cx: &mut Context<Self>) {
        let Some((ix, _)) = self.editing(window, cx) else { return };
        if self.document.blocks[ix].kind == BlockKind::Code {
            return;
        }
        cx.stop_propagation();
        if blocks::indent(&mut self.document.blocks, ix, delta) {
            self.changed(cx);
        }
    }

    fn on_escape(&mut self, _: &Escape, window: &mut Window, cx: &mut Context<Self>) {
        let Some((ix, _)) = self.editing(window, cx) else { return };
        if self.slash_is_open(ix) {
            cx.stop_propagation();
            self.slash = None;
            cx.notify();
        }
    }

    /// Multi-line text is read like an imported Markdown file, so callouts,
    /// cues and the app's own export paste back as the blocks they were;
    /// single lines and code blocks use the input's own paste.
    fn on_paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        let Some((ix, input)) = self.editing(window, cx) else { return };
        if self.document.blocks[ix].kind == BlockKind::Code {
            return;
        }
        let Some(pasted) = cx.read_from_clipboard().and_then(|item| item.text()) else { return };
        if !pasted.trim_end().contains('\n') {
            return;
        }
        let mut pasted = parse_pasted(&pasted);
        if pasted.is_empty() {
            return;
        }
        cx.stop_propagation();

        let (text, selection) = {
            let state = input.read(cx);
            (state.value().to_string(), state.selected_range())
        };
        let (before, after) = (&text[..selection.start], &text[selection.end..]);
        let first = pasted.remove(0);
        let block = &mut self.document.blocks[ix];
        let id = block.id;
        // An empty paragraph becomes the first pasted block. Otherwise the
        // first line joins the text before the caret, unless it is a block
        // that cannot be inlined (a divider or code), which goes below.
        if before.is_empty() && block.kind == BlockKind::Paragraph {
            block.kind = first.kind;
            block.checked = first.checked;
            block.indent = first.indent;
            block.text = first.text;
        } else if first.kind.has_text() && first.kind != BlockKind::Code {
            block.text = format!("{before}{}", first.text);
        } else {
            block.text = before.to_owned();
            pasted.insert(0, first);
        }

        let mut last = ix;
        for block in pasted {
            last += 1;
            self.document.blocks.insert(last, block);
        }
        // The caret needs a text block after a divider, and the text after
        // it must not end up inside pasted code.
        let trailing = self.document.blocks[last].kind;
        if !trailing.has_text() || (trailing == BlockKind::Code && !after.is_empty()) {
            last += 1;
            self.document.blocks.insert(last, Block::paragraph(""));
        }
        let caret = self.document.blocks[last].text.len();
        self.document.blocks[last].text.push_str(after);
        let last_id = self.document.blocks[last].id;
        if self.document.blocks[ix].kind.has_text() {
            self.sync_input(id, window, cx);
        } else {
            self.inputs.remove(&id);
        }
        self.focus_block(last_id, Caret::At(caret), window, cx);
        self.changed(cx);
    }

    fn toggle_bold(&mut self, _: &ToggleBold, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_marker(Marker::Bold, window, cx);
    }

    fn toggle_italic(&mut self, _: &ToggleItalic, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_marker(Marker::Italic, window, cx);
    }

    fn toggle_code(&mut self, _: &ToggleCode, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_marker(Marker::Code, window, cx);
    }

    fn toggle_marker(&mut self, marker: Marker, window: &mut Window, cx: &mut Context<Self>) {
        let Some((ix, input)) = self.editing(window, cx) else { return };
        if self.document.blocks[ix].kind == BlockKind::Code {
            return;
        }
        input.update(cx, |state, cx| {
            let (text, selection) = toggle_marker(&state.value(), state.selected_range(), marker);
            state.replace_all(text, window, cx);
            state.set_selected_range(selection, cx);
        });
    }

    // Selected divider.

    fn select_previous(&mut self, _: &SelectPreviousBlock, window: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(-1, window, cx);
    }

    fn select_next(&mut self, _: &SelectNextBlock, window: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(1, window, cx);
    }

    fn move_selection(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.selected.and_then(|id| self.index(id)) else { return };
        let Some(target) = ix.checked_add_signed(delta).and_then(|ix| self.document.blocks.get(ix)) else {
            return;
        };
        let caret = if delta < 0 { Caret::End } else { Caret::Start };
        self.focus_block(target.id, caret, window, cx);
    }

    fn delete_selected(&mut self, _: &DeleteSelectedBlock, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.selected {
            self.delete_block(id, window, cx);
        }
    }

    fn insert_below_selected(&mut self, _: &InsertParagraphBelow, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.selected.and_then(|id| self.index(id)) {
            self.insert_after(ix, Block::paragraph(""), window, cx);
        }
    }

    fn clear_selection(&mut self, _: &ClearBlockSelection, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected.take().is_some() {
            self.set_focused(None, cx);
            cx.notify();
        }
    }

    // Block commands, shared by the block menu, the gutter and the keyboard.

    fn insert_after(&mut self, ix: usize, block: Block, window: &mut Window, cx: &mut Context<Self>) -> BlockId {
        let id = block.id;
        self.document.blocks.insert(ix + 1, block);
        self.focus_block(id, Caret::Start, window, cx);
        self.changed(cx);
        id
    }

    /// The gutter's "+": a new paragraph below with the slash menu open.
    fn insert_with_menu(&mut self, after: BlockId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.index(after) else { return };
        let id = self.insert_after(ix, Block::paragraph("/"), window, cx);
        self.focus_block(id, Caret::End, window, cx);
        self.slash = Some(SlashMenu::new(id, 0));
    }

    fn set_checked(&mut self, id: BlockId, checked: bool, cx: &mut Context<Self>) {
        if let Some(ix) = self.index(id) {
            self.document.blocks[ix].checked = checked;
            self.changed(cx);
        }
    }

    fn duplicate_block(&mut self, id: BlockId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.index(id) else { return };
        let copy = blocks::duplicate(&mut self.document.blocks, ix);
        let copy_id = self.document.blocks[copy].id;
        self.focus_block(copy_id, Caret::End, window, cx);
        self.changed(cx);
    }

    fn move_block_by(&mut self, id: BlockId, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.index(id) else { return };
        let Some(target) = ix.checked_add_signed(delta).filter(|target| *target < self.document.blocks.len()) else {
            return;
        };
        let before = if delta > 0 { target + 1 } else { target };
        blocks::move_block(&mut self.document.blocks, ix, before);
        self.focus_block(id, Caret::End, window, cx);
        self.changed(cx);
    }

    fn delete_block(&mut self, id: BlockId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.index(id) else { return };
        self.document.blocks.remove(ix);
        self.inputs.remove(&id);
        if self.selected == Some(id) {
            self.selected = None;
        }
        if self.document.blocks.is_empty() {
            self.document.blocks.push(Block::paragraph(""));
        }
        let (neighbour, caret) = match ix.checked_sub(1) {
            Some(previous) => (self.document.blocks[previous].id, Caret::End),
            None => (self.document.blocks[0].id, Caret::Start),
        };
        self.focus_block(neighbour, caret, window, cx);
        self.changed(cx);
    }

    fn drop_block(&mut self, dragged: BlockId, onto: BlockId, window: &mut Window, cx: &mut Context<Self>) {
        let after = self.drop_target.take().is_some_and(|target| target.block == onto && target.after);
        let (Some(from), Some(to)) = (self.index(dragged), self.index(onto)) else { return };
        let before = if after { to + 1 } else { to };
        if before == from || before == from + 1 {
            cx.notify();
            return;
        }
        blocks::move_block(&mut self.document.blocks, from, before);
        self.focus_block(dragged, Caret::End, window, cx);
        self.changed(cx);
    }

    fn open_block_menu(&mut self, id: BlockId, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.index(id) else { return };
        let editor = cx.entity().downgrade();
        let target = render::MenuTarget {
            id,
            kind: self.document.blocks[ix].kind,
            is_first: ix == 0,
            is_last: ix + 1 == self.document.blocks.len(),
        };
        // Dismissing the menu returns focus to where it was opened from; a
        // menu reopened over another keeps the first one's origin.
        let restore_focus = match self.block_menu.take() {
            Some(open) => open.restore_focus,
            None => window.focused(cx),
        };
        let menu = PopupMenu::build(window, cx, {
            let restore_focus = restore_focus.clone();
            move |menu, window, cx| {
                let menu = render::block_menu(menu, editor, &target, window, cx);
                match restore_focus {
                    Some(handle) => menu.action_context(handle),
                    None => menu,
                }
            }
        });
        menu.focus_handle(cx).focus(window, cx);
        let subscription = cx.subscribe_in(&menu, window, |this, _, _: &DismissEvent, _, cx| {
            this.block_menu = None;
            cx.notify();
        });
        self.slash = None;
        self.block_menu = Some(BlockMenu { block: id, menu, position, restore_focus, _subscription: subscription });
        cx.notify();
    }

    /// Opens the block menu beside the focused block's text, where the drag
    /// handle's menu would appear.
    fn open_focused_block_menu(&mut self, _: &OpenBlockMenu, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.focused else { return };
        let Some(bounds) = self.geometry.borrow().row_bounds(id) else { return };
        let position = point(bounds.left() + window.rem_size() * render::GUTTER.0, bounds.top());
        self.open_block_menu(id, position, window, cx);
    }

    fn set_hovered(&mut self, id: BlockId, hovered: bool, cx: &mut Context<Self>) {
        let next = if hovered { Some(id) } else { self.hovered.filter(|current| *current != id) };
        if self.hovered != next {
            self.hovered = next;
            cx.notify();
        }
    }

    /// Writes a block's model text into its input after a structural edit.
    fn sync_input(&mut self, id: BlockId, window: &mut Window, cx: &mut Context<Self>) {
        if self.inputs.contains_key(&id) {
            self.input(id, window, cx);
        }
    }

    /// Focuses the block under a click on its rendered text. The block is
    /// already where the pointer is, so the page does not scroll.
    fn click_text(&mut self, id: BlockId, offset: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.block_menu = None;
        self.focus_block(id, Caret::At(offset), window, cx);
        self.geometry.borrow_mut().cancel_reveal();
    }
}

fn at_offset(input: &Entity<TextareaState>, offset: usize, cx: &App) -> bool {
    let state = input.read(cx);
    state.selected_range() == (offset..offset)
}

#[derive(Clone, Copy)]
enum Edge {
    First,
    Last,
}

/// Whether the caret is on the first or last visual line of the input, so
/// Up / Down should leave the block.
fn on_edge_line(input: &Entity<TextareaState>, edge: Edge, cx: &App) -> bool {
    let state = input.read(cx);
    let cursor = state.cursor();
    let reference = match edge {
        Edge::First => 0,
        Edge::Last => state.text().len(),
    };
    let laid_out = (state.range_to_bounds(&(cursor..cursor)), state.range_to_bounds(&(reference..reference)));
    match (laid_out, state.line_height()) {
        ((Some(caret), Some(reference)), Some(line_height)) => (caret.top() - reference.top()).abs() < line_height / 2.,
        _ => {
            let text = state.value();
            match edge {
                Edge::First => !text[..cursor].contains('\n'),
                Edge::Last => !text[cursor..].contains('\n'),
            }
        }
    }
}
