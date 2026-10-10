//! The background services' records: AI edit runs and their logs, uploads,
//! and settings of their own. The services keep them in memory and save them
//! through the write queue, as every other change: neither a running edit nor
//! an upload makes the UI thread wait for another program's lock. Reading them
//! uses the UI thread's connection, which doesn't wait for writers, with the
//! queued writes applied.

use gpui_kit::Context;
use serde::Serialize;
use serde::de::DeserializeOwned;
use videocrm_core::{PipelineRun, RunEvent, RunId, Store, StoreError, UploadJob, VideoId};

use super::AppState;
use super::writes::{Queue, Write};

impl AppState {
    /// Saves a new AI edit run. `false` when the database refused it, which is reported.
    pub fn insert_run(&mut self, run: PipelineRun, cx: &mut Context<Self>) -> bool {
        self.save(Write::InsertRun(run), cx)
    }

    /// Saves an AI edit run's state as the caller keeps it, `updated_at` included.
    pub fn save_run(&mut self, run: PipelineRun, cx: &mut Context<Self>) {
        self.save(Write::Run(run), cx);
    }

    /// Saves an entry of a run's log, numbered by the caller, which keeps the log.
    pub fn save_run_event(&mut self, video_id: VideoId, event: RunEvent, cx: &mut Context<Self>) {
        self.save(Write::RunEvent { video_id, event }, cx);
    }

    /// Saves a new upload job. `false` when the database refused it, which is reported.
    pub fn insert_upload(&mut self, job: UploadJob, cx: &mut Context<Self>) -> bool {
        self.save(Write::InsertUpload(job), cx)
    }

    /// Saves an upload job's state as the caller keeps it, `updated_at` included.
    pub fn save_upload(&mut self, job: UploadJob, cx: &mut Context<Self>) {
        self.save(Write::Upload(job), cx);
    }

    /// Saves a value of the settings table under `key`, other than the app
    /// settings. `false` when it can't be saved, which is reported.
    pub fn save_setting(&mut self, key: &str, value: &impl Serialize, cx: &mut Context<Self>) -> bool {
        match serde_json::to_value(value) {
            Ok(value) => self.save(Write::Setting { key: key.to_owned(), value }, cx),
            Err(error) => {
                log_line!("videocrm: can't save the setting {key}: {error}");
                false
            }
        }
    }

    /// Every AI edit run, newest first.
    pub fn read_runs(&self) -> Result<Vec<PipelineRun>, StoreError> {
        self.read_records(|store| store.list_runs(None), Queue::overlay_runs)
    }

    /// The log of an AI edit run, in order.
    pub fn read_run_events(&self, run_id: RunId) -> Result<Vec<RunEvent>, StoreError> {
        self.read_records(
            |store| store.list_run_events(run_id),
            |queue, events| queue.overlay_run_events(run_id, events),
        )
    }

    /// Every upload job, newest first.
    pub fn read_uploads(&self) -> Result<Vec<UploadJob>, StoreError> {
        self.read_records(|store| store.list_uploads(None), Queue::overlay_uploads)
    }

    /// A value [`AppState::save_setting`] saved under `key`.
    pub fn read_setting<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>, StoreError> {
        let queued = self.writes.queued().read().setting(key).cloned();
        if let Some(value) = queued {
            match serde_json::from_value(value) {
                Ok(value) => return Ok(Some(value)),
                Err(error) => log_line!("videocrm: can't read the queued setting {key}: {error}"),
            }
        }
        self.lock().get_setting(key)
    }

    /// Reads with the queue held, then replays the queue on what was read: a
    /// write the background writer lands meanwhile is both read and queued
    /// (replaying it again changes nothing), never neither. The connection is
    /// taken before the queue, as the background writer does.
    fn read_records<T>(
        &self,
        read: impl FnOnce(&Store) -> Result<T, StoreError>,
        overlay: impl FnOnce(&Queue, &mut T),
    ) -> Result<T, StoreError> {
        let store = self.lock();
        let queued = self.writes.queued();
        let queue = queued.read();
        let mut records = read(&store)?;
        overlay(&queue, &mut records);
        Ok(records)
    }
}
