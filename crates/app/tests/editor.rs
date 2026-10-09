//! UI integration tests for the block document editor: real keyboard and
//! pointer input in a headless window, checked against the edited document.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, ClipboardItem, ElementId, Entity, TestAppContext, Window,
    WindowBounds, WindowOptions, point, px, size,
};
use videocrm::editor::{DocumentEditor, DocumentEditorEvent};
use videocrm_core::{Block, BlockId, BlockKind, Document, DocumentId, DocumentKind, VideoId};

struct Fixture {
    window: AnyWindowHandle,
    editor: Entity<DocumentEditor>,
    events: Rc<RefCell<Vec<DocumentEditorEvent>>>,
}

fn document(blocks: Vec<Block>) -> Document {
    Document {
        id: DocumentId::new(),
        video_id: VideoId::new(),
        kind: DocumentKind::Script,
        title: "Script".into(),
        blocks,
        position: 0,
        updated_at: chrono::Utc::now(),
    }
}

/// Element id of one part of a block, as the editor assigns it.
fn part(id: BlockId, name: &'static str) -> ElementId {
    ElementId::NamedChild(Arc::new(ElementId::Uuid(id.0)), name.into())
}

fn slash_item(label: &'static str) -> ElementId {
    ElementId::NamedChild(Arc::new("slash-item".into()), label.into())
}

impl Fixture {
    fn new(cx: &mut TestAppContext, blocks: Vec<Block>) -> Self {
        cx.update(gpui_kit::init);
        let (window, editor) = cx.update(|cx| {
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    Default::default(),
                    size(px(1000.), px(900.)),
                ))),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| DocumentEditor::new(document(blocks), window, cx))
            })
            .expect("open test window")
        });
        let events = Rc::new(RefCell::new(Vec::new()));
        let sink = events.clone();
        cx.update(|cx| {
            cx.subscribe(&editor, move |_, event: &DocumentEditorEvent, _| {
                sink.borrow_mut().push(event.clone())
            })
            .detach()
        });
        let fixture = Self { window, editor, events };
        fixture.step(cx, |window, cx| window.render_frame(cx));
        fixture
    }

    /// Runs one interaction, then lets GPUI deliver input events and redraw.
    fn step(&self, cx: &mut TestAppContext, interact: impl FnOnce(&mut Window, &mut App)) {
        cx.update_window(self.window, |_, window, cx| interact(window, cx)).expect("window is open");
        cx.run_until_parked();
        cx.update_window(self.window, |_, window, cx| window.render_frame(cx)).expect("window is open");
    }

    fn press(&self, cx: &mut TestAppContext, keys: &[&str]) {
        self.step(cx, |window, cx| {
            for key in keys {
                window.press(key, cx);
            }
        });
    }

    fn type_text(&self, cx: &mut TestAppContext, text: &str) {
        self.step(cx, |window, cx| window.input(text, cx));
    }

    fn click(&self, cx: &mut TestAppContext, target: ElementId) {
        self.step(cx, |window, cx| window.click(target, cx));
    }

    /// Hovers a block so its gutter (add button and handle) is shown.
    fn hover(&self, cx: &mut TestAppContext, id: BlockId) {
        self.step(cx, |window, cx| window.hover(part(id, "row"), cx));
    }

    fn focus_first(&self, cx: &mut TestAppContext) {
        let editor = self.editor.clone();
        self.step(cx, |window, cx| editor.update(cx, |editor, cx| editor.focus_first(window, cx)));
    }

    fn focus_last(&self, cx: &mut TestAppContext) {
        let editor = self.editor.clone();
        self.step(cx, |window, cx| editor.update(cx, |editor, cx| editor.focus_last(window, cx)));
    }

    fn blocks(&self, cx: &mut TestAppContext) -> Vec<Block> {
        cx.read(|cx| self.editor.read(cx).document(cx).blocks)
    }

    fn summary(&self, cx: &mut TestAppContext) -> Vec<(BlockKind, String)> {
        self.blocks(cx).into_iter().map(|block| (block.kind, block.text)).collect()
    }

    fn focused(&self, cx: &mut TestAppContext) -> Option<BlockId> {
        cx.read(|cx| self.editor.read(cx).focused_block())
    }

    fn changes(&self) -> usize {
        self.events.borrow().iter().filter(|event| **event == DocumentEditorEvent::Changed).count()
    }

    fn is_visible(&self, cx: &mut TestAppContext, target: ElementId) -> bool {
        cx.update_window(self.window, |_, window, _| {
            window.try_find(target).is_some_and(|found| found.visible())
        })
        .expect("window is open")
    }
}

