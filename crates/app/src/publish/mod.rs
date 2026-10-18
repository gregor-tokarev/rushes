//! YouTube: account connection, publish form, uploads. Owned by the publish workstream.
//!
//! [`YoutubeService`] is the one owner of the connection and of upload jobs; the
//! views read it and call its methods. [`PublishAssistant`] has the agent write a
//! video's details and Codex draw its thumbnails. Blocking network work runs on worker
//! threads (sign-in, uploads) or the background executor (short API calls).
//!
//! The views draw Lucide icons beyond GPUI Kit's default bundle (`Film`, `Image`,
//! `ListOrdered`, `Upload`, `CloudUpload`, `CirclePlay`, ...), so the application
//! registers the full catalog (`gpui_kit::assets::AllAssets`).

mod assist_dialogs;
mod assistant;
mod badge;
mod channel;
mod chapters;
mod panel;
mod rules;
mod service;
mod settings;
mod thumbnail;
mod transfer;
mod ui;
mod upload;
mod uploads_page;
mod visibility;

pub use assistant::{
    AssistError, AssistEvent, DEFAULT_THUMBNAILS, DetailSources, DetailsJob, PublishAssistant, ThumbnailJob,
    ThumbnailOptions,
};
pub use badge::AccountBadge;
pub use channel::{ChannelChoices, ChooseChannel};
pub use chapters::{CHAPTERS_HEADING, Chapter, chapters, chapters_text, insert_chapters};
pub use panel::PublishPanel;
pub use rules::{AccountAccess, BlockingReason, CATEGORIES, Category, blocking_reasons, tags_length};
pub use service::{Connection, EmulatorCheck, YoutubeAccount, YoutubeEvent, YoutubeService, backend_config};
pub use settings::YoutubeSettings;
pub use thumbnail::ThumbnailSync;
pub use transfer::Transfer;
pub use upload::{
    INTERRUPTED, INTERRUPTED_RESUMABLE, ProcessingOutcome, Retired, Retry, UPLOAD_FILE_LOCKED, UploadError,
};
pub use uploads_page::UploadsPage;
pub use visibility::VisibilitySync;

use gpui_kit::App;
use rushes_core::VideoId;

/// A sentence for delete confirmations when one of `video_ids` is uploading: deleting
/// a video cancels its upload. `None` when none is.
pub fn delete_upload_notice(video_ids: &[VideoId], cx: &App) -> Option<&'static str> {
    let service = YoutubeService::try_global(cx)?;
    let uploading = service.read(cx).is_uploading_any(video_ids);
    uploading.then_some(if video_ids.len() == 1 {
        "It’s uploading to YouTube right now: deleting it cancels the upload."
    } else {
        "Some of them are uploading to YouTube right now: deleting them cancels those uploads."
    })
}
