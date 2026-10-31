//! Small helpers for locating and running external tools.

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
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
    run_captured_checked(program, args, env, timeout).ok()
}

/// Like [`run_captured`], with a reason when a launcher fails (for example, a
/// JavaScript entrypoint whose Node runtime is missing from the desktop's PATH).
pub(crate) fn run_captured_checked<I, S>(
    program: &Path,
    args: I,
    env: &[(String, String)],
    timeout: Duration,
) -> Result<String, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new(program);
    command.args(args).envs(env.iter().map(|(key, value)| (key, value)));
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    // Drain both pipes concurrently, including when a managed launcher starts a
    // second process. Receiving, rather than joining, also bounds the pipe wait.
    let stdout = capture_stream(child.stdout.take().expect("piped stdout"), u64::MAX);
    let stderr = capture_stream(child.stderr.take().expect("piped stderr"), 4096);
    let deadline = Instant::now() + timeout;
    let timed_out = || format!("timed out after {} seconds", timeout.as_secs());
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
            result => {
                stop_capture(&mut child);
                return Err(result.err().map_or_else(timed_out, |error| error.to_string()));
            }
        }
    };
    let receive = |stream: mpsc::Receiver<std::io::Result<Vec<u8>>>| {
        stream
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| timed_out())?
            .map_err(|error| error.to_string())
    };
    let output = receive(stdout).and_then(|stdout| receive(stderr).map(|stderr| (stdout, stderr)));
    let (stdout, stderr) = output.inspect_err(|_| stop_capture(&mut child))?;
    if status.success() {
        Ok(String::from_utf8_lossy(&stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&stderr);
        let detail: String =
            stderr.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or_default().chars().take(500).collect();
        Err(if detail.is_empty() { status.to_string() } else { format!("{status}: {detail}") })
    }
}

fn capture_stream(mut stream: impl Read + Send + 'static, limit: u64) -> mpsc::Receiver<std::io::Result<Vec<u8>>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = Vec::new();
        let result = stream.by_ref().take(limit).read_to_end(&mut buf).and_then(|_| {
            // Retain only the diagnostic prefix, but drain everything so the
            // command can finish even when it writes more than a pipe holds.
            std::io::copy(&mut stream, &mut std::io::sink()).map(|_| buf)
        });
        _ = tx.send(result);
    });
    rx
}

fn stop_capture(child: &mut Child) {
    #[cfg(unix)]
    // SAFETY: this process group was created for the captured command.
    unsafe {
        libc::killpg(child.id() as libc::pid_t, libc::SIGKILL);
    }
    _ = child.kill();
    _ = child.wait();
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
