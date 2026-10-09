//! Footage discovery and probing with `ffprobe`.

use std::ffi::OsStr;
use std::io;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use crate::exec::{find_executable, run_captured};

/// File extensions video-use can take as source footage (lowercase).
const MEDIA_EXTENSIONS: [&str; 15] = [
    "mp4", "mov", "m4v", "mkv", "webm", "avi", "mts", "m2ts", "mxf", "mp3", "wav", "m4a", "aac",
    "flac", "ogg",
];

const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq)]
pub struct MediaInfo {
    pub duration_secs: f64,
    /// Frame size of the first video stream; `None` for audio-only files.
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub has_audio: bool,
}

/// A source file in the footage folder.
#[derive(Clone, Debug, PartialEq)]
pub struct FootageFile {
    /// File name relative to the footage folder.
    pub name: String,
    /// `None` when ffprobe is unavailable or cannot read the file.
    pub media: Option<MediaInfo>,
}

/// Duration, frame size and audio presence of a media file, via `ffprobe` on `PATH`.
pub fn probe_media(path: &Path) -> Option<MediaInfo> {
    let ffprobe = find_executable(Path::new("ffprobe"), std::env::var_os("PATH").as_deref())?;
    let args = [
        OsStr::new("-v"),
        OsStr::new("error"),
        OsStr::new("-print_format"),
        OsStr::new("json"),
        OsStr::new("-show_format"),
        OsStr::new("-show_streams"),
        path.as_os_str(),
    ];
    let json = run_captured(&ffprobe, args, &[], PROBE_TIMEOUT)?;
    parse_probe(&json)
}

/// Media files directly inside `dir` (hidden files and sub-folders such as `edit/`
/// are skipped), sorted by name and probed.
pub fn list_footage(dir: &Path) -> io::Result<Vec<FootageFile>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let is_media = Path::new(&name)
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|ext| MEDIA_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()));
        // `metadata` follows symlinks, so linked takes count too.
        if is_media && !name.starts_with('.') && entry.path().metadata()?.is_file() {
            names.push(name);
        }
    }
    names.sort();
    Ok(names
        .into_iter()
        .map(|name| FootageFile {
            media: probe_media(&dir.join(&name)),
            name,
        })
        .collect())
}

#[derive(Deserialize)]
struct Probe {
    #[serde(default)]
    streams: Vec<ProbeStream>,
    format: Option<ProbeFormat>,
}

#[derive(Deserialize)]
struct ProbeStream {
    codec_type: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    duration: Option<String>,
    disposition: Option<ProbeDisposition>,
}

#[derive(Deserialize)]
struct ProbeDisposition {
    #[serde(default)]
    attached_pic: u8,
}

#[derive(Deserialize)]
struct ProbeFormat {
    duration: Option<String>,
}

fn parse_probe(json: &str) -> Option<MediaInfo> {
    let probe: Probe = serde_json::from_str(json).ok()?;
    let seconds = |text: &Option<String>| text.as_deref().and_then(|text| text.parse::<f64>().ok());
    // Cover art in audio files shows up as a one-frame video stream.
    let video = probe.streams.iter().find(|stream| {
        stream.codec_type.as_deref() == Some("video")
            && stream
                .disposition
                .as_ref()
                .is_none_or(|d| d.attached_pic == 0)
    });
    let has_audio = probe
        .streams
        .iter()
        .any(|stream| stream.codec_type.as_deref() == Some("audio"));
    if video.is_none() && !has_audio {
        return None;
    }
    let duration_secs = probe
        .format
        .as_ref()
        .and_then(|format| seconds(&format.duration))
        .or_else(|| {
            probe
                .streams
                .iter()
                .filter_map(|stream| seconds(&stream.duration))
                .reduce(f64::max)
        })?;
    Some(MediaInfo {
        duration_secs,
        width: video.and_then(|stream| stream.width),
        height: video.and_then(|stream| stream.height),
        has_audio,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_video_with_audio() {
        let json = r#"{"streams":[
            {"codec_type":"video","width":1280,"height":720,"duration":"3.000000","disposition":{"attached_pic":0}},
            {"codec_type":"audio","duration":"3.010000"}],
            "format":{"duration":"3.023000"}}"#;
        assert_eq!(
            parse_probe(json),
            Some(MediaInfo {
                duration_secs: 3.023,
                width: Some(1280),
                height: Some(720),
                has_audio: true
            })
        );
    }

    #[test]
    fn cover_art_is_not_video() {
        let json = r#"{"streams":[
            {"codec_type":"audio","duration":"61.5"},
            {"codec_type":"video","width":600,"height":600,"disposition":{"attached_pic":1}}],
            "format":{}}"#;
        let info = parse_probe(json).expect("audio file");
        assert_eq!(
            (info.duration_secs, info.width, info.has_audio),
            (61.5, None, true)
        );
    }

    #[test]
    fn rejects_non_media() {
        assert_eq!(
            parse_probe(r#"{"streams":[],"format":{"duration":"1.0"}}"#),
            None
        );
        assert_eq!(parse_probe("not json"), None);
    }
}
