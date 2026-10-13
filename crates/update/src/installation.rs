use std::env;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::version::Version;

/// Folder next to the installation where an update is downloaded and unpacked:
/// on the same volume, so putting it in place is a rename.
const STAGING_DIR: &str = ".rushes-update";

/// The executable's name, which the desktop entry and window class share.
pub(crate) const EXECUTABLE: &str = "rushes";
/// The app bundle's name on macOS.
pub(crate) const BUNDLE: &str = "Rushes.app";
/// The app bundle's identifier, as `scripts/bundle-macos.sh` sets it.
#[cfg(target_os = "macos")]
const BUNDLE_IDENTIFIER: &str = "work.tokarev.rushes";
/// The executable and bundle names while Rushes was called VideoCRM. Releases
/// also carry files under VideoCRM's names, which copies from before the
/// rename update themselves with; they keep these names until their next
/// update puts Rushes's in place.
pub(crate) const LEGACY_EXECUTABLE: &str = "videocrm";
pub(crate) const LEGACY_BUNDLE: &str = "VideoCRM.app";

/// How this copy of Rushes was installed, which decides the release file
/// that updates it and how it's replaced. A copy built from source has none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Installation {
    /// `Rushes.app` on macOS, copied from the disk image.
    MacApp { bundle: PathBuf },
    /// The AppImage file on Linux.
    AppImage { file: PathBuf },
    /// The Linux tarball unpacked into a prefix such as `~/.local`:
    /// `bin/rushes` and its desktop entry and icons under `share/` (or
    /// VideoCRM's names, for a copy from before the rename).
    Prefix { prefix: PathBuf },
}

impl Installation {
    /// The installation of the running executable, if it came from a release.
    pub fn detect() -> Option<Self> {
        let exe = env::current_exe().ok()?;
        let exe = exe.canonicalize().unwrap_or(exe);
        Self::detect_from(&exe, env::var_os("APPIMAGE"), env::var_os("APPDIR"))
    }

    /// [`Installation::detect`] for `exe`, with the AppImage runtime's
    /// `APPIMAGE` (the file) and `APPDIR` (where it's mounted) variables.
    pub fn detect_from(exe: &Path, appimage: Option<OsString>, appdir: Option<OsString>) -> Option<Self> {
        if cfg!(target_os = "macos") {
            // <bundle>.app/Contents/MacOS/<executable>
            let macos = exe.parent()?;
            let contents = macos.parent()?;
            let bundle = contents.parent()?;
            let is_bundle = macos.file_name()? == "MacOS"
                && contents.file_name()? == "Contents"
                && bundle.extension().is_some_and(|extension| extension == "app");
            return is_bundle.then(|| Installation::MacApp { bundle: bundle.to_path_buf() });
        }
        if cfg!(target_os = "linux") {
            // The runtime's variables reach programs this one starts, too: only
            // trust them for the executable inside the mounted AppImage.
            if let (Some(file), Some(appdir)) = (appimage, appdir)
                && !file.is_empty()
                && exe.starts_with(&appdir)
            {
                return Some(Installation::AppImage { file: PathBuf::from(file) });
            }
            let bin = exe.parent()?;
            let prefix = bin.parent()?;
            let from_tarball = bin.file_name()? == "bin"
                && [EXECUTABLE, LEGACY_EXECUTABLE].into_iter().any(|name| {
                    exe.file_name() == Some(name.as_ref())
                        && prefix.join(format!("share/applications/{name}.desktop")).is_file()
                });
            return from_tarball.then(|| Installation::Prefix { prefix: prefix.to_path_buf() });
        }
        None
    }

    /// What an update replaces: the app bundle, the AppImage file, or the prefix.
    pub fn path(&self) -> &Path {
        match self {
            Installation::MacApp { bundle } => bundle,
            Installation::AppImage { file } => file,
            Installation::Prefix { prefix } => prefix,
        }
    }

    /// The name of the release file that updates this installation to `version`,
    /// as `scripts/bundle-*.sh` names them.
    pub fn asset_name(&self, version: &Version) -> String {
        let arch = env::consts::ARCH;
        match self {
            Installation::MacApp { .. } => format!("Rushes-{version}-macos-universal.dmg"),
            Installation::AppImage { .. } => format!("Rushes-{version}-{arch}.AppImage"),
            Installation::Prefix { .. } => format!("rushes-{version}-linux-{arch}.tar.gz"),
        }
    }

    /// The folder the update is prepared in, beside what it replaces.
    pub(crate) fn staging_dir(&self) -> PathBuf {
        let parent = match self {
            Installation::MacApp { bundle } => bundle.parent(),
            Installation::AppImage { file } => file.parent(),
            Installation::Prefix { prefix } => Some(prefix.as_path()),
        };
        parent.unwrap_or(Path::new("/")).join(STAGING_DIR)
    }

