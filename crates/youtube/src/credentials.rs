use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};

use crate::config::YoutubeConfig;
use crate::error::{Error, Result};
use crate::oauth::Credentials;
use crate::types::{Channel, GoogleUser};

/// The signed-in account as shown in the app.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub email: String,
    pub name: Option<String>,
    pub picture: Option<String>,
    /// `None` when the Google account has no YouTube channel.
    pub channel_id: Option<String>,
    pub channel_title: Option<String>,
    /// Handle such as `@creator`.
    pub channel_handle: Option<String>,
    pub channel_thumbnail: Option<String>,
    /// `None` when the channel hides its subscriber count.
    pub subscriber_count: Option<u64>,
    pub video_count: Option<u64>,
    /// [`YoutubeConfig::backend_label`] of the backend the tokens belong to.
    pub backend: String,
}

impl Account {
    pub fn new(user: &GoogleUser, channel: Option<&Channel>, config: &YoutubeConfig) -> Self {
        Self {
            email: user.email.clone(),
            name: user.name.clone(),
            picture: user.picture.clone(),
            channel_id: channel.map(|channel| channel.id.clone()),
            channel_title: channel.map(|channel| channel.title.clone()),
            channel_handle: channel.and_then(|channel| channel.custom_url.clone()),
            channel_thumbnail: channel.and_then(|channel| channel.thumbnail_url.clone()),
            subscriber_count: channel.and_then(|channel| channel.subscriber_count),
            video_count: channel.map(|channel| channel.video_count),
            backend: config.backend_label(),
        }
    }

    /// Tells accounts apart: the channel id, or the email while the account has no channel.
    pub fn key(&self) -> &str {
        self.channel_id.as_deref().unwrap_or(&self.email)
    }
}

/// Contents of the credentials file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedLogin {
    pub credentials: Credentials,
    pub account: Option<Account>,
}

/// JSON file holding the OAuth tokens and account, readable only by the user
/// (mode 0600 on unix). Writes go through a temp file and a rename. Clones share a
/// lock, so share one store (e.g. with the refresh callback) to keep saves ordered.
#[derive(Clone, Debug)]
pub struct CredentialStore {
    path: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl CredentialStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), lock: Arc::default() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `None` when nobody is signed in.
    pub fn load(&self) -> Result<Option<SavedLogin>> {
        match fs::read(&self.path) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::io(format!("reading {}", self.path.display()), e)),
        }
    }

    pub fn save(&self, login: &SavedLogin) -> Result<()> {
        let _guard = self.locked();
        self.write(login)
    }

    /// Replaces the tokens and keeps the stored account, e.g. after a token refresh.
    pub fn save_credentials(&self, credentials: &Credentials) -> Result<()> {
        let _guard = self.locked();
        let account = self.load()?.and_then(|login| login.account);
        self.write(&SavedLogin { credentials: credentials.clone(), account })
    }

    /// Signs out locally. Missing file is fine.
    pub fn clear(&self) -> Result<()> {
        let _guard = self.locked();
        match fs::remove_file(&self.path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                Err(Error::io(format!("removing {}", self.path.display()), e))
            }
            _ => Ok(()),
        }
    }

    fn locked(&self) -> MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self, login: &SavedLogin) -> Result<()> {
        let json = serde_json::to_vec_pretty(login)?;
        write_private(&self.path, &json).map_err(|e| Error::io(format!("writing {}", self.path.display()), e))
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        fs::create_dir_all(dir)?;
    }
    // A fresh name per write: never reuse a leftover file or share one with another process.
    let mut suffix = [0u8; 6];
    getrandom::fill(&mut suffix)?;
    let mut temp_name = path.file_name().unwrap_or_default().to_os_string();
    temp_name.push(format!(".{}.tmp", URL_SAFE_NO_PAD.encode(suffix)));
    let temp = path.with_file_name(temp_name);

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let written = options.open(&temp).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)
    });
    if written.is_err() {
        // Best effort: the original error is what matters.
        let _ = fs::remove_file(&temp);
    }
    written
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    fn credentials(access_token: &str) -> Credentials {
        Credentials {
            access_token: access_token.into(),
            refresh_token: Some("refresh".into()),
            expires_at: Utc::now(),
            scope: "openid email".into(),
            id_token: None,
        }
    }

    #[test]
    fn round_trips_and_keeps_account_on_token_update() {
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("nested/credentials.json"));
        assert_eq!(store.load().unwrap(), None);

        let account = Account {
            email: "creator@example.com".into(),
            name: Some("Demo Creator".into()),
            picture: None,
            channel_id: Some("UC123".into()),
            channel_title: Some("Demo Creator".into()),
            channel_handle: Some("@creator".into()),
            channel_thumbnail: None,
            subscriber_count: Some(42),
            video_count: Some(7),
            backend: "Google".into(),
        };
        let login = SavedLogin { credentials: credentials("first"), account: Some(account.clone()) };
        store.save(&login).unwrap();
        assert_eq!(store.load().unwrap(), Some(login));

        store.save_credentials(&credentials("second")).unwrap();
        let loaded = store.load().unwrap().unwrap();
        assert_eq!(loaded.credentials.access_token, "second");
        assert_eq!(loaded.account, Some(account));

        store.clear().unwrap();
        store.clear().unwrap();
        assert_eq!(store.load().unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn file_is_private_and_no_temp_files_remain() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        let store = CredentialStore::new(&path);
        store.save_credentials(&credentials("token")).unwrap();
        store.save_credentials(&credentials("again")).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn concurrent_saves_leave_a_valid_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("credentials.json"));
        let writers: Vec<_> = (0..4)
            .map(|i| {
                let store = store.clone();
                std::thread::spawn(move || {
                    for j in 0..2 {
                        store.save_credentials(&credentials(&format!("{i}-{j}"))).unwrap();
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }
        assert!(store.load().unwrap().is_some());
    }

    #[test]
    fn loads_accounts_saved_without_channel_details() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        let json = r#"{"credentials": {"access_token": "a", "refresh_token": null,
            "expires_at": "2026-01-01T00:00:00Z", "scope": "openid", "id_token": null},
            "account": {"email": "a@b.c", "name": null, "picture": null, "channel_id": "UC1",
            "channel_title": "Demo", "backend": "Google"}}"#;
        fs::write(&path, json).unwrap();
        let account = CredentialStore::new(path).load().unwrap().unwrap().account.unwrap();
        assert_eq!(account.channel_title.as_deref(), Some("Demo"));
        assert_eq!(account.channel_handle, None);
        assert_eq!(account.subscriber_count, None);
        assert_eq!(account.video_count, None);
    }

    #[test]
    fn corrupt_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        fs::write(&path, b"{not json").unwrap();
        assert!(matches!(CredentialStore::new(path).load(), Err(Error::Json(_))));
    }
}
