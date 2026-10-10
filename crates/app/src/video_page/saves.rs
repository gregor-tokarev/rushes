//! Document saves, written on the background executor and retried until
//! they succeed.
//!
//! A [`DocumentTab`](super::DocumentTab) hands its snapshot to [`DocumentSaves`]
//! and forgets about it. The snapshot joins the app's write queue (see
//! [`AppState`]), which keeps the newest snapshot per document, so a slow write
//! never overwrites a later edit, and survives the page that made it: a write
//! that fails while the database is locked (another VideoCRM, a backup or sync
//! tool) is retried, and whatever is still queued when the app quits is written
//! before it exits.

use gpui_kit::{App, AppContext as _, Context, Entity, Global, Subscription};
use videocrm_core::{Document, DocumentId};

pub use crate::state::SAVE_RETRY_DELAY;
use crate::state::{AppState, QueuedWrites};

/// Pending document writes. Observe it to learn when [`Self::is_failing`] changes.
pub struct DocumentSaves {
    state: Entity<AppState>,
    queued: QueuedWrites,
    _subscriptions: [Subscription; 2],
}

struct GlobalSaves(Entity<DocumentSaves>);

impl Global for GlobalSaves {}

impl DocumentSaves {
    /// The app's queue, created on first use.
    pub fn global(cx: &mut App) -> Entity<Self> {
        if let Some(saves) = cx.try_global::<GlobalSaves>() {
            return saves.0.clone();
        }
        let state = AppState::global(cx);
        let saves = cx.new(|cx| Self {
            queued: state.read(cx).queued_writes(),
            _subscriptions: [
                cx.observe(&state, |_, _, cx| cx.notify()),
                // Quitting must not lose a queued edit, even one whose page is gone.
                cx.on_app_quit(|this: &mut Self, cx| {
                    this.write_now(cx);
                    async {}
                }),
            ],
            state,
        });
        cx.set_global(GlobalSaves(saves.clone()));
        saves
    }

    /// Queues `document` and starts writing it in the background.
    pub fn save(&mut self, document: Document, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| state.queue_document(document, cx));
    }

    /// The queued snapshot of a document that has not been written yet.
    pub fn pending(&self, id: DocumentId) -> Option<Document> {
        self.queued.read().document(id)
    }

    /// Whether the last attempt to write the document failed. It stays queued
    /// and is retried.
    pub fn is_failing(&self, id: DocumentId) -> bool {
        self.queued.read().is_document_failing(id)
    }

    /// Writes every queued change now, on the calling thread. Used when the
    /// app quits; waits for a background write that is in flight.
    pub fn write_now(&mut self, cx: &mut Context<Self>) {
        self.state.update(cx, |state, _| state.write_queued_now());
    }
}
