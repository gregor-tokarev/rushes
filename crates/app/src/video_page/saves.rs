//! Document saves, written on the background executor and retried until
//! they succeed.
//!
//! A [`DocumentTab`](super::DocumentTab) hands its snapshot to [`DocumentSaves`]
//! and forgets about it. The queue keeps the newest snapshot per document, so a
//! slow write never overwrites a later edit, and the snapshot survives the page
//! that made it: a write that fails while the database is locked (another
//! VideoCRM, a backup or sync tool) is retried, and whatever is still queued
//! when the app quits is written before it exits.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use gpui_kit::{App, AppContext as _, Context, Entity, Global, Subscription, Task};
use videocrm_core::{Document, DocumentId, DocumentKind, StoreError, VideoId};

use crate::state::{AppState, SharedStore};

/// How long a failed write waits before the next attempt.
pub const SAVE_RETRY_DELAY: Duration = Duration::from_secs(2);

/// Pending document writes. Observe it to learn when [`Self::is_failing`] changes.
pub struct DocumentSaves {
    state: Entity<AppState>,
    store: SharedStore,
    pending: Arc<Mutex<Pending>>,
    /// The background write in flight, or the wait before a retry.
    worker: Option<Task<()>>,
    /// Documents whose last write failed; they stay queued.
    failing: HashMap<DocumentId, StoreError>,
    /// Whether the user has been told that saving fails, so retries stay quiet.
    reported: bool,
    _quit: Subscription,
}

#[derive(Default)]
struct Pending {
    documents: HashMap<DocumentId, Queued>,
    revision: u64,
}

struct Queued {
    revision: u64,
    document: Document,
}

/// The outcome of one pass over the queue.
#[derive(Default)]
struct Written {
    saved: Vec<DocumentId>,
    failed: Vec<(DocumentId, VideoId, StoreError)>,
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
        let saves = cx.new(|cx| {
            let store = state.read(cx).store();
            Self {
                state,
                store,
                pending: Arc::default(),
                worker: None,
                failing: HashMap::new(),
                reported: false,
                // Quitting must not lose a queued edit, even one whose page is gone.
                _quit: cx.on_app_quit(|this: &mut Self, cx| {
                    this.write_now(cx);
                    async {}
                }),
            }
        });
        cx.set_global(GlobalSaves(saves.clone()));
        saves
    }

    /// Queues `document` and starts writing it in the background.
    pub fn save(&mut self, document: Document, cx: &mut Context<Self>) {
        {
            let mut pending = lock(&self.pending);
            pending.revision += 1;
            let revision = pending.revision;
            pending.documents.insert(document.id, Queued { revision, document });
        }
        self.start(cx);
    }

    /// The queued snapshot of a document that has not been written yet.
    pub fn pending(&self, id: DocumentId) -> Option<Document> {
        lock(&self.pending).documents.get(&id).map(|queued| queued.document.clone())
    }

    /// Whether the last attempt to write the document failed. It stays queued
    /// and is retried.
    pub fn is_failing(&self, id: DocumentId) -> bool {
        self.failing.contains_key(&id)
    }

    /// Writes every queued document now, on the calling thread. Used when the
    /// app quits; waits for a background write that is in flight.
    pub fn write_now(&mut self, cx: &mut Context<Self>) {
        let written = write_pending(&self.store, &self.pending);
        self.finish(written, cx);
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        if self.worker.is_some() {
            return;
        }
        let (store, pending) = (self.store.clone(), self.pending.clone());
        self.worker = Some(cx.spawn(async move |this, cx| {
            let written = cx.background_spawn(async move { write_pending(&store, &pending) }).await;
            _ = this.update(cx, |this, cx| {
                this.worker = None;
                this.finish(written, cx);
            });
        }));
    }

    fn finish(&mut self, written: Written, cx: &mut Context<Self>) {
        for id in &written.saved {
            self.failing.remove(id);
        }
        let mut failed = false;
        for (id, video_id, error) in written.failed {
            // The video was deleted meanwhile: its documents went with it.
            if self.state.read(cx).video(video_id).is_none() {
                lock(&self.pending).documents.remove(&id);
                self.failing.remove(&id);
                continue;
            }
            failed = true;
            self.failing.insert(id, error);
        }
        if !written.saved.is_empty() {
            // Script stats in the database rows follow the saved text.
            self.state.update(cx, |state, cx| state.reload(cx));
        }
        if failed {
            self.report(cx);
            if self.worker.is_none() {
                self.worker = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(SAVE_RETRY_DELAY).await;
                    _ = this.update(cx, |this, cx| {
                        this.worker = None;
                        this.start(cx);
                    });
                }));
            }
        } else {
            self.reported = false;
            // Edits queued while the write was in flight.
            if !lock(&self.pending).documents.is_empty() {
                self.start(cx);
            }
        }
        cx.notify();
    }

    /// Tells the user once that saving fails; the page shows it until a retry succeeds.
    fn report(&mut self, cx: &mut Context<Self>) {
        if std::mem::replace(&mut self.reported, true) {
            return;
        }
        let kinds: Vec<DocumentKind> = {
            let pending = lock(&self.pending);
            self.failing.keys().filter_map(|id| pending.documents.get(id).map(|queued| queued.document.kind)).collect()
        };
        let what = match kinds.as_slice() {
            [DocumentKind::Script] => "the script",
            [DocumentKind::Notes] => "the notes",
            _ => "your writing",
        };
        let busy = self.failing.values().any(is_busy);
        let message = if busy {
            format!(
                "Couldn’t save {what}: another program is using the database. VideoCRM keeps trying and saves it when the database is free."
            )
        } else {
            format!("Couldn’t save {what}. VideoCRM keeps trying; keep the app open until it shows as saved.")
        };
        self.state.update(cx, |state, cx| state.report_error(message, cx));
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn is_busy(error: &StoreError) -> bool {
    error.to_string().contains("locked") || error.to_string().contains("busy")
}

/// Writes the newest snapshot of every queued document. The store is locked
/// first and the queue read under it, so of two passes the later one always
/// writes the later snapshot. A document leaves the queue only when the
/// snapshot written is still its newest.
fn write_pending(store: &SharedStore, pending: &Mutex<Pending>) -> Written {
    let mut store = lock(store);
    let queued: Vec<(u64, Document)> =
        lock(pending).documents.values().map(|queued| (queued.revision, queued.document.clone())).collect();
    let mut written = Written::default();
    for (revision, mut document) in queued {
        let (id, video_id) = (document.id, document.video_id);
        match store.save_document(&mut document) {
            Ok(()) => {
                let mut pending = lock(pending);
                if pending.documents.get(&id).is_some_and(|queued| queued.revision == revision) {
                    pending.documents.remove(&id);
                }
                written.saved.push(id);
            }
            Err(error) => written.failed.push((id, video_id, error)),
        }
    }
    written
}
