//! The speech audio of a take: which track holds it, a 16 kHz mono copy for
//! transcription and analysis, and its loudness envelope for snapping cuts.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{EditError, Result};
use crate::project::{Project, Take};
use crate::tool;

/// Sample rate of the analysis copy.
pub const ANALYSIS_RATE: u32 = 16_000;

/// Decodes on the container's clock, the one players and video frames use: sample `n`
/// sits at `n / rate` seconds. Timestamp gaps in the source (screen recorders leave
/// some, Meld Studio one of 22 ms after the first frame) are filled with silence; a plain
/// decode would close them and put all later sound early against the picture.
fn on_container_clock(rate: u32) -> String {
    format!("aresample={rate}:async=1:min_hard_comp=0.001:first_pts=0")
}

/// Bumped when the decoded clock changes, so older cached copies are made again.
pub(crate) const CLOCK_VERSION: &str = "c1";
/// Envelope resolution: 5 ms.
pub const ENVELOPE_HOP: usize = 80;

/// The audio track with the most signal (OBS-style recordings put the game or desktop
/// on one track and the microphone on another).
pub fn speech_track(take: &Take) -> Result<usize> {
    if take.media.audio.len() <= 1 {
        return Ok(0);
    }
    let ffmpeg = tool::ffmpeg()?;
    let mut best = (0, f64::NEG_INFINITY);
    for track in &take.media.audio {
        let (_, stderr) = tool::run_with_stderr(
            &ffmpeg,
            [
                "-nostdin".as_ref(),
                "-v".as_ref(),
                "info".as_ref(),
                "-i".as_ref(),
                take.path.as_os_str(),
                "-map".as_ref(),
                format!("0:a:{}", track.index).as_ref(),
                "-af".as_ref(),
                "volumedetect".as_ref(),
                "-f".as_ref(),
                "null".as_ref(),
                "-".as_ref(),
            ],
        )?;
        let mean = stderr
            .lines()
            .find_map(|line| line.split("mean_volume:").nth(1))
            .and_then(|value| value.trim().trim_end_matches("dB").trim().parse::<f64>().ok())
            .unwrap_or(f64::NEG_INFINITY);
        if mean > best.1 {
            best = (track.index, mean);
        }
    }
    Ok(best.0)
}

/// `edit/.cache/audio/<stem>.wav`: the speech track as 16 kHz mono 16-bit PCM.
/// Extracted once per take and reused while the source is unchanged.
pub fn speech_audio(project: &Project, take: &Take) -> Result<PathBuf> {
    let path = project.edit_path(&format!(".cache/audio/{}.{CLOCK_VERSION}.wav", take.stem()))?;
    let fresh = match (fs::metadata(&path), fs::metadata(&take.path)) {
        (Ok(copy), Ok(source)) => copy.modified()? >= source.modified()?,
        _ => false,
    };
    if fresh {
        return Ok(path);
    }
    let track = speech_track(take)?;
    let tmp = path.with_extension("tmp.wav");
    tool::run(
        &tool::ffmpeg()?,
        [
            "-nostdin".as_ref(),
            "-y".as_ref(),
            "-v".as_ref(),
            "error".as_ref(),
            "-i".as_ref(),
            take.path.as_os_str(),
            "-map".as_ref(),
            format!("0:a:{track}").as_ref(),
            "-vn".as_ref(),
            "-af".as_ref(),
            on_container_clock(ANALYSIS_RATE).as_ref(),
            "-ac".as_ref(),
            "1".as_ref(),
            "-c:a".as_ref(),
            "pcm_s16le".as_ref(),
            tmp.as_os_str(),
        ],
    )?;
    fs::rename(&tmp, &path)?;
    Ok(path)
}

/// `edit/.cache/audio/<stem>.48k.pcm`: the speech track decoded from the start as 48 kHz
/// stereo 16-bit PCM, sample 0 at time 0.
///
/// The renderer cuts its audio from here by sample index. Seeking with ffmpeg instead
/// (`-ss` before `-i`) shifts AAC audio by the encoder delay (about 22 ms), a constant
/// lip-sync error in every span.
pub fn render_audio(project: &Project, take: &Take) -> Result<PathBuf> {
    let path = project.edit_path(&format!(".cache/audio/{}.{CLOCK_VERSION}.48k.pcm", take.stem()))?;
    let fresh = match (fs::metadata(&path), fs::metadata(&take.path)) {
        (Ok(copy), Ok(source)) => copy.modified()? >= source.modified()?,
        _ => false,
    };
    if fresh {
        return Ok(path);
    }
    let track = speech_track(take)?;
    let tmp = path.with_extension("tmp.pcm");
    tool::run(
        &tool::ffmpeg()?,
        [
            "-nostdin".as_ref(),
            "-y".as_ref(),
            "-v".as_ref(),
            "error".as_ref(),
            "-i".as_ref(),
            take.path.as_os_str(),
            "-map".as_ref(),
            format!("0:a:{track}").as_ref(),
            "-vn".as_ref(),
            "-af".as_ref(),
            format!("{},aformat=sample_fmts=s16:channel_layouts=stereo", on_container_clock(48_000)).as_ref(),
            "-f".as_ref(),
            "s16le".as_ref(),
            "-c:a".as_ref(),
            "pcm_s16le".as_ref(),
            tmp.as_os_str(),
        ],
    )?;
    fs::rename(&tmp, &path)?;
    Ok(path)
}

