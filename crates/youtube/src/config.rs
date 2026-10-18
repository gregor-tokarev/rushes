use std::fmt;

use serde::{Deserialize, Serialize};

/// Lets Rushes change videos already on YouTube (`videos.update`), such as who can
/// watch them. Uploading and reading don't need it.
pub const MANAGE_SCOPE: &str = "https://www.googleapis.com/auth/youtube.force-ssl";

/// OAuth scopes requested at sign-in: identity for the account chip, upload and
/// read access for the channel, and changes to uploaded videos.
pub const SCOPES: [&str; 6] = [
    "openid",
    "email",
    "profile",
    "https://www.googleapis.com/auth/youtube.upload",
    "https://www.googleapis.com/auth/youtube.readonly",
    MANAGE_SCOPE,
];

pub const DEFAULT_GOOGLE_EMULATOR_URL: &str = "http://127.0.0.1:4002";
pub const DEFAULT_YOUTUBE_EMULATOR_URL: &str = "http://127.0.0.1:4020";

/// OAuth client id used against the emulators, which accept any client.
pub const EMULATOR_CLIENT_ID: &str = "rushes-desktop.apps.googleusercontent.com";

/// Endpoints and OAuth client for one YouTube backend: real Google or the local emulators.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct YoutubeConfig {
    pub auth_url: String,
    pub token_url: String,
    pub userinfo_url: String,
    pub revoke_url: String,
    /// Base of the Data API, e.g. `https://www.googleapis.com/youtube/v3`.
    pub api_base: String,
    /// Base of the media upload API, e.g. `https://www.googleapis.com/upload/youtube/v3`.
    pub upload_base: String,
    pub client_id: String,
    /// Desktop OAuth clients get a secret too; Google does not treat it as confidential.
    pub client_secret: Option<String>,
}

impl YoutubeConfig {
    /// Real Google endpoints with the user's own "Desktop app" OAuth client.
    pub fn google(client_id: impl Into<String>, client_secret: Option<String>) -> Self {
        Self {
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth".to_owned(),
            token_url: "https://oauth2.googleapis.com/token".to_owned(),
            userinfo_url: "https://openidconnect.googleapis.com/v1/userinfo".to_owned(),
            revoke_url: "https://oauth2.googleapis.com/revoke".to_owned(),
            api_base: "https://www.googleapis.com/youtube/v3".to_owned(),
            upload_base: "https://www.googleapis.com/upload/youtube/v3".to_owned(),
            client_id: client_id.into(),
            client_secret: client_secret.filter(|secret| !secret.is_empty()),
        }
    }

    /// The local emulators started by `scripts/emulators.sh`.
    pub fn emulator(google_url: &str, youtube_url: &str) -> Self {
        let google = google_url.trim_end_matches('/');
        let youtube = youtube_url.trim_end_matches('/');
        Self {
            auth_url: format!("{google}/o/oauth2/v2/auth"),
            token_url: format!("{google}/oauth2/token"),
            userinfo_url: format!("{google}/oauth2/v2/userinfo"),
            revoke_url: format!("{google}/oauth2/revoke"),
            api_base: format!("{youtube}/youtube/v3"),
            upload_base: format!("{youtube}/upload/youtube/v3"),
            client_id: EMULATOR_CLIENT_ID.to_owned(),
            client_secret: None,
        }
    }

    /// Emulator config when `RUSHES_GOOGLE_URL` or `RUSHES_YOUTUBE_URL` is set; the
    /// other one falls back to its default port.
    pub fn emulator_from_env() -> Option<Self> {
        let google = std::env::var("RUSHES_GOOGLE_URL").ok().filter(|v| !v.is_empty());
        let youtube = std::env::var("RUSHES_YOUTUBE_URL").ok().filter(|v| !v.is_empty());
        if google.is_none() && youtube.is_none() {
            return None;
        }
        Some(Self::emulator(
            google.as_deref().unwrap_or(DEFAULT_GOOGLE_EMULATOR_URL),
            youtube.as_deref().unwrap_or(DEFAULT_YOUTUBE_EMULATOR_URL),
        ))
    }

    pub fn is_google(&self) -> bool {
        self.auth_url.starts_with("https://accounts.google.com/")
    }

    /// Short label for the account UI, e.g. "Google" or "Emulator (127.0.0.1:4002)".
    pub fn backend_label(&self) -> String {
        if self.is_google() {
            return "Google".to_owned();
        }
        let host = url::Url::parse(&self.auth_url)
            .ok()
            .and_then(|url| Some(format!("{}:{}", url.host_str()?, url.port_or_known_default()?)));
        match host {
            Some(host) => format!("Emulator ({host})"),
            None => "Emulator".to_owned(),
        }
    }
}

impl fmt::Debug for YoutubeConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("YoutubeConfig")
            .field("auth_url", &self.auth_url)
            .field("token_url", &self.token_url)
            .field("userinfo_url", &self.userinfo_url)
            .field("revoke_url", &self.revoke_url)
            .field("api_base", &self.api_base)
            .field("upload_base", &self.upload_base)
            .field("client_id", &self.client_id)
            .field("client_secret", &self.client_secret.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emulator_paths_follow_the_google_emulator() {
        let config = YoutubeConfig::emulator("http://127.0.0.1:4002/", "http://127.0.0.1:4020");
        assert_eq!(config.auth_url, "http://127.0.0.1:4002/o/oauth2/v2/auth");
        assert_eq!(config.token_url, "http://127.0.0.1:4002/oauth2/token");
        assert_eq!(config.userinfo_url, "http://127.0.0.1:4002/oauth2/v2/userinfo");
        assert_eq!(config.revoke_url, "http://127.0.0.1:4002/oauth2/revoke");
        assert_eq!(config.api_base, "http://127.0.0.1:4020/youtube/v3");
        assert_eq!(config.upload_base, "http://127.0.0.1:4020/upload/youtube/v3");
        assert!(!config.is_google());
        assert_eq!(config.backend_label(), "Emulator (127.0.0.1:4002)");
    }

    #[test]
    fn google_config_drops_empty_secret_and_redacts_it() {
        assert_eq!(YoutubeConfig::google("id", Some(String::new())).client_secret, None);
        let config = YoutubeConfig::google("id.apps.googleusercontent.com", Some("GOCSPX-secret".into()));
        assert!(config.is_google());
        assert_eq!(config.backend_label(), "Google");
        assert!(!format!("{config:?}").contains("GOCSPX"));
    }
}
