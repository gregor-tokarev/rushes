//! Compact, human-readable values for the AI edit views.

use std::time::Duration;

use chrono::{DateTime, Local, Utc};
use videocrm_core::document::strip_inline_markdown;
use videocrm_pipeline::MediaInfo;

/// File size in decimal units: "820 KB", "1.4 GB".
pub fn bytes(size: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if size < 1000 {
        return format!("{size} B");
    }
    let mut value = size as f64;
    let mut unit = "B";
    for next in UNITS {
        if value < 1000.0 {
            break;
        }
        value /= 1000.0;
        unit = next;
    }
    if value < 10.0 { format!("{value:.1} {unit}") } else { format!("{value:.0} {unit}") }
}

/// A media duration like a player shows it: "0:07", "12:30", "1:02:05".
pub fn media_duration(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    let (hours, minutes, seconds) = (total / 3600, total / 60 % 60, total % 60);
    if hours > 0 { format!("{hours}:{minutes:02}:{seconds:02}") } else { format!("{minutes}:{seconds:02}") }
}

/// Duration, frame size and audio of a probed file: "0:07 · 1920×1080", "3:12 · audio".
pub fn media_info(info: &MediaInfo) -> String {
    let duration = media_duration(info.duration_secs);
    match (info.width, info.height) {
        (Some(width), Some(height)) if info.has_audio => format!("{duration} · {width}×{height}"),
        (Some(width), Some(height)) => format!("{duration} · {width}×{height} · no audio"),
        _ => format!("{duration} · audio"),
    }
}

/// How long something has been running: "8s", "2m 05s", "1h 04m".
pub fn elapsed(duration: Duration) -> String {
    let secs = duration.as_secs();
    match (secs / 3600, secs / 60 % 60, secs % 60) {
        (0, 0, seconds) => format!("{seconds}s"),
        (0, minutes, seconds) => format!("{minutes}m {seconds:02}s"),
        (hours, minutes, _) => format!("{hours}h {minutes:02}m"),
    }
}

/// Agent spend in dollars: "$0.04", "<$0.01", "$1.20".
pub fn cost(usd: f64) -> String {
    if usd > 0.0 && usd < 0.005 { "<$0.01".to_owned() } else { format!("${usd:.2}") }
}

/// When a run started, in local time: "Today 14:02", "Yesterday 09:15", "Oct 3, 18:40".
pub fn started(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let (at, now) = (at.with_timezone(&Local), now.with_timezone(&Local));
    let time = at.format("%H:%M");
    match (now.date_naive() - at.date_naive()).num_days() {
        0 => format!("Today {time}"),
        1 => format!("Yesterday {time}"),
        _ if at.format("%Y").to_string() == now.format("%Y").to_string() => at.format("%b %-d, %H:%M").to_string(),
        _ => at.format("%b %-d %Y, %H:%M").to_string(),
    }
}

/// Characters from which an agent message is shown as a preview in the activity log.
const PREVIEW_CHARS: usize = 280;
/// Non-blank lines from which an agent message is shown as a preview.
const PREVIEW_LINES: usize = 4;

/// The start of a long agent message as one line of plain text (Markdown markers
/// removed), for a clamped preview; `None` when the message is short enough to show
/// whole.
pub fn message_preview(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    if text.trim().chars().count() <= PREVIEW_CHARS && lines.len() < PREVIEW_LINES {
        return None;
    }
    let mut preview = String::new();
    for line in lines.iter().filter(|line| !line.starts_with("```")) {
        if preview.chars().count() > 2 * PREVIEW_CHARS {
            break;
        }
        let plain = strip_inline_markdown(strip_block_marker(line));
        if !preview.is_empty() {
            preview.push(' ');
        }
        preview.push_str(plain.trim());
    }
    // The view clamps it to a few lines; more text would only be shaped and cut.
    Some(preview.chars().take(2 * PREVIEW_CHARS).collect())
}

