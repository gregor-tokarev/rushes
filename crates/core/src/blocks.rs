//! Shorthand block builders for the sample workspace.

use crate::model::{Block, BlockKind};

pub(crate) fn h2(text: &str) -> Block {
    Block::new(BlockKind::Heading2, text)
}

pub(crate) fn h3(text: &str) -> Block {
    Block::new(BlockKind::Heading3, text)
}

pub(crate) fn p(text: &str) -> Block {
    Block::paragraph(text)
}

pub(crate) fn bullet(text: &str) -> Block {
    Block::new(BlockKind::Bulleted, text)
}

pub(crate) fn callout(text: &str) -> Block {
    Block::new(BlockKind::Callout, text)
}

pub(crate) fn talking_point(text: &str) -> Block {
    Block::new(BlockKind::TalkingPoint, text)
}

pub(crate) fn broll(text: &str) -> Block {
    Block::new(BlockKind::BRoll, text)
}

pub(crate) fn onscreen(text: &str) -> Block {
    Block::new(BlockKind::OnScreen, text)
}

pub(crate) fn todo(text: &str) -> Block {
    Block::new(BlockKind::Todo, text)
}
