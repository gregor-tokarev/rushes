//! Per-video media folders on disk.
//!
//! Layout under the media root:
//!
//! ```text
//! <slug>-<short id>/
//!   footage/          raw takes dropped in by the user
//!     edit/           everything video-use writes (final.mp4, preview.mp4, project.md, ...)
//! ```

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::model::Video;

const FOOTAGE_DIR: &str = "footage";
const THUMBNAILS_DIR: &str = "thumbnails";
const EDIT_DIR: &str = "edit";
const MAX_SLUG_LEN: usize = 48;

/// Video files the footage folder lists (lowercase).
pub const VIDEO_EXTENSIONS: &[&str] =
    &["mp4", "mov", "m4v", "mkv", "webm", "avi", "mts", "m2ts", "mxf", "mpg", "mpeg", "wmv", "3gp"];
/// Audio files the footage folder lists (lowercase).
pub const AUDIO_EXTENSIONS: &[&str] = &["wav", "mp3", "m4a", "aac", "flac", "ogg", "opus", "aif", "aiff", "wma"];
const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif", "heic", "heif", "tif", "tiff", "bmp"];

#[derive(Debug, Error)]
pub enum MediaError {
    #[error("could not create folder {path}: {source}")]
    CreateDir { path: PathBuf, source: io::Error },
    #[error("could not read folder {path}: {source}")]
    ReadDir { path: PathBuf, source: io::Error },
}

/// `<Videos>/Rushes`, falling back to `~/Videos/Rushes`, then to the data dir.
pub fn default_media_root() -> PathBuf {
    videos_dir().map(|videos| videos.join("Rushes")).unwrap_or_else(|| crate::settings::data_dir().join("media"))
}

/// `<Videos>/VideoCRM`, the default media root while the app was called
/// VideoCRM, before the rename to Rushes. `None` without a videos or home folder.
pub fn legacy_default_media_root() -> Option<PathBuf> {
    videos_dir().map(|videos| videos.join("VideoCRM"))
}

fn videos_dir() -> Option<PathBuf> {
    dirs::video_dir().or_else(|| dirs::home_dir().map(|home| home.join("Videos")))
}

