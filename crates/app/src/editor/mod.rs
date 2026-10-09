//! Block-based rich document editor (Notion-style).
//!
//! [`DocumentEditor`] edits the blocks of a [`videocrm_core::Document`]: text
//! blocks, lists, to-dos, quotes, callouts, code, dividers and the script cues
//! (B-roll, on-screen text). The focused block is edited as raw text with
//! inline Markdown; every other block renders it formatted. Register
//! [`Assets`] (or chain it from the application's asset source) so the
//! editor's icons resolve.

mod blocks;
mod document_editor;
mod icons;
mod inline;
mod markdown;
mod palette;
mod slash;

use gpui_kit::{App, Global, KeyBinding, actions};

pub use document_editor::{DocumentEditor, DocumentEditorEvent};
pub use icons::Assets;

/// Key context of the editor's root element.
const CONTEXT: &str = "DocumentEditor";

actions!(
    document_editor,
    [
        /// Wrap or unwrap the selection in `**`.
        ToggleBold,
        /// Wrap or unwrap the selection in `*`.
        ToggleItalic,
        /// Wrap or unwrap the selection in `` ` ``.
        ToggleCode,
        /// Move the block selection (a selected divider) up.
        SelectPreviousBlock,
        /// Move the block selection down.
        SelectNextBlock,
        /// Delete the selected divider.
        DeleteSelectedBlock,
        /// Add an empty paragraph below the selected divider.
        InsertParagraphBelow,
        /// Clear the block selection.
        ClearBlockSelection,
        /// Open the block menu (Turn into, Duplicate, Move, Delete) of the
        /// focused block; the keyboard path to the drag handle's menu.
        OpenBlockMenu,
    ]
);

struct KeyBindingsInstalled;

impl Global for KeyBindingsInstalled {}

/// Registers the editor's key bindings once per application. Text editing
/// keys (Enter, Backspace, arrows, Tab) reuse the input's own bindings and
/// are intercepted by the editor before the input handles them.
fn bind_keys(cx: &mut App) {
    if cx.has_global::<KeyBindingsInstalled>() {
        return;
    }
    cx.set_global(KeyBindingsInstalled);
    cx.bind_keys([
        KeyBinding::new("secondary-b", ToggleBold, Some(CONTEXT)),
        KeyBinding::new("secondary-i", ToggleItalic, Some(CONTEXT)),
        KeyBinding::new("secondary-e", ToggleCode, Some(CONTEXT)),
        KeyBinding::new("up", SelectPreviousBlock, Some(CONTEXT)),
        KeyBinding::new("down", SelectNextBlock, Some(CONTEXT)),
        KeyBinding::new("backspace", DeleteSelectedBlock, Some(CONTEXT)),
        KeyBinding::new("delete", DeleteSelectedBlock, Some(CONTEXT)),
        KeyBinding::new("enter", InsertParagraphBelow, Some(CONTEXT)),
        KeyBinding::new("escape", ClearBlockSelection, Some(CONTEXT)),
        KeyBinding::new("secondary-/", OpenBlockMenu, Some(CONTEXT)),
    ]);
}
