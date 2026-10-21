//! Row measurements from the previous frame.
//!
//! The editor does not own its scroll container, so it learns what is visible
//! from the clip of its rows. Rows far outside that area render as spacers of
//! their measured height, keeping layout and paint proportional to the
//! viewport rather than to the document. The same measurements let the editor
//! scroll the focused block into view when the owner shares its scroll handle.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use gpui_kit::{Bounds, Pixels, ScrollHandle, Window};
use rushes_core::{Block, BlockId};

#[derive(Default)]
pub(super) struct Geometry {
    /// Visible area (the clip of the nearest scrolling ancestor) in window
    /// coordinates.
    viewport: Option<Bounds<Pixels>>,
    /// Column width and rem size the rows were measured at.
    width: Pixels,
    rem_size: Pixels,
    rows: HashMap<BlockId, Row>,
    /// Scroll the focused block into view after the next layout.
    reveal: Option<Reveal>,
}

/// How a pending reveal brings the focused block into view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reveal {
    /// The least scrolling that shows the block, as for the caret while typing.
    /// A block too tall to show whole shows the end the caret was put at, if
    /// it was put at one.
    Nearest(Option<CaretEnd>),
    /// The block at the top of the viewport, as for a table of contents.
    Top,
}

/// The end of a block the caret was put at, as by Up and Down from the
/// blocks around it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CaretEnd {
    Start,
    End,
}

#[derive(Clone, Copy)]
struct Row {
    bounds: Bounds<Pixels>,
    /// Everything that affects the row's height except width and rem size.
    fingerprint: u64,
}

/// What a frame rendered for one block, for [`Geometry::record`].
pub(super) struct RenderedRow {
    pub id: BlockId,
    pub fingerprint: u64,
    pub spacer: bool,
}

pub(super) fn fingerprint(block: &Block, number: usize) -> u64 {
    let mut hasher = DefaultHasher::new();
    (block.kind, block.indent, block.checked, number, &block.text).hash(&mut hasher);
    hasher.finish()
}

impl Geometry {
    /// Scrolls the focused block into view after the next layout; `caret`
    /// says which end of it the caret was put at, if it was.
    pub fn request_reveal(&mut self, caret: Option<CaretEnd>) {
        if self.reveal != Some(Reveal::Top) {
            self.reveal = Some(Reveal::Nearest(caret));
        }
    }

    /// Scrolls the focused block to the top of the viewport after the next layout.
    pub fn request_reveal_at_top(&mut self) {
        self.reveal = Some(Reveal::Top);
    }

    pub fn cancel_reveal(&mut self) {
        self.reveal = None;
    }

    /// Where the row was drawn in the last frame, in window coordinates.
    pub fn row_bounds(&self, id: BlockId) -> Option<Bounds<Pixels>> {
        self.rows.get(&id).map(|row| row.bounds)
    }

    /// Height of the spacer to draw instead of a row that was measured with
    /// the same content and lies more than a viewport away from the visible
    /// area, or `None` when the row must be rendered.
    pub fn spacer_height(&self, id: BlockId, fingerprint: u64, rem_size: Pixels) -> Option<Pixels> {
        let viewport = self.viewport?;
        let row = self.rows.get(&id).filter(|row| row.fingerprint == fingerprint && self.rem_size == rem_size)?;
        let margin = viewport.size.height;
        let far = row.bounds.bottom() < viewport.top() - margin || row.bounds.top() > viewport.bottom() + margin;
        far.then_some(row.bounds.size.height)
    }

    /// Stores the bounds of the rows prepainted this frame (`bounds` may end
    /// with trailing non-row children) and scrolls a pending reveal into view,
    /// below the `covered_top` of the viewport that the owner draws over.
    pub fn record(
        &mut self,
        rendered: &[RenderedRow],
        bounds: &[Bounds<Pixels>],
        focused: Option<BlockId>,
        scroll: Option<&ScrollHandle>,
        covered_top: Pixels,
        window: &mut Window,
    ) {
        let viewport = window.content_mask().bounds;
        let rem_size = window.rem_size();
        let width = bounds.first().map_or(Pixels::ZERO, |bounds| bounds.size.width);
        let stale = width != self.width || rem_size != self.rem_size;
        self.width = width;
        self.rem_size = rem_size;
        self.viewport = Some(viewport);
        self.rows = rendered
            .iter()
            .zip(bounds)
            .filter(|(row, _)| !(stale && row.spacer))
            .map(|(row, bounds)| (row.id, Row { bounds: *bounds, fingerprint: row.fingerprint }))
            .collect();

        // A spacer sized for another width, or one scrolled into view, needs
        // its row rendered: draw again with the fresh measurements.
        let spacer_shown =
            rendered.iter().zip(bounds).any(|(row, bounds)| row.spacer && (stale || bounds.intersects(&viewport)));
        if spacer_shown {
            window.refresh();
        }

        if let Some(reveal) = self.reveal.take()
            && let Some(scroll) = scroll
            && let Some(row) = focused.and_then(|id| self.rows.get(&id))
        {
            let margin = rem_size * 2.;
            let top = viewport.top() + covered_top;
            let shown = |edge: Pixels| edge - margin >= top && edge + margin <= viewport.bottom();
            let fits = row.bounds.size.height + margin * 2. <= viewport.bottom() - top;
            let mut offset = scroll.offset();
            let to_top = (offset.y + top - row.bounds.top() + margin).min(Pixels::ZERO);
            let to_bottom = offset.y - (row.bounds.bottom() + margin - viewport.bottom());
            match reveal {
                Reveal::Top => offset.y = to_top,
                // Too tall to show whole: the end with the caret, if it isn't shown.
                Reveal::Nearest(Some(CaretEnd::Start)) if !fits && !shown(row.bounds.top()) => offset.y = to_top,
                Reveal::Nearest(Some(CaretEnd::End)) if !fits && !shown(row.bounds.bottom()) => offset.y = to_bottom,
                Reveal::Nearest(Some(_)) if !fits => {}
                Reveal::Nearest(_) if row.bounds.bottom() + margin > viewport.bottom() => offset.y = to_bottom,
                Reveal::Nearest(_) if row.bounds.top() - margin < top => offset.y = to_top,
                Reveal::Nearest(_) => {}
            }
            if offset != scroll.offset() {
                scroll.set_offset(offset);
                window.refresh();
            }
        }
    }
}
