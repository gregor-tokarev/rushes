//! Google sign-in and YouTube Data API v3 client for VideoCRM.
//!
//! Synchronous and blocking: run calls on a background thread. Long operations take
//! a cancel flag and a progress callback. Works against real Google
//! ([`YoutubeConfig::google`]) or the local emulators ([`YoutubeConfig::emulator`]).

mod client;
mod config;
mod credentials;
mod error;
mod http;
mod oauth;
mod types;
mod upload;

pub use client::YoutubeClient;
pub use config::{
    DEFAULT_GOOGLE_EMULATOR_URL, DEFAULT_YOUTUBE_EMULATOR_URL, EMULATOR_CLIENT_ID, SCOPES, YoutubeConfig,
};
pub use credentials::{Account, CredentialStore, SavedLogin};
pub use error::{Error, Result, RetryCause};
pub use oauth::{Credentials, PendingAuthorization, begin_authorization, check_client, refresh, revoke};
pub use types::{Channel, GoogleUser, Privacy, ProcessingStatus, UploadStatus, VideoInfo, watch_url};
pub use upload::{DEFAULT_CHUNK_SIZE, UPLOAD_GRANULARITY, UploadOptions, UploadProgress, UploadedVideo, VideoMetadata};
