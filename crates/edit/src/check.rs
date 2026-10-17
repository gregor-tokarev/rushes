//! Checks on a rendered file, reported as numbers the agent can act on: the agent
//! cannot watch or listen, so it gets measurements instead.

use std::fmt::Write as _;
use std::path::Path;

use crate::audio::{self, ANALYSIS_RATE};
use crate::error::{EditError, Result};
use crate::plan::clock;
use crate::render::{LOUDNESS_TARGET, MAX_TRUE_PEAK, Quality, RenderRecord, last_record};
use crate::session::Session;
use crate::tool;

/// Joins checked for audio placement.
const SYNC_POINTS: usize = 30;

pub fn check(session: &mut Session, quality: Quality, segment_labels: &[(usize, String, bool)]) -> Result<String> {
    let record = last_record(session, quality)?
        .ok_or_else(|| EditError::invalid(format!("nothing rendered yet as {}", quality.file_name())))?;
    if !record.output.is_file() {
        return Err(EditError::invalid(format!("{} is missing; render again", record.output.display())));
    }
    let ffmpeg = tool::ffmpeg()?;
    let ffprobe = tool::ffprobe()?;
    let mut problems = Vec::new();
    let mut out = String::new();

    // Streams: frame count and audio length must match the plan exactly.
    let probe = tool::run(
        &ffprobe,
        [
            "-v".as_ref(),
            "error".as_ref(),
            "-count_packets".as_ref(),
            "-show_entries".as_ref(),
            "stream=codec_type,nb_read_packets,duration".as_ref(),
            "-of".as_ref(),
            "json".as_ref(),
            record.output.as_os_str(),
        ],
    )?;
    let probe: serde_json::Value = serde_json::from_slice(&probe)?;
    let streams = probe["streams"].as_array().cloned().unwrap_or_default();
    let field = |kind: &str, key: &str| -> Option<f64> {
        streams
            .iter()
            .find(|s| s["codec_type"] == kind)
            .and_then(|s| s[key].as_str())
            .and_then(|v| v.parse::<f64>().ok())
    };
    let frames = field("video", "nb_read_packets").unwrap_or(0.0) as u64;
    let frame = record.format.rate.den as f64 / record.format.rate.num as f64;
    let video_duration = frames as f64 * frame;
    // Decoded length: the container's audio duration counts AAC priming and padding.
    let output_audio = decode_mono(&record.output)?;
    let audio_duration = output_audio.len() as f64 / f64::from(ANALYSIS_RATE);
    let _ = writeln!(
        out,
        "{}: {} ({:.2} s), {} video frames (plan: {}), audio {:.3} s vs video {:.3} s.",
        record.output.display(),
        clock(record.duration),
        record.duration,
        frames,
        record.frames,
        audio_duration,
        video_duration
    );
    if frames != record.frames {
        problems.push(format!("the video has {frames} frames, the plan {}", record.frames));
    }
    // The last AAC frame is padded to 1024 samples: allow that much extra sound.
    if audio_duration - video_duration > frame + 0.0214 || video_duration - audio_duration > frame {
        problems.push(format!(
            "audio and video lengths differ by {:.0} ms",
            (audio_duration - video_duration).abs() * 1000.0
        ));
    }

    // Loudness of the result.
    let (_, stderr) = tool::run_with_stderr(
        &ffmpeg,
        [
            "-nostdin".as_ref(),
            "-v".as_ref(),
            "info".as_ref(),
            "-i".as_ref(),
            record.output.as_os_str(),
            "-map".as_ref(),
            "0:a:0".as_ref(),
            "-af".as_ref(),
            "ebur128=peak=true".as_ref(),
            "-f".as_ref(),
            "null".as_ref(),
            "-".as_ref(),
        ],
    )?;
    let summary = stderr.rsplit("Summary:").next().unwrap_or("");
    let value = |label: &str| {
        summary
            .lines()
            .find_map(|line| line.trim().strip_prefix(label))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|v| v.parse::<f64>().ok())
    };
    if let (Some(integrated), Some(peak)) = (value("I:"), value("Peak:")) {
        let _ = writeln!(
            out,
            "Loudness: {integrated:.1} LUFS integrated, true peak {peak:.1} dBTP (target {LOUDNESS_TARGET} LUFS, peak under -1)."
        );
        if (integrated - LOUDNESS_TARGET).abs() > 2.0 {
            problems.push(format!("loudness is {integrated:.1} LUFS, not {LOUDNESS_TARGET}"));
        }
        if peak > MAX_TRUE_PEAK + 0.05 {
            problems.push(format!("true peak {peak:.1} dBTP is over {MAX_TRUE_PEAK} and may clip"));
        }
    }

    // Long silences, and which segment they fall in.
    let (_, stderr) = tool::run_with_stderr(
        &ffmpeg,
        [
            "-nostdin".as_ref(),
            "-v".as_ref(),
            "info".as_ref(),
            "-i".as_ref(),
            record.output.as_os_str(),
            "-map".as_ref(),
            "0:a:0".as_ref(),
            "-af".as_ref(),
            "silencedetect=noise=-45dB:d=1.2".as_ref(),
            "-f".as_ref(),
            "null".as_ref(),
            "-".as_ref(),
        ],
    )?;
    let mut silences = Vec::new();
    let mut open = None;
    for line in stderr.lines() {
        if let Some(rest) = line.split("silence_start: ").nth(1) {
            open = rest.split_whitespace().next().and_then(|v| v.parse::<f64>().ok());
        } else if let Some(rest) = line.split("silence_end: ").nth(1)
            && let (Some(start), Some(end)) =
                (open.take(), rest.split_whitespace().next().and_then(|v| v.parse::<f64>().ok()))
        {
            silences.push((start, end));
        }
    }
    if let Some(start) = open {
        silences.push((start, record.duration));
    }
    let segment_at =
        |time: f64| record.placed.iter().find(|p| time >= p.out_start && time < p.out_end()).map(|p| p.span.segment);
    let mut unexpected = Vec::new();
    for (start, end) in &silences {
        let segment = segment_at((start + end) / 2.0);
        let described = segment.and_then(|segment| segment_labels.iter().find(|(index, _, _)| *index == segment));
        match described {
            // A time segment (a silent demo) is allowed to be quiet.
            Some((_, _, true)) => {}
            Some((index, label, false)) => unexpected.push(format!(
                "{}-{} ({:.1} s) in S{} {label}",
                clock(*start),
                clock(*end),
                end - start,
                index + 1
            )),
            None => unexpected.push(format!("{}-{} ({:.1} s)", clock(*start), clock(*end), end - start)),
        }
    }
    let _ =
        writeln!(out, "Silences of 1.2 s or more: {} ({} inside speech segments).", silences.len(), unexpected.len());
    for line in unexpected.iter().take(15) {
        let _ = writeln!(out, "  - {line}");
    }

    // Audio placement at the joins: where the sound of each span really is.
    let sync = sync_offsets(session, &record, &output_audio)?;
    if !sync.is_empty() {
        let mut abs: Vec<f64> = sync.iter().map(|(_, lag)| lag.abs()).collect();
        abs.sort_by(f64::total_cmp);
        let worst = sync.iter().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).expect("non-empty");
        let _ = writeln!(
            out,
            "Audio placement at {} joins: median offset {:.1} ms, worst {:.1} ms (S{}).",
            sync.len(),
            abs[abs.len() / 2] * 1000.0,
            worst.1 * 1000.0,
            worst.0 + 1
        );
        if worst.1.abs() > 0.03 {
            problems.push(format!("sound is {:.0} ms off at S{}", worst.1 * 1000.0, worst.0 + 1));
        }
    }

    if problems.is_empty() {
        out.push_str("No technical problems found.\n");
    } else {
        let _ = writeln!(out, "Problems:");
        for problem in problems {
            let _ = writeln!(out, "  - {problem}");
        }
    }
    Ok(out)
}

