use std::io;
use std::process::{Command, Stdio};

/// Waits for the process given as `$1` to exit (or become a zombie, which
/// has released its files), at most 60 s, then runs the rest of the arguments.
const WAIT_THEN_RUN: &str = r#"pid=$1
shift
tries=0
while kill -0 "$pid" 2>/dev/null; do
  case $(ps -o stat= -p "$pid" 2>/dev/null) in Z*) break ;; esac
  tries=$((tries + 1))
  [ "$tries" -ge 600 ] && break
  sleep 0.1
done
exec "$@""#;

/// Starts `command` once the process `pid` has exited, from a helper that
/// outlives it. VideoCRM holds its data folder until it exits, so a copy
/// started earlier would only say that VideoCRM is already open.
pub(crate) fn run_after_exit(pid: u32, command: &Command) -> io::Result<()> {
    let mut helper = Command::new("/bin/sh");
    helper.arg("-c").arg(WAIT_THEN_RUN).arg("videocrm-relaunch").arg(pid.to_string());
    helper.arg(command.get_program()).args(command.get_args());
    helper.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(unix)]
    {
        // Out of the terminal's process group: closing it or Ctrl+C there
        // ends VideoCRM, not the helper that starts it again.
        use std::os::unix::process::CommandExt as _;
        helper.process_group(0);
    }
    helper.spawn().map(drop)
}

#[cfg(all(test, unix))]
mod tests {
    use std::path::Path;
    use std::time::{Duration, Instant};

    use super::*;

    fn wait_for(path: &Path, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if path.exists() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        path.exists()
    }

    #[test]
    fn starts_the_command_only_after_the_process_exits() {
        let dir = tempfile::tempdir().expect("temp dir");
        let marker = dir.path().join("started");
        let mut old = Command::new("sleep").arg("1").spawn().expect("a process to wait for");

        let mut command = Command::new("touch");
        command.arg(&marker);
        run_after_exit(old.id(), &command).expect("helper starts");

        assert!(!wait_for(&marker, Duration::from_millis(500)), "started while the old process runs");
        // Exited but not reaped yet: a zombie, which the helper doesn't wait for.
        std::thread::sleep(Duration::from_millis(700));
        assert!(wait_for(&marker, Duration::from_secs(5)), "started after the old process exited");
        old.wait().expect("reap");
    }
}
