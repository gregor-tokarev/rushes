use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

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
            backend: config.backend_label(),
        }
    }
}

/// Contents of the credentials file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedLogin {
    pub credentials: Credentials,
    pub account: Option<Account>,
}

/// JSON file holding the OAuth tokens and account, readable only by the user
/// (mode 0600 on unix). Writes go through a temp file and a rename.
#[derive(Clone, Debug)]
pub struct CredentialStore {
    path: PathBuf,
}

impl CredentialStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
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
        let json = serde_json::to_vec_pretty(login)?;
        write_private(&self.path, &json).map_err(|e| Error::io(format!("writing {}", self.path.display()), e))
    }

    /// Replaces the tokens and keeps the stored account, e.g. after a token refresh.
    pub fn save_credentials(&self, credentials: &Credentials) -> Result<()> {
        let account = self.load()?.and_then(|login| login.account);
        self.save(&SavedLogin { credentials: credentials.clone(), account })
    }

    /// Signs out locally. Missing file is fine.
    pub fn clear(&self) -> Result<()> {
        match fs::remove_file(&self.path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                Err(Error::io(format!("removing {}", self.path.display()), e))
            }
            _ => Ok(()),
        }
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        fs::create_dir_all(dir)?;
    }
    let mut temp_name = path.file_name().unwrap_or_default().to_os_string();
    temp_name.push(".tmp");
    let temp = path.with_file_name(temp_name);

    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    // A leftover temp file keeps its old mode; tighten it before writing secrets.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temp, path)
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
    fn file_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        // A stale temp file with open permissions must not leak its mode.
        fs::write(dir.path().join("credentials.json.tmp"), b"").unwrap();
        fs::set_permissions(dir.path().join("credentials.json.tmp"), fs::Permissions::from_mode(0o644)).unwrap();
        let store = CredentialStore::new(&path);
        store.save_credentials(&credentials("token")).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn corrupt_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        fs::write(&path, b"{not json").unwrap();
        assert!(matches!(CredentialStore::new(path).load(), Err(Error::Json(_))));
    }
}