    /// Whether this user can replace the installation: false for a folder
    /// owned by another user (`/usr/local`, `/Applications` on a managed Mac),
    /// a read-only disk image, or a copy macOS runs from a quarantine location.
    pub fn check_writable(&self) -> io::Result<()> {
        let staging = self.staging_dir();
        fs::create_dir_all(&staging)?;
        let probe = tempfile_in(&staging);
        let written = fs::write(&probe, b"");
        _ = fs::remove_file(&probe);
        _ = fs::remove_dir(&staging);
        written?;
        if let Installation::Prefix { prefix } = self {
            // The executable is replaced in bin/, which may belong to someone else.
            let bin = prefix.join("bin");
            let probe = tempfile_in(&bin);
            fs::write(&probe, b"")?;
            _ = fs::remove_file(&probe);
        }
        Ok(())
    }

    /// Gives `VideoCRM.app` Rushes's name: macOS calls an app by its bundle's
    /// name, and an update from a copy from before the rename installs Rushes
    /// as `VideoCRM.app`. Returns the renamed installation; `None` for any
    /// other installation, or when a `Rushes.app` is there already.
    pub fn rename_legacy_bundle(&self) -> io::Result<Option<Installation>> {
        let Installation::MacApp { bundle } = self else {
            return Ok(None);
        };
        let renamed = bundle.with_file_name(BUNDLE);
        if bundle.file_name() != Some(LEGACY_BUNDLE.as_ref()) || renamed.exists() {
            return Ok(None);
        }
        fs::rename(bundle, &renamed)?;
        Ok(Some(Installation::MacApp { bundle: renamed }))
    }

    /// Points the Dock's icon kept for `VideoCRM.app` at this `Rushes.app`,
    /// once the bundle took Rushes's name; the Dock restarts to show it.
    /// Returns whether there was one.
    pub fn adopt_legacy_dock_icon(&self) -> io::Result<bool> {
        #[cfg(target_os = "macos")]
        if let Installation::MacApp { bundle } = self {
            let legacy = bundle.with_file_name(LEGACY_BUNDLE);
            if bundle.file_name() == Some(BUNDLE.as_ref()) && !legacy.exists() {
                return crate::dock::replace(&legacy, bundle, BUNDLE_IDENTIFIER);
            }
        }
        Ok(false)
    }

    /// Starts this installation once the running process exits, from a
    /// helper that outlives it.
    pub fn start_after_exit(&self) -> io::Result<()> {
        crate::relaunch::run_after_exit(std::process::id(), &self.launch_command())
    }

    /// The command that starts this installation.
    pub(crate) fn launch_command(&self) -> Command {
        match self {
            Installation::MacApp { bundle } => {
                // Through Launch Services, as a click in the Finder does.
                let mut command = Command::new("/usr/bin/open");
                command.arg(updated_bundle(bundle));
                command
            }
            Installation::AppImage { file } => Command::new(file),
            Installation::Prefix { prefix } => Command::new(prefix.join("bin").join(EXECUTABLE)),
        }
    }
}

/// The bundle that holds the app after an update of `bundle`: the update
/// renames `VideoCRM.app` to `Rushes.app` beside it, unless that name is taken.
pub(crate) fn updated_bundle(bundle: &Path) -> PathBuf {
    let renamed = bundle.with_file_name(BUNDLE);
    if bundle.file_name() == Some(LEGACY_BUNDLE.as_ref()) && !bundle.exists() && renamed.exists() {
        renamed
    } else {
        bundle.to_path_buf()
    }
}