fn paragraph(text: &str) -> Block {
    Block::paragraph(text)
}

fn block(kind: BlockKind, text: &str) -> Block {
    Block::new(kind, text)
}

fn owned(summary: &[(BlockKind, &str)]) -> Vec<(BlockKind, String)> {
    summary.iter().map(|(kind, text)| (*kind, text.to_string())).collect()
}

#[gpui_kit::test]
fn typing_into_a_clicked_block_updates_the_document(cx: &mut TestAppContext) {
    let hello = paragraph("Hello");
    let f = Fixture::new(cx, vec![hello.clone()]);

    f.click(cx, part(hello.id, "text"));
    assert_eq!(f.focused(cx), Some(hello.id));
    assert!(f.events.borrow().contains(&DocumentEditorEvent::FocusChanged(Some(hello.id))));

    f.press(cx, &["end"]);
    f.type_text(cx, " world");
    assert_eq!(f.summary(cx), owned(&[(BlockKind::Paragraph, "Hello world")]));
    assert!(f.changes() >= 1, "typing emits Changed");
}

#[gpui_kit::test]
fn enter_splits_the_block_at_the_caret(cx: &mut TestAppContext) {
    let f = Fixture::new(cx, vec![paragraph("Hello world")]);
    f.focus_first(cx);
    f.press(cx, &["right", "right", "right", "right", "right", "enter"]);
    assert_eq!(f.summary(cx), owned(&[(BlockKind::Paragraph, "Hello"), (BlockKind::Paragraph, " world")]));
    let blocks = f.blocks(cx);
    assert_eq!(f.focused(cx), Some(blocks[1].id), "the caret moves to the new block");

    f.type_text(cx, "big");
    assert_eq!(f.blocks(cx)[1].text, "big world");
}

#[gpui_kit::test]
fn enter_continues_lists_and_leaves_them_when_empty(cx: &mut TestAppContext) {
    let f = Fixture::new(cx, vec![block(BlockKind::Bulleted, "one")]);
    f.focus_last(cx);
    f.press(cx, &["enter"]);
    assert_eq!(f.summary(cx), owned(&[(BlockKind::Bulleted, "one"), (BlockKind::Bulleted, "")]));
    f.press(cx, &["enter"]);
    assert_eq!(f.summary(cx), owned(&[(BlockKind::Bulleted, "one"), (BlockKind::Paragraph, "")]));

    let f = Fixture::new(cx, vec![block(BlockKind::Heading1, "Title")]);
    f.focus_last(cx);
    f.press(cx, &["enter"]);
    f.type_text(cx, "Body");
    assert_eq!(f.summary(cx), owned(&[(BlockKind::Heading1, "Title"), (BlockKind::Paragraph, "Body")]));
}

#[gpui_kit::test]
fn shift_enter_inserts_a_soft_line_break(cx: &mut TestAppContext) {
    let f = Fixture::new(cx, vec![paragraph("one")]);
    f.focus_last(cx);
    f.press(cx, &["shift-enter"]);
    f.type_text(cx, "two");
    assert_eq!(f.summary(cx), owned(&[(BlockKind::Paragraph, "one\ntwo")]));
}

#[gpui_kit::test]
fn backspace_at_the_start_merges_into_the_previous_block(cx: &mut TestAppContext) {
    let second = paragraph("world");
    let f = Fixture::new(cx, vec![paragraph("Hello"), second.clone()]);
    f.click(cx, part(second.id, "text"));
    f.press(cx, &["home", "backspace"]);
    assert_eq!(f.summary(cx), owned(&[(BlockKind::Paragraph, "Helloworld")]));
    // The caret sits at the join.
    f.type_text(cx, ", ");
    assert_eq!(f.summary(cx), owned(&[(BlockKind::Paragraph, "Hello, world")]));
}

