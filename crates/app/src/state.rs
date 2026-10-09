//! Application state: the single owner of persisted data and navigation.
//!
//! Every view reads through [`AppState`] and every mutation goes through it, so
//! the cached rows stay consistent with the database. Background services
//! (pipelines, uploads) write to the [`Store`] directly through [`AppState::store`]
//! and then call [`AppState::reload`] on the foreground thread.

use std::sync::{Arc, Mutex, MutexGuard};

use gpui_kit::*;
use videocrm_core::media;
use videocrm_core::settings::AppSettings;
use videocrm_core::{
    DatabaseView, Document, DocumentKind, NewVideo, PropertyDef, PropertyId, Store, StoreError, Video, VideoId,
    VideoRow, ViewId,
};

use crate::route::{Route, VideoTab};

const RECENT_LIMIT: usize = 8;

/// Shared handle to the database. `Store` is `Send` but not `Sync`.
pub type SharedStore = Arc<Mutex<Store>>;

#[derive(Clone, Debug, PartialEq)]
pub enum AppEvent {
    /// Rows, properties or views were reloaded.
    DataChanged,
    RouteChanged,
    SettingsChanged,
    /// A user-facing error the shell should surface as a notification.
    Error(SharedString),
}

pub struct AppState {
    store: SharedStore,
    settings: AppSettings,
    rows: Vec<VideoRow>,
    properties: Vec<PropertyDef>,
    views: Vec<DatabaseView>,
    route: Route,
    back: Vec<Route>,
    forward: Vec<Route>,
    recent: Vec<VideoId>,
    /// A just-created video whose script takes keyboard focus when its page opens.
    script_focus: Option<VideoId>,
}

impl EventEmitter<AppEvent> for AppState {}

struct GlobalAppState(Entity<AppState>);

impl Global for GlobalAppState {}

impl AppState {
    /// Loads settings and data from `store`. The first route is onboarding until
    /// it has been completed.
    pub fn new(store: Store) -> Result<Self, StoreError> {
        let settings = store.load_settings()?;
        let route = if settings.onboarding_completed { Route::default() } else { Route::Onboarding };
        let mut state = Self {
            store: Arc::new(Mutex::new(store)),
            settings,
            rows: Vec::new(),
            properties: Vec::new(),
            views: Vec::new(),
            route,
            back: Vec::new(),
            forward: Vec::new(),
            recent: Vec::new(),
            script_focus: None,
        };
        state.load()?;
        Ok(state)
    }

    /// Registers `state` as the app-wide instance returned by [`AppState::global`].
    pub fn set_global(state: Entity<AppState>, cx: &mut App) {
        cx.set_global(GlobalAppState(state));
    }

    pub fn global(cx: &App) -> Entity<AppState> {
        cx.global::<GlobalAppState>().0.clone()
    }

    /// The database handle, for background work. Lock it briefly; never hold the
    /// guard across `await` points or long operations.
    pub fn store(&self) -> SharedStore {
        self.store.clone()
    }

    fn lock(&self) -> MutexGuard<'_, Store> {
        // A panic while holding the lock leaves the store usable: SQLite
        // transactions roll back on their own.
        self.store.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn load(&mut self) -> Result<(), StoreError> {
        let store = self.lock();
        let rows = store.list_video_rows()?;
        let properties = store.list_properties()?;
        let views = store.list_views()?;
        drop(store);
        self.rows = rows;
        self.properties = properties;
        self.views = views;
        let known: Vec<VideoId> = self.rows.iter().map(|row| row.video.id).collect();
        self.recent.retain(|id| known.contains(id));
        Ok(())
    }