/// Folder name for a video: ASCII slug of the title plus the short id, e.g.
/// `rust-in-10-minutes-0192f3a4`. The id keeps names unique across renames and
/// duplicate titles.
pub fn video_folder_name(video: &Video) -> String {
    let mut slug = String::new();
    for ch in video.title.chars() {
        if ch.is_ascii_alphanumeric() {
            if slug.len() >= MAX_SLUG_LEN {
                break;
            }
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-');
    let slug = if slug.is_empty() { "video" } else { slug };
    format!("{slug}-{}", video.id.short())
}

/// Makes sure the video has a media folder with a `footage/` subfolder and
/// returns the video folder. Assigns `<root>/<video_folder_name>` when the video
/// has no folder yet; the caller persists the updated `video.folder`.
pub fn ensure_video_folder(root: &Path, video: &mut Video) -> Result<PathBuf, MediaError> {
    let folder = video.folder.clone().unwrap_or_else(|| root.join(video_folder_name(video)));
    let footage = folder.join(FOOTAGE_DIR);
    fs::create_dir_all(&footage).map_err(|source| MediaError::CreateDir { path: footage, source })?;
    video.folder = Some(folder.clone());
    Ok(folder)
}

/// `<folder>/footage`, or `None` while the video has no media folder.
pub fn footage_dir(video: &Video) -> Option<PathBuf> {
    video.folder.as_ref().map(|folder| folder.join(FOOTAGE_DIR))
}

/// `<folder>/thumbnails`, where the thumbnails drawn for the video go, or `None`
/// while the video has no media folder.
pub fn thumbnails_dir(video: &Video) -> Option<PathBuf> {
    video.folder.as_ref().map(|folder| folder.join(THUMBNAILS_DIR))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MediaKind {
    Video,
    Audio,
    Image,
}

impl MediaKind {
    /// Classifies a file by extension (case-insensitive).
    pub fn from_path(path: &Path) -> Option<MediaKind> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        let ext = ext.as_str();
        if VIDEO_EXTENSIONS.contains(&ext) {
            Some(MediaKind::Video)
        } else if AUDIO_EXTENSIONS.contains(&ext) {
            Some(MediaKind::Audio)
        } else if IMAGE_EXTENSIONS.contains(&ext) {
            Some(MediaKind::Image)
        } else {
            None
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            MediaKind::Video => "Video",
            MediaKind::Audio => "Audio",
            MediaKind::Image => "Image",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MediaFile {
    pub path: PathBuf,
    pub name: String,
    /// Size in bytes.
    pub size: u64,
    pub modified: Option<DateTime<Utc>>,
    pub kind: MediaKind,
}

/// Media files directly inside `dir`, sorted by name. Subfolders (including
/// `edit/`), hidden files and non-media files are skipped. A missing folder
/// yields an empty list.
pub fn list_footage(dir: &Path) -> Result<Vec<MediaFile>, MediaError> {
    let read_err = |source| MediaError::ReadDir { path: dir.to_path_buf(), source };
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(read_err(err)),
    };

    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(read_err)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let Some(kind) = MediaKind::from_path(&path) else { continue };
        // Follows symlinks so linked takes are listed like regular files.
        let Ok(metadata) = fs::metadata(&path) else { continue };
        if !metadata.is_file() {
            continue;
        }
        let modified = metadata.modified().ok().map(DateTime::<Utc>::from);
        files.push(MediaFile { path, name, size: metadata.len(), modified, kind });
    }
    files.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(files)
}

/// Files video-use leaves in `<footage>/edit/`. Each is `Some` only if it exists.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct EditOutputs {
    /// `edit/final.mp4`, the full-quality render.
    pub final_cut: Option<PathBuf>,
    /// `edit/preview.mp4`, the fast 720p render shown before the final one.
    pub preview: Option<PathBuf>,
    /// `edit/project.md`, video-use's session memory (decisions, cuts, grade).
    pub project_notes: Option<PathBuf>,
}

pub fn edit_outputs(footage_dir: &Path) -> EditOutputs {
    let edit = footage_dir.join(EDIT_DIR);
    let existing = |name: &str| Some(edit.join(name)).filter(|path| path.is_file());
    EditOutputs {
        final_cut: existing("final.mp4"),
        preview: existing("preview.mp4"),
        project_notes: existing("project.md"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(title: &str) -> Video {
        Video::new(title)
    }

    #[test]
    fn folder_name_is_ascii_slug_with_short_id() {
        let v = video("Rust in 100 Seconds… Actually, 10 Minutes!");
        assert_eq!(video_folder_name(&v), format!("rust-in-100-seconds-actually-10-minutes-{}", v.id.short()));
    }

    #[test]
    fn folder_name_handles_unicode_and_empty_titles() {
        let v = video("  Ünïcode — Café 2026 ");
        assert_eq!(video_folder_name(&v), format!("n-code-caf-2026-{}", v.id.short()));
        let v = video("Привет мир");
        assert_eq!(video_folder_name(&v), format!("video-{}", v.id.short()));
        let v = video("");
        assert_eq!(video_folder_name(&v), format!("video-{}", v.id.short()));
    }

    #[test]
    fn videos_with_the_same_title_get_separate_folders() {
        // UUID v7 ids created close together share their leading timestamp hex digits.
        let names: std::collections::HashSet<_> = (0..1000).map(|_| video_folder_name(&video("Untitled"))).collect();
        assert_eq!(names.len(), 1000);
    }

    #[test]
    fn folder_name_is_truncated() {
        let v = video(&"long title ".repeat(20));
        let name = video_folder_name(&v);
        let slug = name.strip_suffix(&format!("-{}", v.id.short())).unwrap();
        assert!(slug.len() <= MAX_SLUG_LEN, "{slug}");
        assert!(!slug.ends_with('-'));
        assert!(name.is_ascii());
    }

    #[test]
    fn ensure_video_folder_creates_footage_and_sets_folder() {
        let root = tempfile::tempdir().unwrap();
        let mut v = video("Desk setup tour");
        assert_eq!(footage_dir(&v), None);

        let folder = ensure_video_folder(root.path(), &mut v).unwrap();
        assert_eq!(folder, root.path().join(video_folder_name(&v)));
        assert_eq!(v.folder.as_deref(), Some(folder.as_path()));
        assert!(folder.join("footage").is_dir());
        assert_eq!(footage_dir(&v), Some(folder.join("footage")));

        // An existing folder is kept even when the title (and so the slug) changes.
        v.title = "Renamed".into();
        assert_eq!(ensure_video_folder(root.path(), &mut v).unwrap(), folder);
    }

    #[test]
    fn list_footage_filters_and_sorts() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        fs::write(p.join("b-roll.MOV"), b"12345").unwrap();
        fs::write(p.join("a-take.mp4"), b"1").unwrap();
        fs::write(p.join("voice.wav"), b"").unwrap();
        fs::write(p.join("thumb.png"), b"").unwrap();
        fs::write(p.join("notes.txt"), b"").unwrap();
        fs::write(p.join(".hidden.mp4"), b"").unwrap();
        fs::create_dir_all(p.join("edit")).unwrap();
        fs::write(p.join("edit/final.mp4"), b"").unwrap();
        fs::create_dir_all(p.join("clips.mp4")).unwrap();

        let files = list_footage(p).unwrap();
        let names: Vec<_> = files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["a-take.mp4", "b-roll.MOV", "thumb.png", "voice.wav"]);
        assert_eq!(files[1].size, 5);
        assert_eq!(files[1].kind, MediaKind::Video);
        assert_eq!(files[2].kind, MediaKind::Image);
        assert_eq!(files[3].kind, MediaKind::Audio);
        assert!(files[0].modified.is_some());
        assert_eq!(files[0].path, p.join("a-take.mp4"));
    }

    #[test]
    fn list_footage_of_missing_dir_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(list_footage(&dir.path().join("nope")).unwrap().is_empty());
    }

    #[test]
    fn edit_outputs_reports_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(edit_outputs(dir.path()), EditOutputs::default());

        let edit = dir.path().join("edit");
        fs::create_dir_all(&edit).unwrap();
        fs::write(edit.join("final.mp4"), b"").unwrap();
        fs::write(edit.join("project.md"), b"").unwrap();
        assert_eq!(
            edit_outputs(dir.path()),
            EditOutputs {
                final_cut: Some(edit.join("final.mp4")),
                preview: None,
                project_notes: Some(edit.join("project.md")),
            }
        );
    }
}