/// A smaller lossless copy for the upload.
pub fn flac_for_upload(wav: &Path) -> Result<PathBuf> {
    let flac = wav.with_extension("upload.flac");
    tool::run(
        &tool::ffmpeg()?,
        [
            "-nostdin".as_ref(),
            "-y".as_ref(),
            "-v".as_ref(),
            "error".as_ref(),
            "-i".as_ref(),
            wav.as_os_str(),
            "-c:a".as_ref(),
            "flac".as_ref(),
            flac.as_os_str(),
        ],
    )?;
    Ok(flac)
}

/// Samples of a 16-bit PCM WAV file (first channel layout as stored).
pub fn read_wav(path: &Path) -> Result<Vec<i16>> {
    let bytes = fs::read(path)?;
    let invalid = || EditError::invalid(format!("{} is not a 16-bit PCM WAV file", path.display()));
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(invalid());
    }
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().expect("4 bytes")) as usize;
        let body = at + 8;
        if id == b"data" {
            let end = (body + size).min(bytes.len());
            return Ok(bytes[body..end].as_chunks::<2>().0.iter().map(|pair| i16::from_le_bytes(*pair)).collect());
        }
        at = body + size + (size & 1);
    }
    Err(invalid())
}

/// Peak level in dBFS (`-inf` for digital silence).
pub fn peak_db(wav: &Path) -> Result<f64> {
    let peak = read_wav(wav)?.iter().map(|sample| sample.unsigned_abs()).max().unwrap_or(0);
    Ok(if peak == 0 { f64::NEG_INFINITY } else { 20.0 * (f64::from(peak) / 32768.0).log10() })
}

/// Loudness in dBFS of consecutive 5 ms windows of 16 kHz audio.
#[derive(Clone, Debug)]
pub struct Envelope {
    pub db: Vec<f32>,
}

impl Envelope {
    pub fn of(samples: &[i16]) -> Self {
        let db = samples
            .chunks(ENVELOPE_HOP)
            .map(|chunk| {
                let power: f64 = chunk.iter().map(|s| f64::from(*s) * f64::from(*s)).sum::<f64>() / chunk.len() as f64;
                if power <= 0.0 { -120.0 } else { (10.0 * (power / (32768.0 * 32768.0)).log10()) as f32 }
            })
            .collect();
        Self { db }
    }

    pub fn hop_seconds() -> f64 {
        ENVELOPE_HOP as f64 / f64::from(ANALYSIS_RATE)
    }

    /// The quietest moment in `[from, to]` (seconds), preferring the one closest to
    /// `near` among near-equally quiet windows (within 2 dB).
    pub fn quietest(&self, from: f64, to: f64, near: f64) -> f64 {
        if to <= from || self.db.is_empty() {
            return near.clamp(from.min(to), to.max(from));
        }
        let hop = Self::hop_seconds();
        let first = ((from / hop).ceil() as usize).min(self.db.len() - 1);
        let last = ((to / hop).floor() as usize).min(self.db.len() - 1);
        if last < first {
            return near.clamp(from, to);
        }
        let floor = self.db[first..=last].iter().copied().fold(f32::INFINITY, f32::min);
        let center = |index: usize| (index as f64 + 0.5) * hop;
        let best = (first..=last)
            .filter(|index| self.db[*index] <= floor + 2.0)
            .min_by(|a, b| (center(*a) - near).abs().total_cmp(&(center(*b) - near).abs()))
            .unwrap_or(first);
        center(best).clamp(from, to)
    }

    /// Mean level of `[from, to]` in dBFS.
    pub fn level(&self, from: f64, to: f64) -> f32 {
        let hop = Self::hop_seconds();
        let first = (from / hop).floor().max(0.0) as usize;
        let last = ((to / hop).ceil() as usize).min(self.db.len());
        if last <= first {
            return -120.0;
        }
        self.db[first..last].iter().sum::<f32>() / (last - first) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quietest_moment_is_found_near_the_hint() {
        // 1 s of loud signal with a quiet dip at 0.5 s and another at 0.9 s.
        let mut samples = vec![10_000i16; 16_000];
        for s in &mut samples[7_920..8_080] {
            *s = 0;
        }
        for s in &mut samples[14_320..14_480] {
            *s = 0;
        }
        let envelope = Envelope::of(&samples);
        let at = envelope.quietest(0.3, 0.7, 0.4);
        assert!((at - 0.5).abs() < 0.006, "{at}");
        let at = envelope.quietest(0.3, 1.0, 0.95);
        assert!((at - 0.9).abs() < 0.006, "{at}");
    }
}
