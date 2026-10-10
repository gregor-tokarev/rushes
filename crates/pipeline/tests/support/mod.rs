//! Helpers shared by the pipeline's test binaries.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::Command;

/// Writes an executable script at `path`, from a child process.
///
/// A file this process writes is open for writing until it is closed, and a
/// process another test thread forks meanwhile inherits that descriptor and
/// keeps it until it starts its own program. Running the script before then
/// fails with "Text file busy" (ETXTBSY), which a loaded machine turns from a
/// microsecond window into an occasional failure. Written by a child, the
/// script is never open in this process, so no fork of ours can hold it.
pub fn write_executable(path: &Path, script: &str) {
    let written = Command::new("/bin/sh")
        .args(["-c", r#"printf '%s' "$1" > "$2""#, "sh"])
        .arg(script)
        .arg(path)
        .status()
        .expect("run /bin/sh");
    assert!(written.success(), "couldn't write {}", path.display());
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("make it executable");
}