/// For spans spread over the output: (segment, seconds the sound is off from where
/// the plan puts it), measured by matching the output's audio against the take's.
fn sync_offsets(session: &mut Session, record: &RenderRecord, output_audio: &[f32]) -> Result<Vec<(usize, f64)>> {
    let rate = f64::from(ANALYSIS_RATE);
    let candidates: Vec<usize> =
        (0..record.placed.len()).filter(|i| record.placed[*i].samples as f64 / 48_000.0 > 1.0).collect();
    let step = (candidates.len() as f64 / SYNC_POINTS as f64).max(1.0);
    let mut results = Vec::new();
    let mut sources: std::collections::HashMap<String, Vec<f32>> = std::collections::HashMap::new();
    let mut index = 0.0;
    while (index as usize) < candidates.len() {
        let item = &record.placed[candidates[index as usize]];
        index += step;
        let take = session.take(&item.span.take)?.clone();
        if take.media.audio.is_empty() {
            continue;
        }
        if !sources.contains_key(&take.id) {
            let wav = audio::speech_audio(&session.project, &take)?;
            sources.insert(take.id.clone(), load_cached(&wav)?);
        }
        let source = &sources[&take.id];
        let window = (0.4 * rate) as usize;
        let search = (0.08 * rate) as usize;
        let at_out = ((item.out_start + 0.2) * rate) as usize;
        let at_src = ((item.span.start + 0.2) * rate) as usize;
        if at_out + window > output_audio.len() || at_src < search || at_src + window + search > source.len() {
            continue;
        }
        if let Some(lag) =
            best_lag(&output_audio[at_out..at_out + window], &source[at_src - search..at_src + window + search], search)
        {
            results.push((item.span.segment, lag as f64 / rate));
        }
    }
    Ok(results)
}

