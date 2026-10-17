//! Locating and running ffmpeg and ffprobe.

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

use crate::error::{EditError, Result};

/// How much of a failing program's error output is kept for the message.
const ERROR_TAIL_CHARS: usize = 1500;

pub(crate) fn ffmpeg() -> Result<PathBuf> {
    find("ffmpeg").ok_or(EditError::ToolMissing("ffmpeg"))
}

pub(crate) fn ffprobe() -> Result<PathBuf> {
    find("ffprobe").ok_or(EditError::ToolMissing("ffprobe"))
}

/// `name` on `PATH`.
pub fn find(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|path| is_executable(path))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata().is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file() || path.with_extension("exe").is_file()
}

/// Runs `program` to completion and returns its stdout. A non-zero exit becomes
/// [`EditError::ToolFailed`] with the end of its stderr.
pub(crate) fn run<I, S>(program: &Path, args: I) -> Result<Vec<u8>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let (status, stdout, stderr) = run_raw(program, args)?;
    if !status.success() {
        return Err(failure(program, &stderr));
    }
    Ok(stdout)
}

/// Like [`run`], but also returns stderr (ffmpeg prints its measurements there).
pub(crate) fn run_with_stderr<I, S>(program: &Path, args: I) -> Result<(Vec<u8>, String)>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let (status, stdout, stderr) = run_raw(program, args)?;
    if !status.success() {
        return Err(failure(program, &stderr));
    }
    Ok((stdout, stderr))
}

fn run_raw<I, S>(program: &Path, args: I) -> Result<(std::process::ExitStatus, Vec<u8>, String)>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut child =
        Command::new(program).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let mut stderr = child.stderr.take().expect("piped stderr");
    // Read stderr concurrently so a chatty program never blocks on a full pipe.
    let errors = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });
    let mut stdout = Vec::new();
    child.stdout.take().expect("piped stdout").read_to_end(&mut stdout)?;
    let status = child.wait()?;
    let stderr = String::from_utf8_lossy(&errors.join().unwrap_or_default()).into_owned();
    Ok((status, stdout, stderr))
}

fn failure(program: &Path, stderr: &str) -> EditError {
    let name =
        program.file_name().map_or_else(|| program.display().to_string(), |name| name.to_string_lossy().into_owned());
    let trimmed = stderr.trim();
    let start = trimmed.char_indices().rev().nth(ERROR_TAIL_CHARS).map_or(0, |(index, _)| index);
    EditError::ToolFailed { program: name, detail: trimmed[start..].to_owned() }
}

/// Seconds as ffmpeg reads them, with microsecond precision.
pub(crate) fn secs(value: f64) -> String {
    format!("{:.6}", value.max(0.0))
}
