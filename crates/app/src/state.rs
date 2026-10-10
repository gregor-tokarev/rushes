//! Application state: the single owner of persisted data and navigation.
//!
//! Every view reads through [`AppState`] and every mutation goes through it, so
//! the cached rows stay consistent with the database. Changes are written at
//! once, or queued and retried in the background while another program holds
//! the database (see [`writes`]); the UI thread never waits for a lock.
//! Background services (pipelines, uploads) keep their records in memory and
//! save them through the same queue (see [`records`]), then call
//! [`AppState::reload`] for the rows that summarize them.

mod problem;
mod records;
mod writes;

use std::sync::{Arc, Mutex, MutexGuard};

use gpui_kit::*;
use videocrm_core::media;
use videocrm_core::settings::AppSettings;
use videocrm_core::store::{blank_document, copy_of};
use videocrm_core::{
    DatabaseView, Document, DocumentKind, NewVideo, PropertyDef, PropertyId, Store, StoreError, Video, VideoId,
    VideoRow, ViewId,
};

use crate::route::{Route, SettingsSection, VideoTab};

pub(crate) use problem::folder_error_reason;
pub use problem::{Problem, Remedy, Severity};
pub(crate) use writes::QueuedWrites;
use writes::{Cached, Write, Writes};
pub use writes::{QUIT_SAVE_TIMEOUT, SAVE_RETRY_DELAY, UI_BUSY_TIMEOUT, WRITE_BUSY_TIMEOUT};

const RECENT_LIMIT: usize = 8;
/// The key [`Store::load_settings`] reads, for reading what it can't parse.
const APP_SETTINGS_KEY: &str = "app";

/// Shared handle to the database. `Store` is `Send` but not `Sync`.
pub type SharedStore = Arc<Mutex<Store>>;

#[derive(Clone, Debug, PartialEq)]
pub enum AppEvent {
    /// Rows, properties or views were reloaded.
    DataChanged,
    RouteChanged,
    SettingsChanged,
    /// A user-facing error message the shell should surface as a notification.
    Error(SharedString),
    /// A user-facing error with a title and possibly a remedy, surfaced as a notification.
    Problem(Problem),
}

pub struct AppState {
    /// The UI thread's connection: reads, and writes while nothing is queued.
    /// Waits at most [`UI_BUSY_TIMEOUT`] for another program's lock.
    ui: SharedStore,
    /// A connection for code outside the write queue, see [`AppState::store`].
    services: SharedStore,
    /// Writes queued for the background writer, which has its own connection.
    writes: Writes,
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
    /// Problems found while loading, shown once the window is open.
    startup_problems: Vec<Problem>,
}

impl EventEmitter<AppEvent> for AppState {}

struct GlobalAppState(Entity<AppState>);

impl Global for GlobalAppState {}

impl AppState {
    /// Loads settings and data from `store`. The first route is onboarding until
    /// it has been completed. A database file gets two more connections, for
    /// the background writer and for [`AppState::store`]; an in-memory one is
    /// shared by all three.
    pub fn new(store: Store) -> Result<Self, StoreError> {
        let database = store.path();
        let services = store.reopen()?;
        let writer = store.reopen()?;
        let ui = Arc::new(Mutex::new(store));
        let shared = |store: Option<Store>| store.map_or_else(|| ui.clone(), |store| Arc::new(Mutex::new(store)));
        let (services, writer) = (shared(services), shared(writer));
        // Changes kept at the last quit are written first, so the settings read
        // next are the ones the creator chose last; those that still wait for
        // the database are used as queued.
        let (writes, unsaved_problem) = Writes::new(writer, database.as_deref());
        let store = ui.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (stored, settings_problem) = load_settings(&store)?;
        store.set_busy_timeout(UI_BUSY_TIMEOUT)?;
        drop(store);
        let queued = writes.queued().read().settings().cloned();
        let settings_problem = settings_problem.filter(|_| queued.is_none());
        let settings = queued.unwrap_or(stored);
        let route = if settings.onboarding_completed { Route::default() } else { Route::Onboarding };
        let mut state = Self {
            ui,
            services,
            writes,
            settings,
            rows: Vec::new(),
            properties: Vec::new(),
            views: Vec::new(),
            route,
            back: Vec::new(),
            forward: Vec::new(),
            recent: Vec::new(),
            script_focus: None,
            startup_problems: settings_problem.into_iter().chain(unsaved_problem).collect(),
        };
        state.load()?;
        Ok(state)
    }

