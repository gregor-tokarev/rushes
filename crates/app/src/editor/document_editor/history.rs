//! Document-level undo: the block list before and after each structural
//! edit (insert, delete, move, convert, duplicate, split, merge, paste).
//!
//! Typing inside a block stays in that block's input history, which the
//! input undoes itself. The two meet at the base: the document as the last
//! structural edit (or undo / redo) left it. While the focused block's text
//! differs from its text there, Undo belongs to the input; once it is back,
//! Undo steps through the document history. Typing that is still outside the
//! history when the document history is used becomes a step of its own
//! first, so no edit is skipped or lost.

use gpui_kit::component::history::UndoHistory;
use rushes_core::{Block, BlockId};

/// Structural edits kept for Undo.
const MAX_STEPS: usize = 500;

/// The blocks and where the caret was, restored together.
#[derive(Clone, Debug)]
pub(super) struct Snapshot {
    pub blocks: Vec<Block>,
    /// The block with keyboard focus and the caret's byte offset in it.
    pub focus: Option<(BlockId, usize)>,
}

#[derive(Clone, Debug)]
struct Step {
    before: Snapshot,
    after: Snapshot,
}

pub(super) struct DocumentHistory {
    steps: UndoHistory<Step>,
    base: Snapshot,
}

impl DocumentHistory {
    pub fn new(blocks: Vec<Block>) -> Self {
        Self { steps: UndoHistory::new().max_undos(MAX_STEPS), base: Snapshot { blocks, focus: None } }
    }

    /// The text block `id` had when the last step, undo or redo was made.
    pub fn base_text(&self, id: BlockId) -> Option<&str> {
        self.base.blocks.iter().find(|block| block.id == id).map(|block| block.text.as_str())
    }

    /// Records the change from the base to `after` as one step, unless
    /// nothing changed. `after` becomes the base.
    pub fn record(&mut self, after: Snapshot) {
        if after.blocks != self.base.blocks {
            let before = std::mem::replace(&mut self.base, after.clone());
            self.steps.push(Step { before, after });
        } else {
            self.base = after;
        }
    }

    /// Steps back once; returns the document to restore.
    pub fn undo(&mut self) -> Option<Snapshot> {
        // Steps are never grouped, so the transaction holds one step.
        let step = self.steps.undo()?.pop()?;
        self.base = step.before.clone();
        Some(step.before)
    }

    /// Steps forward once; returns the document to restore.
    pub fn redo(&mut self) -> Option<Snapshot> {
        let step = self.steps.redo()?.pop()?;
        self.base = step.after.clone();
        Some(step.after)
    }
}