/// A Markdown line without its heading, list or quote marker.
fn strip_block_marker(line: &str) -> &str {
    let line = line.trim_start_matches('#').trim_start_matches('>').trim_start();
    if let Some(rest) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")).or_else(|| line.strip_prefix("+ "))
    {
        return rest;
    }
    let digits = line.find(|c: char| !c.is_ascii_digit()).unwrap_or(0);
    match line[digits..].strip_prefix(". ") {
        Some(rest) if digits > 0 => rest,
        _ => line,
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone as _;

    use super::*;

    #[test]
    fn sizes_use_decimal_units() {
        assert_eq!(bytes(999), "999 B");
        assert_eq!(bytes(1_234), "1.2 KB");
        assert_eq!(bytes(820_000), "820 KB");
        assert_eq!(bytes(1_400_000_000), "1.4 GB");
    }

    #[test]
    fn media_reads_like_a_player() {
        assert_eq!(media_duration(3.02), "0:03");
        assert_eq!(media_duration(3725.0), "1:02:05");
        let video = MediaInfo { duration_secs: 7.0, width: Some(1920), height: Some(1080), has_audio: true };
        assert_eq!(media_info(&video), "0:07 · 1920×1080");
        let silent = MediaInfo { has_audio: false, ..video.clone() };
        assert_eq!(media_info(&silent), "0:07 · 1920×1080 · no audio");
        let audio = MediaInfo { duration_secs: 192.0, width: None, height: None, has_audio: true };
        assert_eq!(media_info(&audio), "3:12 · audio");
    }

    #[test]
    fn elapsed_and_cost_stay_short() {
        assert_eq!(elapsed(Duration::from_secs(8)), "8s");
        assert_eq!(elapsed(Duration::from_secs(125)), "2m 05s");
        assert_eq!(elapsed(Duration::from_secs(3840)), "1h 04m");
        assert_eq!(cost(0.0123), "$0.01");
        assert_eq!(cost(0.001), "<$0.01");
        assert_eq!(cost(0.0), "$0.00");
        assert_eq!(cost(1.2), "$1.20");
    }

    #[test]
    fn start_times_are_relative_to_today() {
        let now = Local.with_ymd_and_hms(2026, 10, 9, 15, 0, 0).unwrap().with_timezone(&Utc);
        let at =
            |day, hour, minute| Local.with_ymd_and_hms(2026, 10, day, hour, minute, 0).unwrap().with_timezone(&Utc);
        assert_eq!(started(at(9, 14, 2), now), "Today 14:02");
        assert_eq!(started(at(8, 9, 15), now), "Yesterday 09:15");
        assert_eq!(started(at(3, 18, 40), now), "Oct 3, 18:40");
        let last_year = Local.with_ymd_and_hms(2025, 12, 31, 8, 0, 0).unwrap().with_timezone(&Utc);
        assert_eq!(started(last_year, now), "Dec 31 2025, 08:00");
    }

    #[test]
    fn long_messages_are_previewed_as_plain_text() {
        let summary = "## The cut\n\nThe final cut is rendered: **`edit/final.mp4`**, 14.6s.\n\n**What's in it:**\n\
                       1. **Intro:** take 1.\n- *Main point:* take 2.\n> Outro: take 3.";
        assert_eq!(
            message_preview(summary).as_deref(),
            Some(
                "The cut The final cut is rendered: edit/final.mp4, 14.6s. What's in it: Intro: take 1. \
                 Main point: take 2. Outro: take 3."
            )
        );
        // Short messages show whole, as Markdown.
        assert_eq!(message_preview("Rendering the **preview**."), None);
        assert_eq!(message_preview("One.\nTwo.\nThree."), None);
        assert!(message_preview(&"word ".repeat(100)).is_some());
        // Code blocks lose their fences; very long messages only their start.
        assert_eq!(message_preview("Ran:\n```\nls edit\n```\nDone.").as_deref(), Some("Ran: ls edit Done."));
        let long = "A sentence of the summary. ".repeat(200);
        assert_eq!(message_preview(&format!("{long}\n{long}")).unwrap().chars().count(), 2 * PREVIEW_CHARS);
    }
}