    /// Problems found while loading; asking clears them.
    pub fn take_startup_problems(&mut self) -> Vec<Problem> {
        std::mem::take(&mut self.startup_problems)
    }

    /// Registers `state` as the app-wide instance returned by [`AppState::global`],
    /// and starts writing what is queued. From now on, quitting writes the queue
    /// first, before the quit hooks registered later queue their last edits.
    pub fn set_global(state: Entity<AppState>, cx: &mut App) {
        state.update(cx, |state, cx| state.start_writes(cx));
        cx.set_global(GlobalAppState(state));
    }

    pub fn global(cx: &App) -> Entity<AppState> {
        cx.global::<GlobalAppState>().0.clone()
    }

    /// A connection of its own to the database, for reading or writing it
    /// outside the app's write queue, as tests do. It waits up to 5 s for
    /// another program's lock: never use it on the UI thread. The app's
    /// services save through [`AppState::save_run`], [`AppState::save_upload`]
    /// and the like, and video rows change through [`AppState::update_video`]:
    /// pages save whole rows from the cache, so a direct write is lost if an
    /// edit lands before the reload.
    pub fn store(&self) -> SharedStore {
        self.services.clone()
    }

    /// The UI thread's connection.
    fn lock(&self) -> MutexGuard<'_, Store> {
        // A panic while holding the lock leaves the store usable: SQLite
        // transactions roll back on their own.
        self.ui.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn cached(&mut self) -> Cached<'_> {
        Cached { rows: &mut self.rows, properties: &mut self.properties, views: &mut self.views }
    }

    /// Reads rows, properties and views, with the queued writes applied.
    fn load(&mut self) -> Result<(), StoreError> {
        let store = self.lock();
        let rows = store.list_video_rows()?;
        let properties = store.list_properties()?;
        let views = store.list_views()?;
        drop(store);
        self.rows = rows;
        self.properties = properties;
        self.views = views;
        let mut cached = Cached { rows: &mut self.rows, properties: &mut self.properties, views: &mut self.views };
        self.writes.overlay(&mut cached);
        let known: Vec<VideoId> = self.rows.iter().map(|row| row.video.id).collect();
        self.recent.retain(|id| known.contains(id));
        self.prune_history(&known);
        Ok(())
    }

    /// Drops the history entries of videos that no longer exist, and the repeats
    /// that leaves, so Back and Forward never land on a missing page or one that
    /// looks like the page they leave.
    fn prune_history(&mut self, known: &[VideoId]) {
        let exists = |route: &Route| !matches!(route, Route::Video { id, .. } if !known.contains(id));
        let current = shown_route(&self.route, &self.views);
        for stack in [&mut self.back, &mut self.forward] {
            stack.retain(exists);
            stack.dedup_by(|a, b| shown_route(a, &self.views) == shown_route(b, &self.views));
            while stack.last().is_some_and(|last| shown_route(last, &self.views) == current) {
                stack.pop();
            }
        }
    }

    /// Re-reads rows, properties and views and notifies observers.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        match self.load() {
            Ok(()) => cx.emit(AppEvent::DataChanged),
            Err(error) => self.report_problem(Problem::store("Couldn’t load your videos", &error), cx),
        }
        cx.notify();
    }

    /// Surfaces a user-facing error message as [`AppEvent::Error`].
    pub fn report_error(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        cx.emit(AppEvent::Error(message.into()));
    }

    /// Surfaces a problem as [`AppEvent::Problem`]. Build it with [`Problem::store`]
    /// or [`Problem::media`] to turn a library error into readable copy.
    pub fn report_problem(&mut self, problem: Problem, cx: &mut Context<Self>) {
        cx.emit(AppEvent::Problem(problem));
    }

    /// Whether changes to the video wait for a retry because the database
    /// refused them; they are kept and written once it accepts them.
    pub fn is_save_failing(&self, id: VideoId) -> bool {
        self.writes.is_video_failing(id)
    }

    /// The writes not in the database yet, readable without a context.
    pub(crate) fn queued_writes(&self) -> QueuedWrites {
        self.writes.queued()
    }

    /// Whether changes wait to be written.
    pub fn has_unsaved_changes(&self) -> bool {
        !self.writes.is_empty()
    }

    /// Writes what is queued now, on the calling thread, waiting at most
    /// [`QUIT_SAVE_TIMEOUT`] for a lock. For quitting: what can't be written
    /// is kept and written at the next start.
    pub fn write_queued_now(&mut self) {
        self.write_before_quit();
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
        if self.save(Write::Settings(settings.clone()), cx) {
            self.settings = settings;
            cx.emit(AppEvent::SettingsChanged);
            cx.notify();
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
        let last = self.rows.iter().map(|row| row.video.position).reduce(f64::max);
        let (mut video, documents) = new.build(last.map_or(0.0, |last| last + 1.0));
        // A missing folder is not fatal: footage import creates it later.
        let folder_error = media::ensure_video_folder(&media_root, &mut video).err();
        let id = video.id;
        if !self.save(Write::CreateVideo { video, documents }, cx) {
            return None;
        }
        if let Some(error) = folder_error {
            self.report_problem(Problem::media(&media_root, &error), cx);
        }
        Some(id)
    }

    pub fn update_video(&mut self, video: Video, cx: &mut Context<Self>) {
        self.save(Write::Video(video), cx);
    }

    pub fn delete_videos(&mut self, ids: &[VideoId], cx: &mut Context<Self>) {
        self.save(Write::DeleteVideos(ids.to_vec()), cx);
        let leaves_current_page = matches!(&self.route, Route::Video { id, .. } if ids.contains(id));
        if leaves_current_page {
            // The deleted page is replaced by the page the creator came from, so
            // the next Back shows another page rather than this one again. Its
            // other tabs leave the history first: a delete that waits in the
            // queue gets no reload that would drop them.
            let known: Vec<VideoId> = self.rows.iter().map(|row| row.video.id).collect();
            self.prune_history(&known);
            self.route = self.back.pop().unwrap_or_default();
            self.prune_history(&known);
            self.on_route_changed(cx);
        }
    }

    /// Copies a video right below it, with its documents; see [`copy_of`].
    pub fn duplicate_video(&mut self, id: VideoId, cx: &mut Context<Self>) -> Option<VideoId> {
        let original = self.video(id)?.clone();
        let documents = match self.read_documents(id) {
            Ok(documents) => documents,
            Err(error) => {
                self.report_problem(Problem::store("Couldn’t duplicate the video", &error), cx);
                return None;
            }
        };
        let next = self.rows.iter().map(|row| row.video.position).filter(|&p| p > original.position).reduce(f64::min);
        let (copy, documents) = copy_of(&original, documents, next);
        let copy_id = copy.id;
        self.save(Write::CreateVideo { video: copy, documents }, cx).then_some(copy_id)
    }

    /// Ensures the video has a media folder and returns it.
    pub fn ensure_video_folder(&mut self, id: VideoId, cx: &mut Context<Self>) -> Option<std::path::PathBuf> {
        let mut video = self.video(id)?.clone();
        if let Some(folder) = video.folder.clone()
            && folder.is_dir()
        {
            return Some(folder);
        }
        let media_root = self.settings.resolved_media_root();
        match media::ensure_video_folder(&media_root, &mut video) {
            Ok(folder) => {
                self.update_video(video, cx);
                Some(folder)
            }
            Err(error) => {
                self.report_problem(Problem::media(&media_root, &error), cx);
                None
            }
        }
    }

    pub fn seed_sample_workspace(&mut self, cx: &mut Context<Self>) {
        self.save(Write::SeedSample, cx);
    }

    // Properties and views

    pub fn properties(&self) -> &[PropertyDef] {
        &self.properties
    }

    pub fn insert_property(&mut self, property: PropertyDef, cx: &mut Context<Self>) {
        self.save(Write::InsertProperty(property), cx);
    }

    pub fn update_property(&mut self, property: PropertyDef, cx: &mut Context<Self>) {
        self.save(Write::UpdateProperty(property), cx);
    }

    pub fn delete_property(&mut self, id: PropertyId, cx: &mut Context<Self>) {
        self.save(Write::DeleteProperty(id), cx);
    }

    pub fn views(&self) -> &[DatabaseView] {
        &self.views
    }

    pub fn save_view(&mut self, view: DatabaseView, cx: &mut Context<Self>) {
        self.save(Write::SaveView(view), cx);
    }

    pub fn delete_view(&mut self, id: ViewId, cx: &mut Context<Self>) {
        self.save(Write::DeleteView(id), cx);
        if matches!(self.route, Route::Database { view: Some(view) } if view == id) {
            // The first view takes the deleted one's place, in the history too.
            self.route = Route::default();
            self.on_route_changed(cx);
        }
    }

    // Documents

    /// The video's documents, with the edits not written yet.
    pub fn documents(&self, video_id: VideoId) -> Vec<Document> {
        self.read_documents(video_id).unwrap_or_default()
    }

    fn read_documents(&self, video_id: VideoId) -> Result<Vec<Document>, StoreError> {
        let mut documents = self.lock().list_documents(video_id)?;
        self.writes.overlay_documents(video_id, &mut documents);
        Ok(documents)
    }

    /// Adds an empty document after the video's existing ones.
    pub fn create_document(
        &mut self,
        video_id: VideoId,
        kind: DocumentKind,
        title: &str,
        cx: &mut Context<Self>,
    ) -> Option<Document> {
        let documents = match self.read_documents(video_id) {
            Ok(documents) => documents,
            Err(error) => {
                self.report_problem(Problem::store("Couldn’t create the document", &error), cx);
                return None;
            }
        };
        let position = documents.iter().map(|document| document.position + 1).max().unwrap_or(0);
        let document = blank_document(video_id, kind, title, position);
        self.save(Write::Document(document.clone()), cx).then_some(document)
    }

    /// The video's first document of `kind`, created when it has none.
    pub fn ensure_document(
        &mut self,
        video_id: VideoId,
        kind: DocumentKind,
        cx: &mut Context<Self>,
    ) -> Option<Document> {
        let existing = self.read_documents(video_id).map(|documents| {
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
                self.create_document(video_id, kind, title, cx)
            }
            Err(error) => {
                self.report_problem(Problem::store("Couldn’t open the document", &error), cx);
                None
            }
        }
    }

    /// Persists a document. Script edits change derived row stats, so rows reload.
    pub fn save_document(&mut self, document: Document, cx: &mut Context<Self>) {
        self.save(Write::Document(document), cx);
    }

    /// Queues a document for the background writer, as typing does: it never
    /// waits for the database on the UI thread.
    pub fn queue_document(&mut self, document: Document, cx: &mut Context<Self>) {
        self.queue(Write::Document(document), cx);
    }

    // Navigation

    pub fn route(&self) -> &Route {
        &self.route
    }

    pub fn navigate(&mut self, route: Route, cx: &mut Context<Self>) {
        if shown_route(&route, &self.views) == shown_route(&self.route, &self.views) {
            return;
        }
        let previous = std::mem::replace(&mut self.route, route);
        self.back.push(previous);
        self.forward.clear();
        self.on_route_changed(cx);
    }

    /// Shows `section` of the settings, also when the settings are already open
    /// on another section (the route does not follow the settings' own list).
    pub fn open_settings(&mut self, section: SettingsSection, cx: &mut Context<Self>) {
        let route = Route::Settings { section };
        if self.route == route {
            self.on_route_changed(cx);
        } else {
            self.navigate(route, cx);
        }
    }

    pub fn open_video(&mut self, id: VideoId, tab: VideoTab, cx: &mut Context<Self>) {
        self.navigate(Route::Video { id, tab }, cx);
    }

    /// Opens a video on its Script tab with the caret in the first block: one
    /// just created, or one opened from the keyboard or the command palette to
    /// write. A script already shown takes the caret too.
    pub fn open_script(&mut self, id: VideoId, cx: &mut Context<Self>) {
        self.script_focus = Some(id);
        let route = Route::Video { id, tab: VideoTab::Script };
        if self.route == route {
            self.on_route_changed(cx);
        } else {
            self.navigate(route, cx);
        }
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

/// `route` as the main area shows it: the database names the view it shows,
/// which is the first one when the route names none or one that was deleted.
/// Routes to one page compare equal this way.
fn shown_route(route: &Route, views: &[DatabaseView]) -> Route {
    match route {
        Route::Database { view } => {
            let view = view
                .filter(|id| views.iter().any(|known| known.id == *id))
                .or_else(|| views.iter().min_by_key(|known| known.position).map(|known| known.id));
            Route::Database { view }
        }
        route => route.clone(),
    }
}

/// The stored settings. Values this version can't read (an unknown theme from a
/// newer VideoCRM, a field of the wrong type) fall back to their defaults one
/// by one instead of keeping the creator from their videos; the stored JSON is
/// left alone until a setting is changed.
fn load_settings(store: &Store) -> Result<(AppSettings, Option<Problem>), StoreError> {
    let error = match store.load_settings() {
        Ok(settings) => return Ok((settings, None)),
        Err(error @ StoreError::Sqlite(rusqlite::Error::FromSqlConversionFailure(..))) => error,
        Err(error) => return Err(error),
    };
    log_line!("videocrm: some settings can't be read, using their defaults: {error}");
    let stored = match store.get_setting::<serde_json::Value>(APP_SETTINGS_KEY) {
        Ok(stored) => stored.unwrap_or_default(),
        Err(StoreError::Sqlite(rusqlite::Error::FromSqlConversionFailure(..))) => serde_json::Value::Null,
        Err(error) => return Err(error),
    };
    let problem = Problem::new(
        "Some settings couldn’t be read",
        "Another version of VideoCRM saved them, so they use their defaults for now. Your videos aren’t affected.",
    )
    .with_remedy(Remedy::OpenSettings(SettingsSection::General));
    Ok((readable_settings(stored), Some(problem)))
}

/// `stored` with every value [`AppSettings`] can't read replaced by its default.
fn readable_settings(stored: serde_json::Value) -> AppSettings {
    use serde_json::{Map, Value};

    /// Copies the readable values of `stored` into the object at `parent` in `merged`.
    fn merge(merged: &mut Value, parent: &str, stored: Map<String, Value>) {
        for (key, value) in stored {
            let mut candidate = merged.clone();
            let Some(object) = candidate.pointer_mut(parent).and_then(Value::as_object_mut) else { return };
            object.insert(key.clone(), value.clone());
            if serde_json::from_value::<AppSettings>(candidate.clone()).is_ok() {
                *merged = candidate;
                continue;
            }
            // The readable fields of an unreadable group, such as the pipeline settings.
            let child = format!("{parent}/{}", key.replace('~', "~0").replace('/', "~1"));
            if let Value::Object(inner) = value
                && merged.pointer(&child).is_some_and(Value::is_object)
            {
                merge(merged, &child, inner);
            }
        }
    }

    let mut merged = serde_json::to_value(AppSettings::default()).unwrap_or_default();
    if let Value::Object(stored) = stored {
        merge(&mut merged, "", stored);
    }
    serde_json::from_value(merged).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use videocrm_core::Store;
    use videocrm_core::settings::ThemeMode;

    // Not a glob: the parent's `gpui_kit::*` would bring GPUI's `test` macro.
    use super::{APP_SETTINGS_KEY, load_settings, readable_settings};

    #[test]
    fn unreadable_settings_fall_back_one_by_one() {
        let stored = json!({
            "media_root": "/srv/media",
            "onboarding_completed": true,
            "theme": "high_contrast",
            "pipeline": { "agent_command": "my-agent", "auto_approve": "sometimes" },
        });
        let settings = readable_settings(stored);
        assert_eq!(settings.media_root.as_deref(), Some(std::path::Path::new("/srv/media")));
        assert!(settings.onboarding_completed);
        assert_eq!(settings.theme, ThemeMode::default());
        assert_eq!(settings.pipeline.agent_command, "my-agent");
        assert!(!settings.pipeline.auto_approve);
    }

    #[test]
    fn a_store_with_unreadable_settings_still_loads() {
        let mut store = Store::open_in_memory().expect("store");
        store
            .set_setting(APP_SETTINGS_KEY, &json!({ "onboarding_completed": true, "theme": "high_contrast" }))
            .expect("write settings");
        let (settings, problem) = load_settings(&store).expect("settings load");
        assert!(settings.onboarding_completed);
        assert!(problem.is_some());
        // Nothing is written back until a setting changes.
        let stored: Option<serde_json::Value> = store.get_setting(APP_SETTINGS_KEY).expect("read settings");
        assert_eq!(
            stored.and_then(|stored| stored["theme"].as_str().map(str::to_string)).as_deref(),
            Some("high_contrast")
        );
    }
}