    /// Re-reads rows, properties and views and notifies observers.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        match self.load() {
            Ok(()) => cx.emit(AppEvent::DataChanged),
            Err(error) => self.report(error, cx),
        }
        cx.notify();
    }

    fn report(&self, error: impl std::fmt::Display, cx: &mut Context<Self>) {
        cx.emit(AppEvent::Error(error.to_string().into()));
    }

    /// Surfaces a user-facing error as [`AppEvent::Error`].
    pub fn report_error(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        cx.emit(AppEvent::Error(message.into()));
    }

    /// Runs a write against the store, then reloads. Errors become [`AppEvent::Error`].
    fn write<T>(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Store) -> Result<T, StoreError>) -> Option<T> {
        let result = f(&mut self.lock());
        match result {
            Ok(value) => {
                self.reload(cx);
                Some(value)
            }
            Err(error) => {
                self.report(error, cx);
                None
            }
        }
    }

    // Settings

    pub fn settings(&self) -> &AppSettings {
        &self.settings
    }

    pub fn update_settings(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut AppSettings)) {
        let mut settings = self.settings.clone();
        f(&mut settings);
        if settings == self.settings {
            return;
        }
        let result = self.lock().save_settings(&settings);
        match result {
            Ok(()) => {
                self.settings = settings;
                cx.emit(AppEvent::SettingsChanged);
                cx.notify();
            }
            Err(error) => self.report(error, cx),
        }
    }

    // Videos

    pub fn rows(&self) -> &[VideoRow] {
        &self.rows
    }

    pub fn row(&self, id: VideoId) -> Option<&VideoRow> {
        self.rows.iter().find(|row| row.video.id == id)
    }

    pub fn video(&self, id: VideoId) -> Option<&Video> {
        self.row(id).map(|row| &row.video)
    }

    /// Creates a video from a template, creates its media folder, and returns its id.
    pub fn create_video(&mut self, new: NewVideo, cx: &mut Context<Self>) -> Option<VideoId> {
        let media_root = self.settings.resolved_media_root();
        let mut store = self.lock();
        let created = store.create_video(new).and_then(|(mut video, _documents)| {
            // A missing folder is not fatal: footage import creates it later.
            if media::ensure_video_folder(&media_root, &mut video).is_ok() {
                store.update_video(&mut video)?;
            }
            Ok(video.id)
        });
        drop(store);
        match created {
            Ok(id) => {
                self.reload(cx);
                Some(id)
            }
            Err(error) => {
                self.report(error, cx);
                None
            }
        }
    }

    pub fn update_video(&mut self, mut video: Video, cx: &mut Context<Self>) {
        self.write(cx, |store| store.update_video(&mut video));
    }

    pub fn delete_videos(&mut self, ids: &[VideoId], cx: &mut Context<Self>) {
        self.write(cx, |store| ids.iter().try_for_each(|id| store.delete_video(*id)));
        let leaves_current_page = matches!(&self.route, Route::Video { id, .. } if ids.contains(id));
        if leaves_current_page {
            self.navigate(Route::default(), cx);
        }
    }

    pub fn duplicate_video(&mut self, id: VideoId, cx: &mut Context<Self>) -> Option<VideoId> {
        self.write(cx, |store| store.duplicate_video(id)).map(|video| video.id)
    }

    /// Ensures the video has a media folder and returns it.
    pub fn ensure_video_folder(&mut self, id: VideoId, cx: &mut Context<Self>) -> Option<std::path::PathBuf> {
        let mut video = self.video(id)?.clone();
        if let Some(folder) = video.folder.clone()
            && folder.is_dir()
        {
            return Some(folder);
        }
        match media::ensure_video_folder(&self.settings.resolved_media_root(), &mut video) {
            Ok(folder) => {
                self.update_video(video, cx);
                Some(folder)
            }
            Err(error) => {
                self.report(error, cx);
                None
            }
        }
    }

    pub fn seed_sample_workspace(&mut self, cx: &mut Context<Self>) {
        self.write(cx, |store| store.seed_sample_workspace());
    }

    // Properties and views

    pub fn properties(&self) -> &[PropertyDef] {
        &self.properties
    }

    pub fn insert_property(&mut self, property: PropertyDef, cx: &mut Context<Self>) {
        self.write(cx, |store| store.insert_property(&property));
    }

    pub fn update_property(&mut self, property: PropertyDef, cx: &mut Context<Self>) {
        self.write(cx, |store| store.update_property(&property));
    }

    pub fn delete_property(&mut self, id: PropertyId, cx: &mut Context<Self>) {
        self.write(cx, |store| store.delete_property(id));
    }

    pub fn views(&self) -> &[DatabaseView] {
        &self.views
    }

    pub fn save_view(&mut self, view: DatabaseView, cx: &mut Context<Self>) {
        self.write(cx, |store| store.save_view(&view));
    }

    pub fn delete_view(&mut self, id: ViewId, cx: &mut Context<Self>) {
        self.write(cx, |store| store.delete_view(id));
        if matches!(self.route, Route::Database { view: Some(view) } if view == id) {
            self.navigate(Route::default(), cx);
        }
    }

    // Documents

    pub fn documents(&self, video_id: VideoId) -> Vec<Document> {
        self.lock().list_documents(video_id).unwrap_or_default()
    }

    pub fn create_document(&mut self, video_id: VideoId, kind: DocumentKind, title: &str, cx: &mut Context<Self>) {
        self.write(cx, |store| store.create_document(video_id, kind, title));
    }

    /// The video's first document of `kind`, created when it has none.
    pub fn ensure_document(
        &mut self,
        video_id: VideoId,
        kind: DocumentKind,
        cx: &mut Context<Self>,
    ) -> Option<Document> {
        let existing = self.lock().list_documents(video_id).map(|documents| {
            documents.into_iter().filter(|document| document.kind == kind).min_by_key(|document| document.position)
        });
        match existing {
            Ok(Some(document)) => Some(document),
            Ok(None) => {
                let title = match kind {
                    DocumentKind::Script => "Script",
                    DocumentKind::Notes => "Notes",
                    DocumentKind::Custom => "Untitled",
                };
                self.write(cx, |store| store.create_document(video_id, kind, title))
            }
            Err(error) => {
                self.report(error, cx);
                None
            }
        }
    }

    /// Persists a document. Script edits change derived row stats, so rows reload.
    pub fn save_document(&mut self, mut document: Document, cx: &mut Context<Self>) {
        self.write(cx, |store| store.save_document(&mut document));
    }

    // Navigation

    pub fn route(&self) -> &Route {
        &self.route
    }

    pub fn navigate(&mut self, route: Route, cx: &mut Context<Self>) {
        if route == self.route {
            return;
        }
        let previous = std::mem::replace(&mut self.route, route);
        self.back.push(previous);
        self.forward.clear();
        self.on_route_changed(cx);
    }

    pub fn open_video(&mut self, id: VideoId, tab: VideoTab, cx: &mut Context<Self>) {
        self.navigate(Route::Video { id, tab }, cx);
    }

    /// Opens a video that was just created on its Script tab, with the caret in
    /// the first block.
    pub fn open_new_video(&mut self, id: VideoId, cx: &mut Context<Self>) {
        self.script_focus = Some(id);
        self.open_video(id, VideoTab::Script, cx);
    }

    /// Whether the script of `id` should take focus when shown; asking clears the request.
    pub fn take_script_focus(&mut self, id: VideoId) -> bool {
        let requested = self.script_focus == Some(id);
        if requested {
            self.script_focus = None;
        }
        requested
    }

    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    pub fn go_back(&mut self, cx: &mut Context<Self>) {
        if let Some(route) = self.back.pop() {
            let current = std::mem::replace(&mut self.route, route);
            self.forward.push(current);
            self.on_route_changed(cx);
        }
    }

    pub fn go_forward(&mut self, cx: &mut Context<Self>) {
        if let Some(route) = self.forward.pop() {
            let current = std::mem::replace(&mut self.route, route);
            self.back.push(current);
            self.on_route_changed(cx);
        }
    }

    fn on_route_changed(&mut self, cx: &mut Context<Self>) {
        if let Route::Video { id, .. } = self.route {
            self.recent.retain(|recent| *recent != id);
            self.recent.insert(0, id);
            self.recent.truncate(RECENT_LIMIT);
        }
        cx.emit(AppEvent::RouteChanged);
        cx.notify();
    }

    /// Recently opened videos, most recent first.
    pub fn recent_videos(&self) -> Vec<&Video> {
        self.recent.iter().filter_map(|id| self.video(*id)).collect()
    }

    /// Marks onboarding as done and opens the database.
    pub fn complete_onboarding(&mut self, cx: &mut Context<Self>) {
        self.update_settings(cx, |settings| settings.onboarding_completed = true);
        self.back.clear();
        self.forward.clear();
        self.route = Route::default();
        self.on_route_changed(cx);
    }
}
