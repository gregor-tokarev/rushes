//! The `PATH` of an app started from Finder, the Dock or a desktop launcher.
//!
//! Such a start gets a bare `PATH` (`/usr/bin:/bin:/usr/sbin:/sbin` on macOS), without
//! Homebrew or the folders installers put user tools in, so ffmpeg, ffprobe and Claude
//! Code would be missing although a terminal finds them. The AI edit's requirement
//! checks and the agent (and the shell commands it runs) all inherit this process's
//! `PATH`, so the usual install folders that exist are appended to it at startup.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// Folders tools are commonly installed in, outside the system's own.
const SYSTEM_TOOL_DIRS: &[&str] = &[
    "/opt/homebrew/bin",
    "/opt/homebrew/sbin",
    "/usr/local/bin",
    "/opt/local/bin",
    "/home/linuxbrew/.linuxbrew/bin",
    "/snap/bin",
];

/// The same, inside the home folder: Claude Code's installer, uv/pipx, npm, cargo.
const HOME_TOOL_DIRS: &[&str] = &[".local/bin", ".claude/local", ".npm-global/bin", ".cargo/bin", "bin"];

/// What a process without any `PATH` would search.
const DEFAULT_PATH: &[&str] = &["/usr/bin", "/bin", "/usr/sbin", "/sbin"];

/// Appends the install folders that exist and are missing from `PATH`.
///
/// Call first thing in `main`, before any other thread exists: it sets an
/// environment variable.
pub fn extend_tool_path() {
    let home = std::env::var_os("HOME").filter(|home| !home.is_empty()).map(PathBuf::from);
    let current = std::env::var_os("PATH");
    if let Some(path) = extended_path(current.as_deref(), &tool_dirs(home.as_deref()), |dir| dir.is_dir()) {
        // SAFETY: `main` calls this before it starts any thread, so nothing reads the
        // environment concurrently.
        unsafe { std::env::set_var("PATH", path) };
    }
}

fn tool_dirs(home: Option<&Path>) -> Vec<PathBuf> {
    let system = SYSTEM_TOOL_DIRS.iter().map(PathBuf::from);
    let user = home.into_iter().flat_map(|home| HOME_TOOL_DIRS.iter().map(move |dir| home.join(dir)));
    system.chain(user).collect()
}

/// `current` followed by every candidate that `exists` and `current` lacks, in order;
/// `None` when there is nothing to add. The existing entries keep their order and
/// precedence.
fn extended_path(current: Option<&OsStr>, candidates: &[PathBuf], exists: impl Fn(&Path) -> bool) -> Option<OsString> {
    let mut dirs: Vec<PathBuf> = match current {
        Some(path) => std::env::split_paths(path).collect(),
        None => DEFAULT_PATH.iter().map(PathBuf::from).collect(),
    };
    let known = dirs.len();
    for dir in candidates {
        if !dirs.contains(dir) && exists(dir) {
            dirs.push(dir.clone());
        }
    }
    if dirs.len() == known && current.is_some() {
        return None;
    }
    std::env::join_paths(dirs).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs(list: &[&str]) -> Vec<PathBuf> {
        list.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn a_finder_start_finds_homebrew_and_the_users_tools() {
        let candidates = tool_dirs(Some(Path::new("/Users/me")));
        let installed = ["/opt/homebrew/bin", "/Users/me/.local/bin"];
        let path = extended_path(Some(OsStr::new("/usr/bin:/bin:/usr/sbin:/sbin")), &candidates, |dir| {
            installed.iter().any(|installed| dir == Path::new(installed))
        });
        assert_eq!(
            path.as_deref(),
            Some(OsStr::new("/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin:/Users/me/.local/bin"))
        );
    }

    #[test]
    fn a_terminal_path_keeps_its_order_and_gains_nothing_it_has() {
        let current = OsStr::new("/Users/me/.local/bin:/opt/homebrew/bin:/usr/bin:/bin");
        let candidates = dirs(&["/opt/homebrew/bin", "/Users/me/.local/bin"]);
        assert_eq!(extended_path(Some(current), &candidates, |_| true), None);
    }

    #[test]
    fn folders_that_dont_exist_are_not_added() {
        let candidates = dirs(&["/opt/homebrew/bin", "/usr/local/bin"]);
        assert_eq!(extended_path(Some(OsStr::new("/usr/bin")), &candidates, |_| false), None);
    }

    #[test]
    fn without_any_path_the_system_folders_come_first() {
        let candidates = dirs(&["/usr/local/bin"]);
        let path = extended_path(None, &candidates, |_| true);
        assert_eq!(path.as_deref(), Some(OsStr::new("/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin")));
    }
}
