//! Document-level undo: snapshots of the block list before and after each
//! edit, typing included.
//!
//! The editor answers Undo / Redo itself instead of leaving them to the
//! focused block's input, so typing and structural edits (insert, delete,
//! move, convert, duplicate, split, merge, paste) share one history in the
//! order they were made. Consecutive typing of single characters in one
//! block is one step, until the caret moves, a new word starts or the edit
//! turns from inserting to deleting. Any other text change (replacing a
//! selection, pasting, formatting) is a step of its own.

use std::ops::Range;
use std::rc::Rc;

use rushes_core::{Block, BlockId};

/// Steps kept for Undo.
const MAX_STEPS: usize = 500;

/// The block with keyboard focus and the selected byte range in it (empty
/// for a plain caret).
pub(super) type Focus = Option<(BlockId, Range<usize>)>;

/// The blocks and where the caret was, restored together.
#[derive(Clone, Debug)]
pub(super) struct Snapshot {
    pub blocks: Rc<Vec<Block>>,
    pub focus: Focus,
}

#[derive(Clone, Debug)]
struct Step {
    before: Snapshot,
    after: Snapshot,
}

/// What a text change in a block did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Typing {
    Insert,
    Delete,
}

pub(super) struct DocumentHistory {
    undos: Vec<Step>,
    redos: Vec<Step>,
    /// The document as the last step, undo or redo left it, with the caret
    /// where it is now: the `before` of the next step.
    base: Snapshot,
    /// The typing the last step was, which the next text change may extend.
    typing: Option<(BlockId, Typing)>,
}

impl DocumentHistory {
    pub fn new(blocks: Vec<Block>) -> Self {
        Self {
            undos: Vec::new(),
            redos: Vec::new(),
            base: Snapshot { blocks: Rc::new(blocks), focus: None },
            typing: None,
        }
    }

    /// The caret moved without an edit; the next step's undo puts it back here.
    pub fn set_focus(&mut self, focus: Focus) {
        self.base.focus = focus;
    }

    /// The caret as of the last step or [`Self::set_focus`].
    pub fn focus(&self) -> &Focus {
        &self.base.focus
    }

    /// Records a structural edit that left the document as `blocks`, unless
    /// nothing changed.
    pub fn record(&mut self, blocks: &[Block], focus: Focus) {
        self.typing = None;
        if *self.base.blocks == blocks {
            self.base.focus = focus;
            return;
        }
        self.push(blocks, focus);
    }

    /// Records a text change. `typing` is the block and direction of a
    /// single typed or deleted character, which extends the typing step
    /// before it when it continues that typing at the same caret; `None` is
    /// any other change, a step of its own. `word_start` breaks the step so
    /// Undo removes typed text a word at a time.
    pub fn record_text(&mut self, blocks: &[Block], focus: Focus, typing: Option<(BlockId, Typing)>, word_start: bool) {
        let Some(typing) = typing else {
            self.record(blocks, focus);
            return;
        };
        let extends = self.typing == Some(typing)
            && !word_start
            && self.undos.last().is_some_and(|step| step.after.focus == self.base.focus);
        if extends && let Some(step) = self.undos.last_mut() {
            let after = Snapshot { blocks: Rc::new(blocks.to_vec()), focus };
            step.after = after.clone();
            self.base = after;
            self.redos.clear();
            return;
        }
        if *self.base.blocks == blocks {
            return;
        }
        self.push(blocks, focus);
        self.typing = Some(typing);
    }

    fn push(&mut self, blocks: &[Block], focus: Focus) {
        let after = Snapshot { blocks: Rc::new(blocks.to_vec()), focus };
        let before = std::mem::replace(&mut self.base, after.clone());
        self.undos.push(Step { before, after });
        let excess = self.undos.len().saturating_sub(MAX_STEPS);
        self.undos.drain(..excess);
        self.redos.clear();
    }

    /// Steps back once; returns the document to restore.
    pub fn undo(&mut self) -> Option<Snapshot> {
        let step = self.undos.pop()?;
        self.base = step.before.clone();
        self.redos.push(step);
        self.typing = None;
        Some(self.base.clone())
    }

    /// Steps forward once; returns the document to restore.
    pub fn redo(&mut self) -> Option<Snapshot> {
        let step = self.redos.pop()?;
        self.base = step.after.clone();
        self.undos.push(step);
        self.typing = None;
        Some(self.base.clone())
    }
}
