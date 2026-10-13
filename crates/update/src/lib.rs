//! Keeps an installed copy of Rushes up to date with its releases on GitHub.
//!
//! [`Updater::check`] asks GitHub for the newest release and says how this
//! copy gets it: it replaces itself ([`Delivery::Install`]) when it was
//! installed from a release into a folder its user can change, otherwise the
//! creator downloads it ([`Delivery::Manual`]). [`Updater::install`] downloads
//! the release file, checks it against the SHA-256 GitHub lists, and puts it
//! in place; the running copy keeps working, and the next start is the new
//! version. [`Updater::relaunch_after_exit`] starts it once this one quits.
//!
//! Synchronous and blocking: run calls on a background thread. Long
//! operations take a cancel flag and a progress callback.

mod download;
mod error;
mod install;
mod installation;
mod relaunch;
mod release;
mod version;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use ureq::Agent;

pub use error::{Error, Result};
pub use installation::Installation;
pub use release::{Asset, Release};
pub use version::Version;

/// GitHub's answer for the newest Rushes release (drafts and pre-releases excluded).
pub const RELEASES_URL: &str = "https://api.github.com/repos/gregor-tokarev/rushes/releases/latest";
/// Environment variable that replaces [`RELEASES_URL`] (tests, a local mirror).
pub const RELEASES_URL_ENV: &str = "RUSHES_UPDATE_URL";

/// A release newer than the running copy, and how this copy gets it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Update {
    pub release: Release,
    pub delivery: Delivery,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// This copy replaces itself with the release file.
    Install(Asset),
    /// The creator downloads it from the release page.
    Manual(ManualReason),
}

/// Why a copy can't update itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManualReason {
    /// It wasn't installed from a release: built from source, say.
    NotInstalled,
    /// Its user can't change the folder it's in (the path).
    ReadOnly(PathBuf),
    /// The release has no file for this kind of installation or processor.
    NoFile,
}

/// Where [`Updater::install`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Progress {
    Downloading {
        received: u64,
        total: u64,
    },
    /// Unpacking and putting the new version in place.
    Installing,
}

/// Checks for and installs releases newer than the running copy.
pub struct Updater {
    releases_url: String,
    current: Version,
    installation: Option<Installation>,
    agent: Agent,
}

impl Updater {
    /// An updater for `installation` (none: it can't update itself) running
    /// `current`, asking `releases_url` for the newest release.
    pub fn new(releases_url: impl Into<String>, current: Version, installation: Option<Installation>) -> Self {
        let agent = Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(20)))
            .timeout_recv_response(Some(Duration::from_secs(60)))
            // The whole download; slow connections need a while for the disk image.
            .timeout_recv_body(Some(Duration::from_secs(30 * 60)))
            .user_agent(concat!("rushes/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Self { releases_url: releases_url.into(), current, installation, agent }
    }

    /// The updater of the running copy, which is `current`: its installation as
    /// [`Installation::detect`] finds it, and GitHub's releases unless
    /// [`RELEASES_URL_ENV`] names others.
    pub fn for_running_copy(current: Version) -> Self {
        let releases_url = std::env::var(RELEASES_URL_ENV).ok().filter(|url| !url.is_empty());
        Self::new(releases_url.as_deref().unwrap_or(RELEASES_URL), current, Installation::detect())
    }

    /// The version running now.
    pub fn current(&self) -> &Version {
        &self.current
    }

    pub fn installation(&self) -> Option<&Installation> {
        self.installation.as_ref()
    }

    /// The newest release.
    pub fn latest_release(&self) -> Result<Release> {
        let mut response = self
            .agent
            .get(&self.releases_url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .call()?;
        let status = response.status().as_u16();
        let body = response.body_mut().read_to_string()?;
        if !(200..300).contains(&status) {
            let message = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|json| json.get("message")?.as_str().map(str::to_owned))
                .unwrap_or_default();
            return Err(Error::Status { status, message });
        }
        Release::from_github_json(&body)
    }

    /// The newest release if it's newer than `newer_than` (the running
    /// version, or one installed since), and how this copy gets it.
    pub fn check(&self, newer_than: &Version) -> Result<Option<Update>> {
        let release = self.latest_release()?;
        if release.version <= *newer_than {
            return Ok(None);
        }
        let delivery = self.delivery(&release);
        Ok(Some(Update { release, delivery }))
    }

    fn delivery(&self, release: &Release) -> Delivery {
        let Some(installation) = &self.installation else {
            return Delivery::Manual(ManualReason::NotInstalled);
        };
        let Some(asset) = release.asset(&installation.asset_name(&release.version)) else {
            return Delivery::Manual(ManualReason::NoFile);
        };
        match installation.check_writable() {
            Ok(()) => Delivery::Install(asset.clone()),
            Err(_) => {
                let folder = match installation {
                    Installation::Prefix { prefix } => prefix.as_path(),
                    other => other.path().parent().unwrap_or(other.path()),
                };
                Delivery::Manual(ManualReason::ReadOnly(folder.to_path_buf()))
            }
        }
    }

    /// Downloads `asset`, checks it, and puts it in place of the installation.
    /// The running copy keeps working; the next start is the new version.
    pub fn install(&self, asset: &Asset, progress: &mut dyn FnMut(Progress), cancel: &AtomicBool) -> Result<()> {
        let Some(installation) = &self.installation else {
            return Err(Error::Tool("this copy of Rushes wasn’t installed from a release".into()));
        };
        let staging = installation.staging_dir();
        // Leftovers of an update that didn't finish.
        remove_staging(&staging);
        fs::create_dir_all(&staging).map_err(|error| Error::io("cannot prepare the update", error))?;
        let file = staging.join(&asset.name);
        let result = download::download(
            &self.agent,
            asset,
            &file,
            &mut |received, total| progress(Progress::Downloading { received, total }),
            cancel,
        )
        .and_then(|()| {
            progress(Progress::Installing);
            install::install(installation, &file)
        });
        remove_staging(&staging);
        result
    }

    /// Starts the installation again once this process exits; call it right
    /// before quitting.
    pub fn relaunch_after_exit(&self) -> io::Result<()> {
        let Some(installation) = &self.installation else {
            return Err(io::Error::new(io::ErrorKind::Unsupported, "this copy wasn’t installed from a release"));
        };
        relaunch::run_after_exit(std::process::id(), &installation.launch_command())
    }
}

fn remove_staging(staging: &Path) {
    // The old macOS app bundle ends up here, and a disk image may still be
    // attached inside: what can't go now goes with the next update.
    _ = fs::remove_dir_all(staging);
}