#[gpui_kit::test]
fn backspace_converts_other_kinds_to_paragraphs_and_deletes_dividers(cx: &mut TestAppContext) {
    let f = Fixture::new(cx, vec![block(BlockKind::Heading2, "Title")]);
    f.focus_first(cx);
    f.press(cx, &["backspace"]);
    assert_eq!(f.summary(cx), owned(&[(BlockKind::Paragraph, "Title")]));

    let last = paragraph("b");
    let f = Fixture::new(cx, vec![paragraph("a"), block(BlockKind::Divider, ""), last.clone()]);
    f.click(cx, part(last.id, "text"));
    f.press(cx, &["home", "backspace"]);
    assert_eq!(f.summary(cx), owned(&[(BlockKind::Paragraph, "a"), (BlockKind::Paragraph, "b")]));
}

#[gpui_kit::test]
fn delete_at_the_end_pulls_the_next_block_in(cx: &mut TestAppContext) {
    let f = Fixture::new(cx, vec![paragraph("ab"), block(BlockKind::Quote, "cd")]);
    f.focus_first(cx);
    f.press(cx, &["end", "delete"]);
    assert_eq!(f.summary(cx), owned(&[(BlockKind::Paragraph, "abcd")]));
}

#[gpui_kit::test]
fn markdown_shortcuts_convert_the_block(cx: &mut TestAppContext) {
    let cases = [
        ("# ", BlockKind::Heading1),
        ("## ", BlockKind::Heading2),
        ("### ", BlockKind::Heading3),
        ("- ", BlockKind::Bulleted),
        ("* ", BlockKind::Bulleted),
        ("1. ", BlockKind::Numbered),
        ("[] ", BlockKind::Todo),
        ("[ ] ", BlockKind::Todo),
        ("> ", BlockKind::Quote),
        ("!! ", BlockKind::Callout),
        ("```", BlockKind::Code),
    ];
    for (marker, kind) in cases {
        let f = Fixture::new(cx, vec![paragraph("")]);
        f.focus_first(cx);
        f.type_text(cx, marker);
        f.type_text(cx, "Text");
        assert_eq!(f.summary(cx), owned(&[(kind, "Text")]), "marker {marker:?}");
    }
}

#[gpui_kit::test]
fn three_dashes_insert_a_divider_and_continue_below(cx: &mut TestAppContext) {
    let f = Fixture::new(cx, vec![paragraph("")]);
    f.focus_first(cx);
    f.type_text(cx, "---");
    f.type_text(cx, "after");
    assert_eq!(f.summary(cx), owned(&[(BlockKind::Divider, ""), (BlockKind::Paragraph, "after")]));
}

#[gpui_kit::test]
fn slash_menu_filters_and_converts_the_block(cx: &mut TestAppContext) {
    let f = Fixture::new(cx, vec![paragraph("")]);
    f.focus_first(cx);
    f.type_text(cx, "/");
    assert!(f.is_visible(cx, "slash-menu".into()));
    assert!(f.is_visible(cx, slash_item("Text")));
    assert!(f.is_visible(cx, slash_item("Heading 1")));

    f.type_text(cx, "broll");
    assert!(f.is_visible(cx, slash_item("B-roll cue")));
    assert!(!f.is_visible(cx, slash_item("Text")));

    f.press(cx, &["enter"]);
    assert!(!f.is_visible(cx, "slash-menu".into()));
    assert_eq!(f.summary(cx), owned(&[(BlockKind::BRoll, "")]));
}

#[gpui_kit::test]
fn slash_menu_keyboard_selection_and_dismissal(cx: &mut TestAppContext) {
    let f = Fixture::new(cx, vec![paragraph("")]);
    f.focus_first(cx);
    f.type_text(cx, "/");
    f.type_text(cx, "head");
    f.press(cx, &["down", "enter"]);
    assert_eq!(f.summary(cx), owned(&[(BlockKind::Heading2, "")]));

    let f = Fixture::new(cx, vec![paragraph("Intro")]);
    f.focus_last(cx);
    f.type_text(cx, " /");
    assert!(f.is_visible(cx, "slash-menu".into()));
    f.press(cx, &["escape"]);
    assert!(!f.is_visible(cx, "slash-menu".into()));
    assert_eq!(f.summary(cx), owned(&[(BlockKind::Paragraph, "Intro /")]));

    // Deleting the slash closes the menu too.
    f.type_text(cx, "x");
    f.press(cx, &["backspace", "backspace"]);
    assert!(f.blocks(cx)[0].text == "Intro ");
    f.type_text(cx, "/");
    assert!(f.is_visible(cx, "slash-menu".into()));
    f.press(cx, &["backspace"]);
    assert!(!f.is_visible(cx, "slash-menu".into()));
}