fn load_cached(wav: &Path) -> Result<Vec<f32>> {
    Ok(audio::read_wav(wav)?.into_iter().map(|s| f32::from(s) / 32768.0).collect())
}

fn decode_mono(path: &Path) -> Result<Vec<f32>> {
    let bytes = tool::run(
        &tool::ffmpeg()?,
        [
            "-nostdin".as_ref(),
            "-v".as_ref(),
            "error".as_ref(),
            "-i".as_ref(),
            path.as_os_str(),
            "-map".as_ref(),
            "0:a:0".as_ref(),
            "-ac".as_ref(),
            "1".as_ref(),
            "-ar".as_ref(),
            ANALYSIS_RATE.to_string().as_ref(),
            "-f".as_ref(),
            "s16le".as_ref(),
            "-".as_ref(),
        ],
    )?;
    Ok(bytes.as_chunks::<2>().0.iter().map(|pair| f32::from(i16::from_le_bytes(*pair)) / 32768.0).collect())
}

/// Offset (samples) of `needle` inside `haystack` relative to `zero`, by normalised
/// cross-correlation; `None` when nothing matches well (silence).
fn best_lag(needle: &[f32], haystack: &[f32], zero: usize) -> Option<isize> {
    let energy: f32 = needle.iter().map(|v| v * v).sum();
    if energy < 1e-4 {
        return None;
    }
    let mut best = (0usize, f32::MIN);
    for offset in 0..=haystack.len() - needle.len() {
        let window = &haystack[offset..offset + needle.len()];
        let dot: f32 = needle.iter().zip(window).map(|(a, b)| a * b).sum();
        let norm: f32 = window.iter().map(|v| v * v).sum::<f32>().sqrt() * energy.sqrt();
        let score = if norm > 0.0 { dot / norm } else { 0.0 };
        if score > best.1 {
            best = (offset, score);
        }
    }
    (best.1 > 0.6).then(|| best.0 as isize - zero as isize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lag_is_found_in_noise_like_signal() {
        let signal: Vec<f32> = (0..4000).map(|i| ((i * 7919 % 1000) as f32 / 500.0 - 1.0) * 0.5).collect();
        let needle = &signal[1030..1830];
        assert_eq!(best_lag(needle, &signal[1000..2000], 30), Some(0));
        assert_eq!(best_lag(needle, &signal[1010..2000], 30), Some(-10));
        assert_eq!(best_lag(&[0.0; 100], &signal[..400], 10), None);
    }
}
