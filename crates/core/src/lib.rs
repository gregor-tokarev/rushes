//! Domain model, persistence and document logic for VideoCRM.

pub mod document;
pub mod media;
pub mod model;
mod sample;
pub mod settings;
pub mod store;
pub mod templates;

pub use model::*;
pub use store::{NewVideo, RunEvent, Store, StoreError, StoreResult};