#[gpui_kit::test]
fn gutter_plus_adds_a_block_with_the_menu_open(cx: &mut TestAppContext) {
    let first = paragraph("first");
    let f = Fixture::new(cx, vec![first.clone()]);
    assert!(!f.is_visible(cx, part(first.id, "add")), "the gutter shows on hover only");
    f.hover(cx, first.id);
    f.click(cx, part(first.id, "add"));
    assert!(f.is_visible(cx, "slash-menu".into()));
    f.type_text(cx, "todo");
    f.press(cx, &["enter"]);
    assert_eq!(f.summary(cx), owned(&[(BlockKind::Paragraph, "first"), (BlockKind::Todo, "")]));
}

#[gpui_kit::test]
fn tab_indents_list_items_only(cx: &mut TestAppContext) {
    let f = Fixture::new(cx, vec![block(BlockKind::Bulleted, "a"), block(BlockKind::Bulleted, "b")]);
    f.focus_last(cx);
    f.press(cx, &["tab"]);
    assert_eq!(f.blocks(cx)[1].indent, 1);
    f.press(cx, &["tab"]);
    assert_eq!(f.blocks(cx)[1].indent, 1, "one level deeper than the item above at most");
    f.press(cx, &["shift-tab"]);
    assert_eq!(f.blocks(cx)[1].indent, 0);

    let f = Fixture::new(cx, vec![paragraph("text")]);
    f.focus_last(cx);
    f.press(cx, &["tab"]);
    assert_eq!(f.summary(cx), owned(&[(BlockKind::Paragraph, "text")]), "no tab character");
}

#[gpui_kit::test]
fn todo_checkbox_and_shortcut_toggle_completion(cx: &mut TestAppContext) {
    let todo = block(BlockKind::Todo, "Film A-roll");
    let f = Fixture::new(cx, vec![todo.clone()]);
    let before = f.changes();
    f.click(cx, part(todo.id, "check"));
    assert!(f.blocks(cx)[0].checked);
    assert!(f.changes() > before);
    assert_eq!(f.focused(cx), None, "toggling does not move the caret into the item");

    f.focus_last(cx);
    f.press(cx, &["secondary-enter"]);
    assert!(!f.blocks(cx)[0].checked);
}

#[gpui_kit::test]
fn formatting_shortcuts_wrap_and_unwrap_the_selection(cx: &mut TestAppContext) {
    let f = Fixture::new(cx, vec![paragraph("Hello world")]);
    f.focus_first(cx);
    f.press(cx, &["shift-right", "shift-right", "shift-right", "shift-right", "shift-right", "secondary-b"]);
    assert_eq!(f.blocks(cx)[0].text, "**Hello** world");
    f.press(cx, &["secondary-b"]);
    assert_eq!(f.blocks(cx)[0].text, "Hello world");
    f.press(cx, &["secondary-i"]);
    assert_eq!(f.blocks(cx)[0].text, "*Hello* world");
    f.press(cx, &["secondary-e"]);
    assert_eq!(f.blocks(cx)[0].text, "*`Hello`* world");
}

#[gpui_kit::test]
fn arrows_move_between_blocks_and_select_dividers(cx: &mut TestAppContext) {
    let blocks = vec![paragraph("one"), block(BlockKind::Divider, ""), paragraph("two")];
    let ids: Vec<_> = blocks.iter().map(|block| block.id).collect();
    let f = Fixture::new(cx, blocks);
    f.focus_first(cx);
    f.press(cx, &["down"]);
    assert_eq!(f.focused(cx), Some(ids[1]), "the divider is selected");
    f.press(cx, &["down"]);
    assert_eq!(f.focused(cx), Some(ids[2]));
    f.press(cx, &["up", "up"]);
    assert_eq!(f.focused(cx), Some(ids[0]));

    f.press(cx, &["down", "backspace"]);
    assert_eq!(f.summary(cx), owned(&[(BlockKind::Paragraph, "one"), (BlockKind::Paragraph, "two")]));
}