/// A file name in `dir` no other process uses.
fn tempfile_in(dir: &Path) -> PathBuf {
    dir.join(format!(".rushes-probe-{}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn finds_the_appimage_only_for_its_own_executable() {
        let exe = Path::new("/tmp/.mount_Rushes/usr/bin/rushes");
        let detected = Installation::detect_from(
            exe,
            Some("/home/me/Apps/Rushes.AppImage".into()),
            Some("/tmp/.mount_Rushes".into()),
        );
        assert_eq!(detected, Some(Installation::AppImage { file: "/home/me/Apps/Rushes.AppImage".into() }));

        // Started by another AppImage, which left its variables behind.
        let other = Installation::detect_from(
            Path::new("/opt/rushes-build/rushes"),
            Some("/home/me/Apps/Other.AppImage".into()),
            Some("/tmp/.mount_Other".into()),
        );
        assert_eq!(other, None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn finds_the_tarball_by_its_desktop_entry() {
        let prefix = tempfile::tempdir().expect("temp dir");
        let exe = prefix.path().join("bin/rushes");
        assert_eq!(Installation::detect_from(&exe, None, None), None, "a bare binary, as cargo builds it");

        fs::create_dir_all(prefix.path().join("share/applications")).expect("share dir");
        fs::write(prefix.path().join("share/applications/rushes.desktop"), "[Desktop Entry]\n").expect("entry");
        assert_eq!(
            Installation::detect_from(&exe, None, None),
            Some(Installation::Prefix { prefix: prefix.path().to_path_buf() })
        );
        assert_eq!(Installation::detect_from(&prefix.path().join("bin/other"), None, None), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn finds_a_tarball_from_before_the_rename() {
        let prefix = tempfile::tempdir().expect("temp dir");
        fs::create_dir_all(prefix.path().join("share/applications")).expect("share dir");
        fs::write(prefix.path().join("share/applications/videocrm.desktop"), "[Desktop Entry]\n").expect("entry");
        let installation = Installation::detect_from(&prefix.path().join("bin/videocrm"), None, None);
        assert_eq!(installation, Some(Installation::Prefix { prefix: prefix.path().to_path_buf() }));
        // VideoCRM's executable needs VideoCRM's entry.
        assert_eq!(Installation::detect_from(&prefix.path().join("bin/rushes"), None, None), None);

        let launch = installation.expect("installation").launch_command();
        assert_eq!(launch.get_program(), prefix.path().join("bin/rushes"), "its update installs Rushes's names");
    }

    #[test]
    fn opens_videocrm_app_as_rushes_app_once_renamed() {
        let dir = tempfile::tempdir().expect("temp dir");
        let legacy = dir.path().join("VideoCRM.app");
        let renamed = dir.path().join("Rushes.app");
        fs::create_dir_all(&legacy).expect("legacy bundle");
        fs::create_dir_all(&renamed).expect("a copy downloaded by hand");
        assert_eq!(updated_bundle(&legacy), legacy, "the update kept the name: Rushes.app was taken");

        fs::remove_dir(&legacy).expect("renamed by the update");
        assert_eq!(updated_bundle(&legacy), renamed);
        assert_eq!(updated_bundle(&renamed), renamed);
        let elsewhere = dir.path().join("Rushes Beta.app");
        assert_eq!(updated_bundle(&elsewhere), elsewhere);
    }

    #[test]
    fn renames_videocrm_app_to_rushes_app() {
        let dir = tempfile::tempdir().expect("temp dir");
        let legacy = dir.path().join("VideoCRM.app");
        let renamed = dir.path().join("Rushes.app");
        fs::create_dir_all(legacy.join("Contents")).expect("legacy bundle");
        let installation = Installation::MacApp { bundle: legacy.clone() };

        fs::create_dir_all(&renamed).expect("a copy downloaded by hand");
        assert_eq!(installation.rename_legacy_bundle().expect("rename"), None, "Rushes.app is taken");
        assert!(legacy.is_dir());

        fs::remove_dir(&renamed).expect("the other copy goes");
        assert_eq!(
            installation.rename_legacy_bundle().expect("rename"),
            Some(Installation::MacApp { bundle: renamed.clone() })
        );
        assert!(!legacy.exists());
        assert!(renamed.join("Contents").is_dir());

        let current = Installation::MacApp { bundle: renamed };
        assert_eq!(current.rename_legacy_bundle().expect("rename"), None);
        let tarball = Installation::Prefix { prefix: dir.path().to_path_buf() };
        assert_eq!(tarball.rename_legacy_bundle().expect("rename"), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn finds_the_app_bundle() {
        let exe = Path::new("/Applications/Rushes.app/Contents/MacOS/rushes");
        assert_eq!(
            Installation::detect_from(exe, None, None),
            Some(Installation::MacApp { bundle: "/Applications/Rushes.app".into() })
        );
        assert_eq!(Installation::detect_from(Path::new("/Users/me/rushes/target/release/rushes"), None, None), None);
    }

    #[test]
    fn names_the_release_file_like_the_bundle_scripts() {
        let version = Version::new(0, 2, 0);
        let arch = env::consts::ARCH;
        assert_eq!(
            Installation::MacApp { bundle: "/Applications/Rushes.app".into() }.asset_name(&version),
            "Rushes-0.2.0-macos-universal.dmg"
        );
        assert_eq!(
            Installation::AppImage { file: "/a/Rushes.AppImage".into() }.asset_name(&version),
            format!("Rushes-0.2.0-{arch}.AppImage")
        );
        assert_eq!(
            Installation::Prefix { prefix: "/home/me/.local".into() }.asset_name(&version),
            format!("rushes-0.2.0-linux-{arch}.tar.gz")
        );
    }

    #[test]
    fn stages_beside_what_it_replaces() {
        assert_eq!(
            Installation::MacApp { bundle: "/Applications/Rushes.app".into() }.staging_dir(),
            Path::new("/Applications/.rushes-update")
        );
        assert_eq!(
            Installation::AppImage { file: "/home/me/Apps/Rushes.AppImage".into() }.staging_dir(),
            Path::new("/home/me/Apps/.rushes-update")
        );
        assert_eq!(
            Installation::Prefix { prefix: "/home/me/.local".into() }.staging_dir(),
            Path::new("/home/me/.local/.rushes-update")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_of_someone_else_cant_be_updated() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("Rushes.AppImage");
        fs::write(&file, b"old").expect("appimage");
        let installation = Installation::AppImage { file };
        installation.check_writable().expect("own folder");
        assert!(!installation.staging_dir().exists(), "the probe leaves nothing behind");

        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).expect("read-only");
        let result = installation.check_writable();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).expect("writable again");
        // Root writes anywhere; then there's nothing to check.
        if let Err(error) = result {
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        }
    }
}
