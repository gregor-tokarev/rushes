//! YouTube: account connection, publish form, uploads. Owned by the publish workstream.
//!
//! [`YoutubeService`] is the one owner of the connection and of upload jobs; the
//! views read it and call its methods. Blocking network work runs on worker
//! threads (sign-in, uploads) or the background executor (short API calls).
//!
//! The views draw Lucide icons beyond GPUI Kit's default bundle (`Film`, `Image`,
//! `ListOrdered`, `Upload`, `CloudUpload`, `CirclePlay`, ...), so the application
//! registers the full catalog (`gpui_kit::assets::AllAssets`).

mod badge;
mod chapters;
mod panel;
mod rules;
mod service;
mod settings;
mod transfer;
mod ui;
mod upload;
mod uploads_page;

pub use badge::AccountBadge;
pub use chapters::{CHAPTERS_HEADING, Chapter, chapters, chapters_text, insert_chapters};
pub use panel::PublishPanel;
pub use rules::{BlockingReason, CATEGORIES, Category, blocking_reasons, tags_length};
pub use service::{Connection, YoutubeEvent, YoutubeService, backend_config};
pub use settings::{EmulatorCheck, YoutubeSettings};
pub use transfer::Transfer;
pub use upload::{INTERRUPTED, INTERRUPTED_RESUMABLE, ProcessingOutcome, Retry, UploadError};
pub use uploads_page::UploadsPage;
