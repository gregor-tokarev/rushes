//! The footage folder an edit works on: its takes and the `edit/` folder outputs go to.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{EditError, Result};
use crate::probe::{MediaInfo, probe};

/// File extensions of the takes (lowercase): video, then audio. Rushes' footage list
/// and the pipeline's prompt use the same list.
pub const FOOTAGE_EXTENSIONS: [&str; 23] = [
    "mp4", "mov", "m4v", "mkv", "webm", "avi", "mts", "m2ts", "mxf", "mpg", "mpeg", "wmv", "3gp", "wav", "mp3", "m4a",
    "aac", "flac", "ogg", "opus", "aif", "aiff", "wma",
];

pub const EDIT_DIR: &str = "edit";

/// One source file. Takes are named `A`, `B`, ... in file-name order; phrase ids start
/// with the take's name (`A12`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Take {
    pub id: String,
    /// File name inside the footage folder.
    pub file: String,
    pub path: PathBuf,
    pub media: MediaInfo,
}

impl Take {
    pub fn stem(&self) -> &str {
        Path::new(&self.file).file_stem().and_then(OsStr::to_str).unwrap_or(&self.file)
    }
}

#[derive(Clone, Debug)]
pub struct Project {
    pub footage_dir: PathBuf,
    pub takes: Vec<Take>,
}

impl Project {
    /// Lists and probes the takes in `footage_dir` (hidden files and folders skipped).
    pub fn open(footage_dir: &Path) -> Result<Self> {
        let footage_dir = std::path::absolute(footage_dir)?;
        let mut files = Vec::new();
        for entry in fs::read_dir(&footage_dir)? {
            let entry = entry?;
            let Ok(name) = entry.file_name().into_string() else { continue };
            let is_media = Path::new(&name)
                .extension()
                .and_then(OsStr::to_str)
                .is_some_and(|ext| FOOTAGE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()));
            if is_media && !name.starts_with('.') && entry.path().metadata().is_ok_and(|meta| meta.is_file()) {
                files.push(name);
            }
        }
        files.sort();
        if files.is_empty() {
            return Err(EditError::NoFootage(footage_dir));
        }
        let takes = files
            .into_iter()
            .enumerate()
            .map(|(index, file)| {
                let path = footage_dir.join(&file);
                Ok(Take { id: take_id(index), media: probe(&path)?, path, file })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { footage_dir, takes })
    }

    pub fn edit_dir(&self) -> PathBuf {
        self.footage_dir.join(EDIT_DIR)
    }

    /// `edit/<name>`, creating `edit/` when needed.
    pub fn edit_path(&self, name: &str) -> Result<PathBuf> {
        let path = self.edit_dir().join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        Ok(path)
    }

    pub fn take(&self, id: &str) -> Result<&Take> {
        let id = id.trim();
        self.takes.iter().find(|take| take.id.eq_ignore_ascii_case(id) || take.file == id).ok_or_else(|| {
            let known: Vec<String> = self.takes.iter().map(|take| format!("{} ({})", take.id, take.file)).collect();
            EditError::invalid(format!("no take “{id}”; takes: {}", known.join(", ")))
        })
    }
}

/// `A`..`Z`, then `AA`, `AB`, ...
pub fn take_id(index: usize) -> String {
    let mut n = index;
    let mut id = Vec::new();
    loop {
        id.push(b'A' + (n % 26) as u8);
        if n < 26 {
            break;
        }
        n = n / 26 - 1;
    }
    id.reverse();
    String::from_utf8(id).expect("ascii")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_ids_count_like_spreadsheet_columns() {
        assert_eq!(take_id(0), "A");
        assert_eq!(take_id(25), "Z");
        assert_eq!(take_id(26), "AA");
        assert_eq!(take_id(27), "AB");
        assert_eq!(take_id(26 * 27), "AAA");
    }
}
