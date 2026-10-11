use std::env;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::version::Version;

/// Folder next to the installation where an update is downloaded and unpacked:
/// on the same volume, so putting it in place is a rename.
const STAGING_DIR: &str = ".videocrm-update";

/// How this copy of VideoCRM was installed, which decides the release file
/// that updates it and how it's replaced. A copy built from source has none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Installation {
    /// `VideoCRM.app` on macOS, copied from the disk image.
    MacApp { bundle: PathBuf },
    /// The AppImage file on Linux.
    AppImage { file: PathBuf },
    /// The Linux tarball unpacked into a prefix such as `~/.local`:
    /// `bin/videocrm` and its desktop entry and icons under `share/`.
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
            let from_tarball = exe.file_name()? == "videocrm"
                && bin.file_name()? == "bin"
                && prefix.join("share/applications/videocrm.desktop").is_file();
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
            Installation::MacApp { .. } => format!("VideoCRM-{version}-macos-universal.dmg"),
            Installation::AppImage { .. } => format!("VideoCRM-{version}-{arch}.AppImage"),
            Installation::Prefix { .. } => format!("videocrm-{version}-linux-{arch}.tar.gz"),
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

    /// The command that starts this installation.
    pub(crate) fn launch_command(&self) -> Command {
        match self {
            Installation::MacApp { bundle } => {
                // Through Launch Services, as a click in the Finder does.
                let mut command = Command::new("/usr/bin/open");
                command.arg(bundle);
                command
            }
            Installation::AppImage { file } => Command::new(file),
            Installation::Prefix { prefix } => Command::new(prefix.join("bin/videocrm")),
        }
    }
}

/// A file name in `dir` no other process uses.
fn tempfile_in(dir: &Path) -> PathBuf {
    dir.join(format!(".videocrm-probe-{}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn finds_the_appimage_only_for_its_own_executable() {
        let exe = Path::new("/tmp/.mount_VideoCRM/usr/bin/videocrm");
        let detected = Installation::detect_from(
            exe,
            Some("/home/me/Apps/VideoCRM.AppImage".into()),
            Some("/tmp/.mount_VideoCRM".into()),
        );
        assert_eq!(detected, Some(Installation::AppImage { file: "/home/me/Apps/VideoCRM.AppImage".into() }));

        // Started by another AppImage, which left its variables behind.
        let other = Installation::detect_from(
            Path::new("/opt/videocrm-build/videocrm"),
            Some("/home/me/Apps/Other.AppImage".into()),
            Some("/tmp/.mount_Other".into()),
        );
        assert_eq!(other, None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn finds_the_tarball_by_its_desktop_entry() {
        let prefix = tempfile::tempdir().expect("temp dir");
        let exe = prefix.path().join("bin/videocrm");
        assert_eq!(Installation::detect_from(&exe, None, None), None, "a bare binary, as cargo builds it");

        fs::create_dir_all(prefix.path().join("share/applications")).expect("share dir");
        fs::write(prefix.path().join("share/applications/videocrm.desktop"), "[Desktop Entry]\n").expect("entry");
        assert_eq!(
            Installation::detect_from(&exe, None, None),
            Some(Installation::Prefix { prefix: prefix.path().to_path_buf() })
        );
        assert_eq!(Installation::detect_from(&prefix.path().join("bin/other"), None, None), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn finds_the_app_bundle() {
        let exe = Path::new("/Applications/VideoCRM.app/Contents/MacOS/videocrm");
        assert_eq!(
            Installation::detect_from(exe, None, None),
            Some(Installation::MacApp { bundle: "/Applications/VideoCRM.app".into() })
        );
        assert_eq!(
            Installation::detect_from(Path::new("/Users/me/videocrm/target/release/videocrm"), None, None),
            None
        );
    }

    #[test]
    fn names_the_release_file_like_the_bundle_scripts() {
        let version = Version::new(0, 2, 0);
        let arch = env::consts::ARCH;
        assert_eq!(
            Installation::MacApp { bundle: "/Applications/VideoCRM.app".into() }.asset_name(&version),
            "VideoCRM-0.2.0-macos-universal.dmg"
        );
        assert_eq!(
            Installation::AppImage { file: "/a/VideoCRM.AppImage".into() }.asset_name(&version),
            format!("VideoCRM-0.2.0-{arch}.AppImage")
        );
        assert_eq!(
            Installation::Prefix { prefix: "/home/me/.local".into() }.asset_name(&version),
            format!("videocrm-0.2.0-linux-{arch}.tar.gz")
        );
    }

    #[test]
    fn stages_beside_what_it_replaces() {
        assert_eq!(
            Installation::MacApp { bundle: "/Applications/VideoCRM.app".into() }.staging_dir(),
            Path::new("/Applications/.videocrm-update")
        );
        assert_eq!(
            Installation::AppImage { file: "/home/me/Apps/VideoCRM.AppImage".into() }.staging_dir(),
            Path::new("/home/me/Apps/.videocrm-update")
        );
        assert_eq!(
            Installation::Prefix { prefix: "/home/me/.local".into() }.staging_dir(),
            Path::new("/home/me/.local/.videocrm-update")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_of_someone_else_cant_be_updated() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("VideoCRM.AppImage");
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
