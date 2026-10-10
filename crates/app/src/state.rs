//! Application state: the single owner of persisted data and navigation.
//!
//! Every view reads through [`AppState`] and every mutation goes through it, so
//! the cached rows stay consistent with the database. Background services
//! (pipelines, uploads) write to the [`Store`] directly through [`AppState::store`]
//! and then call [`AppState::reload`] on the foreground thread.

mod problem;

use std::sync::{Arc, Mutex, MutexGuard};

use gpui_kit::*;
use videocrm_core::media;
use videocrm_core::settings::AppSettings;
use videocrm_core::{
    DatabaseView, Document, DocumentKind, NewVideo, PropertyDef, PropertyId, Store, StoreError, Video, VideoId,
    VideoRow, ViewId,
};

use crate::route::{Route, SettingsSection, VideoTab};

pub(crate) use problem::folder_error_reason;
pub use problem::{Problem, Remedy};

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
    /// Problems found while loading, shown once the window is open.
    startup_problems: Vec<Problem>,
}

impl EventEmitter<AppEvent> for AppState {}

struct GlobalAppState(Entity<AppState>);

impl Global for GlobalAppState {}

impl AppState {
    /// Loads settings and data from `store`. The first route is onboarding until
    /// it has been completed.
    pub fn new(store: Store) -> Result<Self, StoreError> {
        let (settings, settings_problem) = load_settings(&store)?;
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
            startup_problems: settings_problem.into_iter().collect(),
        };
        state.load()?;
        Ok(state)
    }

    /// Problems found while loading; asking clears them.
    pub fn take_startup_problems(&mut self) -> Vec<Problem> {
        std::mem::take(&mut self.startup_problems)
    }

    /// Registers `state` as the app-wide instance returned by [`AppState::global`].
    pub fn set_global(state: Entity<AppState>, cx: &mut App) {
        cx.set_global(GlobalAppState(state));
    }

    pub fn global(cx: &App) -> Entity<AppState> {
        cx.global::<GlobalAppState>().0.clone()
    }

    /// The database handle, for background work. Lock it briefly; never hold the
    /// guard across `await` points or long operations. Change video rows through
    /// [`AppState::update_video`] on the foreground instead: pages save whole rows
    /// from the cache, so a direct write is lost if an edit lands before the reload.
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
        self.prune_history(&known);
        Ok(())
    }

    /// Drops the history entries of videos that no longer exist, and the repeats
    /// that leaves, so Back and Forward never land on a missing page.
    fn prune_history(&mut self, known: &[VideoId]) {
        let exists = |route: &Route| !matches!(route, Route::Video { id, .. } if !known.contains(id));
        for stack in [&mut self.back, &mut self.forward] {
            stack.retain(exists);
            stack.dedup();
            while stack.last() == Some(&self.route) {
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

    /// Runs a write against the store, then reloads. Errors become [`AppEvent::Problem`].
    fn write<T>(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Store) -> Result<T, StoreError>) -> Option<T> {
        let result = f(&mut self.lock());
        match result {
            Ok(value) => {
                self.reload(cx);
                Some(value)
            }
            Err(error) => {
                self.report_problem(Problem::store("Couldn’t save your changes", &error), cx);
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
            Err(error) => self.report_problem(Problem::store("Couldn’t save the settings", &error), cx),
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
        let mut folder_error = None;
        let created = store.create_video(new).and_then(|(mut video, _documents)| {
            // A missing folder is not fatal: footage import creates it later.
            match media::ensure_video_folder(&media_root, &mut video) {
                Ok(_) => store.update_video(&mut video)?,
                Err(error) => folder_error = Some(error),
            }
            Ok(video.id)
        });
        drop(store);
        match created {
            Ok(id) => {
                self.reload(cx);
                if let Some(error) = folder_error {
                    self.report_problem(Problem::media(&media_root, &error), cx);
                }
                Some(id)
            }
            Err(error) => {
                self.report_problem(Problem::store("Couldn’t create the video", &error), cx);
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
            // The deleted page is replaced, not kept in the history.
            self.route = Route::default();
            let known: Vec<VideoId> = self.rows.iter().map(|row| row.video.id).collect();
            self.prune_history(&known);
            self.on_route_changed(cx);
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
                self.report_problem(Problem::store("Couldn’t open the document", &error), cx);
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
    /// just created, or one opened from the keyboard to write.
    pub fn open_script(&mut self, id: VideoId, cx: &mut Context<Self>) {
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
    eprintln!("videocrm: some settings can't be read, using their defaults: {error}");
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
