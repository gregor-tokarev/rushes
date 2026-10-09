//! Small helpers for locating and running external tools.

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Resolves `program` to an absolute executable path. A bare name is searched in
/// `path_var` (a `PATH`-style list); anything with a directory part is taken as a path.
pub(crate) fn find_executable(program: &Path, path_var: Option<&OsStr>) -> Option<PathBuf> {
    if program.components().count() > 1 || program.is_absolute() {
        let path = std::path::absolute(program).ok()?;
        return is_executable(&path).then_some(path);
    }
    std::env::split_paths(path_var?)
        .filter(|dir| dir.is_absolute())
        .flat_map(|dir| candidates(&dir.join(program)))
        .find(|path| is_executable(path))
}

#[cfg(windows)]
fn candidates(path: &Path) -> Vec<PathBuf> {
    vec![path.to_owned(), path.with_extension("exe")]
}

#[cfg(not(windows))]
fn candidates(path: &Path) -> Vec<PathBuf> {
    vec![path.to_owned()]
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata().is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Runs a short-lived command and returns its stdout when it exits successfully
/// within `timeout`. Any failure yields `None`.
pub(crate) fn run_captured<I, S>(program: &Path, args: I, env: &[(String, String)], timeout: Duration) -> Option<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut child = Command::new(program)
        .args(args)
        .envs(env.iter().map(|(key, value)| (key, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    // Read concurrently so a chatty child never blocks on a full pipe.
    let reader = thread::spawn(move || {
        let mut buf = Vec::new();
        stdout.read_to_end(&mut buf).map(|_| buf)
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let stdout = reader.join().ok()?.ok()?;
    status.success().then(|| String::from_utf8_lossy(&stdout).into_owned())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn finds_programs_on_path_and_by_path() {
        let sh = find_executable(Path::new("sh"), Some(OsStr::new("/nonexistent:/bin:/usr/bin"))).expect("sh on PATH");
        assert!(sh.is_absolute());
        assert_eq!(find_executable(&sh, None), Some(sh.clone()));
        assert_eq!(find_executable(Path::new("sh"), None), None);
        assert_eq!(find_executable(Path::new("definitely-not-a-program"), Some(OsStr::new("/bin"))), None);
    }

    #[test]
    fn run_captured_returns_stdout_and_honours_timeout() {
        let sh = find_executable(Path::new("sh"), Some(OsStr::new("/bin:/usr/bin"))).unwrap();
        let out = run_captured(&sh, ["-c", "echo hi"], &[], Duration::from_secs(5));
        assert_eq!(out.as_deref(), Some("hi\n"));
        assert_eq!(run_captured(&sh, ["-c", "exit 3"], &[], Duration::from_secs(5)), None);
        let started = Instant::now();
        assert_eq!(run_captured(&sh, ["-c", "sleep 5"], &[], Duration::from_millis(200)), None);
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}
