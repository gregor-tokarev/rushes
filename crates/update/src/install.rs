//! Putting a downloaded release file in place of the installation. Every step
//! before the last works in the staging folder; the last is a rename, so an
//! update that fails leaves the installation as it was.

use std::fs::{self, File};
use std::io::Read as _;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::installation::Installation;

/// Replaces `installation` with the release file `download`, which sits in
/// the installation's staging folder.
pub(crate) fn install(installation: &Installation, download: &Path) -> Result<()> {
    match installation {
        Installation::AppImage { file } => install_appimage(download, file),
        Installation::Prefix { prefix } => install_tarball(download, prefix),
        Installation::MacApp { bundle } => install_disk_image(download, bundle),
    }
}

fn install_appimage(download: &Path, file: &Path) -> Result<()> {
    // An AppImage is an ELF executable with "AI" and type 2 in its identification bytes.
    let mut magic = Vec::with_capacity(11);
    File::open(download)
        .and_then(|opened| opened.take(11).read_to_end(&mut magic))
        .map_err(|error| Error::io("cannot read the downloaded AppImage", error))?;
    if magic.len() < 11 || &magic[..4] != b"\x7fELF" || &magic[8..11] != b"AI\x02" {
        return Err(Error::Corrupt("the downloaded file isn't an AppImage".into()));
    }
    set_executable(download)?;
    // The running AppImage keeps its open file; the next start opens the new one.
    fs::rename(download, file).map_err(|error| Error::io(format!("cannot replace {}", file.display()), error))
}

fn install_tarball(download: &Path, prefix: &Path) -> Result<()> {
    let staging = download.parent().expect("the download is in the staging folder");
    let unpacked = staging.join("unpacked");
    let archive = File::open(download).map_err(|error| Error::io("cannot open the downloaded archive", error))?;
    fs::create_dir_all(&unpacked).map_err(|error| Error::io("cannot unpack the update", error))?;
    tar::Archive::new(flate2::read::GzDecoder::new(archive))
        .unpack(&unpacked)
        .map_err(|error| Error::Corrupt(format!("cannot unpack the archive: {error}")))?;

    // One top-level folder, videocrm-<version>-linux-<arch>/, holding the prefix's tree.
    let mut entries = fs::read_dir(&unpacked)
        .map_err(|error| Error::io("cannot read the unpacked update", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| Error::io("cannot read the unpacked update", error))?;
    let tree = match entries.as_mut_slice() {
        [only] if only.path().is_dir() => only.path(),
        _ => return Err(Error::Corrupt("the archive doesn't hold one VideoCRM folder".into())),
    };
    let executable = Path::new("bin/videocrm");
    if !tree.join(executable).is_file() {
        return Err(Error::Corrupt("the archive has no bin/videocrm".into()));
    }
    let mut files = Vec::new();
    collect_files(&tree, Path::new(""), &mut files)?;

    // The executable last: if anything before it fails, the old VideoCRM still starts.
    files.sort_by_key(|file| file == executable);
    for file in files {
        let target = prefix.join(&file);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| Error::io(format!("cannot create {}", parent.display()), error))?;
        }
        fs::rename(tree.join(&file), &target)
            .map_err(|error| Error::io(format!("cannot replace {}", target.display()), error))?;
    }
    Ok(())
}

/// The files under `dir`, relative to the root it started from.
fn collect_files(dir: &Path, relative: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    let entries = fs::read_dir(dir).map_err(|error| Error::io("cannot read the unpacked update", error))?;
    for entry in entries {
        let entry = entry.map_err(|error| Error::io("cannot read the unpacked update", error))?;
        let kind = entry.file_type().map_err(|error| Error::io("cannot read the unpacked update", error))?;
        let path = relative.join(entry.file_name());
        if kind.is_dir() {
            collect_files(&entry.path(), &path, files)?;
        } else {
            files.push(path);
        }
    }
    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .map_err(|error| Error::io(format!("cannot make {} executable", path.display()), error))
}

#[cfg(not(unix))]
fn set_executable(_: &Path) -> Result<()> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn install_disk_image(download: &Path, bundle: &Path) -> Result<()> {
    use std::process::Command;

    let staging = download.parent().expect("the download is in the staging folder");
    let mount = staging.join("mount");
    let staged = staging.join("VideoCRM.app");
    fs::create_dir_all(&mount).map_err(|error| Error::io("cannot prepare the update", error))?;

    run(Command::new("/usr/bin/hdiutil")
        .args(["attach", "-nobrowse", "-readonly", "-noautoopen", "-quiet", "-mountpoint"])
        .arg(&mount)
        .arg(download))?;
    // ditto keeps the signature, extended attributes and symlinks.
    let copied = run(Command::new("/usr/bin/ditto").arg(mount.join("VideoCRM.app")).arg(&staged));
    // A disk image still attached goes away when the Mac restarts; the update is fine without detaching.
    _ = run(Command::new("/usr/bin/hdiutil").args(["detach", "-quiet"]).arg(&mount))
        .or_else(|_| run(Command::new("/usr/bin/hdiutil").args(["detach", "-quiet", "-force"]).arg(&mount)));
    copied?;
    run(Command::new("/usr/bin/codesign").args(["--verify", "--strict"]).arg(&staged))?;

    swap(&staged, bundle)
}

/// Exchanges the staged bundle with the installed one in one step, so the
/// app is never missing; the old bundle ends up in the staging folder.
#[cfg(target_os = "macos")]
fn swap(staged: &Path, bundle: &Path) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;

    let from = CString::new(staged.as_os_str().as_bytes()).expect("paths have no NUL");
    let to = CString::new(bundle.as_os_str().as_bytes()).expect("paths have no NUL");
    // SAFETY: both are valid NUL-terminated paths for the duration of the call.
    if unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_SWAP) } == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() != Some(libc::ENOTSUP) {
        return Err(Error::io(format!("cannot replace {}", bundle.display()), error));
    }
    // A volume without swapping: move the old bundle aside, then the new one in.
    let aside = staged.with_extension("old");
    fs::rename(bundle, &aside).map_err(|error| Error::io(format!("cannot replace {}", bundle.display()), error))?;
    fs::rename(staged, bundle).map_err(|error| {
        _ = fs::rename(&aside, bundle);
        Error::io(format!("cannot replace {}", bundle.display()), error)
    })
}

#[cfg(target_os = "macos")]
fn run(command: &mut std::process::Command) -> Result<()> {
    let program = Path::new(command.get_program()).file_name().unwrap_or_default().to_string_lossy().into_owned();
    let output = command.output().map_err(|error| Error::io(format!("cannot run {program}"), error))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(Error::Tool(format!("{program} failed ({}): {}", output.status, stderr.trim())))
}

#[cfg(not(target_os = "macos"))]
fn install_disk_image(_: &Path, _: &Path) -> Result<()> {
    Err(Error::Tool("disk images install only on macOS".into()))
}