#[gpui_kit::test]
fn multi_line_paste_creates_blocks(cx: &mut TestAppContext) {
    let f = Fixture::new(cx, vec![paragraph("")]);
    f.focus_first(cx);
    cx.write_to_clipboard(ClipboardItem::new_string(
        "# Plan\n- shoot\n1. edit\n[ ] upload\n> quote\nnotes".into(),
    ));
    f.press(cx, &["secondary-v"]);
    assert_eq!(
        f.summary(cx),
        owned(&[
            (BlockKind::Heading1, "Plan"),
            (BlockKind::Bulleted, "shoot"),
            (BlockKind::Numbered, "edit"),
            (BlockKind::Todo, "upload"),
            (BlockKind::Quote, "quote"),
            (BlockKind::Paragraph, "notes"),
        ])
    );
    let last = f.blocks(cx).last().map(|block| block.id);
    assert_eq!(f.focused(cx), last);
}

#[gpui_kit::test]
fn an_empty_document_starts_with_a_paragraph_on_click(cx: &mut TestAppContext) {
    let f = Fixture::new(cx, Vec::new());
    f.click(cx, "document-end".into());
    f.type_text(cx, "Hello");
    assert_eq!(f.summary(cx), owned(&[(BlockKind::Paragraph, "Hello")]));
}

#[gpui_kit::test]
fn clicking_below_the_last_block_focuses_a_trailing_paragraph(cx: &mut TestAppContext) {
    let f = Fixture::new(cx, vec![block(BlockKind::Heading1, "Title")]);
    f.click(cx, "document-end".into());
    let blocks = f.blocks(cx);
    assert_eq!(blocks.len(), 2);
    assert_eq!((blocks[1].kind, blocks[1].text.as_str()), (BlockKind::Paragraph, ""));
    assert_eq!(f.focused(cx), Some(blocks[1].id));

    f.click(cx, "document-end".into());
    assert_eq!(f.blocks(cx).len(), 2, "an empty trailing paragraph is reused");
}

#[gpui_kit::test]
fn dragging_a_handle_reorders_blocks(cx: &mut TestAppContext) {
    let blocks = vec![paragraph("a"), paragraph("b"), paragraph("c")];
    let ids: Vec<_> = blocks.iter().map(|block| block.id).collect();
    let f = Fixture::new(cx, blocks);
    f.hover(cx, ids[2]);
    f.step(cx, |window, cx| window.drag_to(part(ids[2], "handle"), part(ids[0], "row"), cx));
    let texts: Vec<_> = f.blocks(cx).into_iter().map(|block| block.text).collect();
    assert_eq!(texts, ["c", "a", "b"]);
}

#[gpui_kit::test]
fn block_menu_duplicates_and_deletes(cx: &mut TestAppContext) {
    let target = paragraph("keep");
    let f = Fixture::new(cx, vec![target.clone(), paragraph("other")]);
    f.hover(cx, target.id);
    f.click(cx, part(target.id, "handle"));
    assert!(f.is_visible(cx, "popup-menu".into()));
    // Items: Turn into, separator, Duplicate, Move up, Move down, separator, Delete.
    f.step(cx, |window, cx| window.within("popup-menu").click(2usize, cx));
    assert_eq!(
        f.summary(cx),
        owned(&[
            (BlockKind::Paragraph, "keep"),
            (BlockKind::Paragraph, "keep"),
            (BlockKind::Paragraph, "other")
        ])
    );

    f.hover(cx, target.id);
    f.click(cx, part(target.id, "handle"));
    f.step(cx, |window, cx| window.within("popup-menu").click(6usize, cx));
    assert_eq!(f.summary(cx), owned(&[(BlockKind::Paragraph, "keep"), (BlockKind::Paragraph, "other")]));
    assert!(f.blocks(cx).iter().all(|block| block.id != target.id));
}

#[gpui_kit::test]
fn set_document_replaces_content_without_reporting_a_change(cx: &mut TestAppContext) {
    let f = Fixture::new(cx, vec![paragraph("draft")]);
    f.focus_last(cx);
    f.type_text(cx, " edited");
    let edits = f.changes();
    assert!(edits > 0);

    let replacement = document(vec![block(BlockKind::Heading1, "Other tab")]);
    let editor = f.editor.clone();
    let next = replacement.clone();
    f.step(cx, |window, cx| editor.update(cx, |editor, cx| editor.set_document(next, window, cx)));
    assert_eq!(cx.read(|cx| f.editor.read(cx).document(cx)), replacement);
    assert_eq!(f.changes(), edits);
}

#[gpui_kit::test]
fn long_documents_render_only_rows_near_the_viewport(cx: &mut TestAppContext) {
    let blocks: Vec<_> = (0..300).map(|ix| paragraph(&format!("Line {ix}"))).collect();
    let (first, last) = (blocks[0].id, blocks[299].id);
    let f = Fixture::new(cx, blocks);
    // The first frame measures every row; later frames skip the far ones.
    f.step(cx, |window, cx| window.render_frame(cx));
    assert!(f.is_visible(cx, part(first, "text")));
    let rendered = cx
        .update_window(f.window, |_, window, _| window.try_find(part(last, "text")).is_some())
        .expect("window is open");
    assert!(!rendered, "a row far below the viewport is a spacer");

    f.focus_last(cx);
    f.type_text(cx, "!");
    assert_eq!(f.blocks(cx)[299].text, "Line 299!");
    assert_eq!(f.blocks(cx).len(), 300);
}

#[gpui_kit::test]
fn the_block_menu_opens_from_the_keyboard(cx: &mut TestAppContext) {
    let f = Fixture::new(cx, vec![paragraph("first"), paragraph("second")]);
    f.focus_last(cx);
    f.press(cx, &["secondary-/"]);
    assert!(f.is_visible(cx, "popup-menu".into()));
    // Move up is the fourth item.
    f.step(cx, |window, cx| window.within("popup-menu").click(3usize, cx));
    let texts: Vec<_> = f.blocks(cx).into_iter().map(|block| block.text).collect();
    assert_eq!(texts, ["second", "first"]);
    assert!(!f.is_visible(cx, "popup-menu".into()));
}

#[gpui_kit::test]
fn clicking_a_link_opens_it_without_editing(cx: &mut TestAppContext) {
    let link = paragraph("[docs](https://example.com/docs) for details");
    let f = Fixture::new(cx, vec![link.clone()]);
    f.step(cx, |window, cx| window.click_at(part(link.id, "text"), point(px(4.), px(8.)), cx));
    assert_eq!(cx.opened_url().as_deref(), Some("https://example.com/docs"));
    assert_eq!(f.focused(cx), None);

    // Elsewhere in the text, a click starts editing.
    f.step(cx, |window, cx| window.click_at(part(link.id, "text"), point(px(200.), px(8.)), cx));
    assert_eq!(f.focused(cx), Some(link.id));
}

#[gpui_kit::test]
fn clicking_inside_the_edited_block_places_the_caret_there(cx: &mut TestAppContext) {
    let hello = paragraph("Hello world");
    let f = Fixture::new(cx, vec![hello.clone()]);
    f.focus_last(cx);
    f.step(cx, |window, cx| window.click_at(part(hello.id, "text"), point(px(2.), px(8.)), cx));
    f.type_text(cx, "X");
    assert_eq!(f.summary(cx), owned(&[(BlockKind::Paragraph, "XHello world")]));
}

#[gpui_kit::test]
fn escape_closes_the_block_menu_and_returns_to_the_block(cx: &mut TestAppContext) {
    let second = paragraph("second");
    let f = Fixture::new(cx, vec![paragraph("first"), second.clone()]);
    f.focus_last(cx);
    f.press(cx, &["secondary-/"]);
    assert!(f.is_visible(cx, "popup-menu".into()));
    f.press(cx, &["escape"]);
    assert!(!f.is_visible(cx, "popup-menu".into()));
    assert_eq!(f.focused(cx), Some(second.id));
    f.type_text(cx, "!");
    assert_eq!(f.summary(cx), owned(&[(BlockKind::Paragraph, "first"), (BlockKind::Paragraph, "second!")]));
}
