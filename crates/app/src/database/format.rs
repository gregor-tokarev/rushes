//! Text presentation of cell values: script stats, runtimes, dates and numbers.

use chrono::{DateTime, Datelike as _, Local, NaiveDate, Utc};
use rushes_core::{UploadStatus, YoutubeState};

/// Numbers at least this large are shown as Rust formats them, without
/// grouping: past it an `f64` no longer holds every whole number exactly.
const LARGEST_GROUPED_NUMBER: f64 = 1e15;

/// "8m 16s · 1,240 words", or `None` when the script is empty. The runtime
/// leads: it is the planning number, and a narrow column cuts the end.
pub fn script_summary(words: usize, seconds: u32) -> Option<String> {
    (words > 0).then(|| {
        let unit = if words == 1 { "word" } else { "words" };
        format!("{} · {} {unit}", runtime(seconds), group_thousands(words as u64))
    })
}

/// A spoken runtime: "45s", "8m 16s", "1h 02m".
pub fn runtime(seconds: u32) -> String {
    let (hours, minutes, seconds) = (seconds / 3600, seconds % 3600 / 60, seconds % 60);
    if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m {seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

pub fn group_thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (ix, digit) in digits.chars().enumerate() {
        if ix > 0 && (digits.len() - ix).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// A number property value: thousands grouped, at most two decimals, no
/// trailing zeros.
pub fn number(value: f64) -> String {
    if !value.is_finite() || value.abs() >= LARGEST_GROUPED_NUMBER {
        return value.to_string();
    }
    let rounded = (value * 100.0).round() / 100.0;
    let sign = if rounded < 0.0 { "-" } else { "" };
    let magnitude = rounded.abs();
    let whole = group_thousands(magnitude.trunc() as u64);
    let cents = (magnitude.fract() * 100.0).round() as u64;
    match cents {
        0 => format!("{sign}{whole}"),
        cents if cents % 10 == 0 => format!("{sign}{whole}.{}", cents / 10),
        cents => format!("{sign}{whole}.{cents:02}"),
    }
}

/// Why a value typed into a Number property was not saved.
pub fn not_a_number(text: &str, property: &str) -> String {
    format!("“{text}” isn’t a number, so {property} wasn’t changed.")
}

/// A calendar date relative to `today`: "Today", "Tomorrow", "Yesterday",
/// "Mar 4" within the year, "Mar 4, 2027" otherwise.
pub fn date(date: NaiveDate, today: NaiveDate) -> String {
    match (date - today).num_days() {
        0 => "Today".into(),
        1 => "Tomorrow".into(),
        -1 => "Yesterday".into(),
        _ if date.year() == today.year() => date.format("%b %-d").to_string(),
        _ => date.format("%b %-d, %Y").to_string(),
    }
}

/// The calendar day of `at` in the user's time zone, which is what "Today"
/// and publish dates are measured against.
pub fn local_day(at: DateTime<Utc>) -> NaiveDate {
    at.with_timezone(&Local).date_naive()
}

/// Whether a video already on YouTube is still waiting for its publish time.
pub fn is_scheduled(publish_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    publish_at.is_some_and(|at| at > now)
}

/// A video whose upload YouTube no longer has (deleted in YouTube Studio, say),
/// in the words of the table, the board, the video page and the Publish tab.
pub const YOUTUBE_GONE: &str = "Not on YouTube anymore";
/// [`YOUTUBE_GONE`] in a chip, which has little room, as the Uploads page says it.
pub const YOUTUBE_GONE_CHIP: &str = "Not on YouTube";
/// What [`YOUTUBE_GONE`] means.
pub const YOUTUBE_GONE_DETAIL: &str = "YouTube no longer has this video. It may have been deleted in YouTube Studio.";

/// The YouTube column: the video's visibility on YouTube ("Scheduled" until
/// its publish time), or the state of its upload attempt.
pub fn youtube(state: YoutubeState<'_>, scheduled: bool) -> String {
    match state {
        YoutubeState::Live(_) if scheduled => "Scheduled".into(),
        YoutubeState::Live(link) => link.privacy.label().into(),
        YoutubeState::Upload(status) => upload(status),
    }
}

/// Upload state with progress, e.g. "Uploading 42%".
pub fn upload(status: &UploadStatus) -> String {
    match status {
        UploadStatus::Uploading { sent, total } if *total > 0 => {
            format!("{} {}%", status.label(), sent.saturating_mul(100) / total)
        }
        status => status.label().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone as _;

    use super::*;

    #[test]
    fn script_summary_reads_like_prose() {
        assert_eq!(script_summary(1240, 496).as_deref(), Some("8m 16s · 1,240 words"));
        assert_eq!(script_summary(1, 1).as_deref(), Some("1s · 1 word"));
        assert_eq!(script_summary(0, 0), None);
    }

    #[test]
    fn runtimes_pick_the_largest_unit() {
        assert_eq!(runtime(45), "45s");
        assert_eq!(runtime(65), "1m 05s");
        assert_eq!(runtime(3725), "1h 02m");
    }

    #[test]
    fn numbers_are_grouped_and_trimmed() {
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(1_234_567), "1,234,567");
        assert_eq!(number(1250.0), "1,250");
        assert_eq!(number(3.5), "3.5");
        assert_eq!(number(-0.125), "-0.13");
        assert_eq!(number(2.05), "2.05");
        // Past exact whole numbers there is no grouping to get right.
        assert_eq!(number(1e20), "100000000000000000000");
        assert_eq!(number(-2.5e18), "-2500000000000000000");
    }

    #[test]
    fn dates_are_relative_to_today() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 9).unwrap();
        let day = |month, day| NaiveDate::from_ymd_opt(2026, month, day).unwrap();
        assert_eq!(date(today, today), "Today");
        assert_eq!(date(day(10, 10), today), "Tomorrow");
        assert_eq!(date(day(10, 8), today), "Yesterday");
        assert_eq!(date(day(3, 4), today), "Mar 4");
        assert_eq!(date(NaiveDate::from_ymd_opt(2027, 1, 2).unwrap(), today), "Jan 2, 2027");
    }

    #[test]
    fn youtube_shows_visibility_once_the_video_is_there() {
        let now = Utc.with_ymd_and_hms(2026, 10, 9, 12, 0, 0).unwrap();
        let link = rushes_core::YoutubeLink {
            video_id: "abc".into(),
            url: "https://www.youtube.com/watch?v=abc".into(),
            channel_id: "UC1".into(),
            privacy: rushes_core::Privacy::Unlisted,
            uploaded_at: now,
            thumbnail: None,
            publish_at: None,
            ab_test: None,
        };
        let live = YoutubeState::Live(&link);
        assert_eq!(youtube(live, false), "Unlisted");
        assert_eq!(youtube(live, true), "Scheduled");
        assert!(is_scheduled(Some(now + chrono::Duration::days(1)), now));
        assert!(!is_scheduled(Some(now - chrono::Duration::days(1)), now));
        assert!(!is_scheduled(None, now));
        assert_eq!(youtube(YoutubeState::Upload(&UploadStatus::Cancelled), false), "Cancelled");
    }

    #[test]
    fn uploads_show_progress() {
        assert_eq!(upload(&UploadStatus::Uploading { sent: 42, total: 100 }), "Uploading 42%");
        assert_eq!(upload(&UploadStatus::Uploading { sent: 0, total: 0 }), "Uploading");
        assert_eq!(upload(&UploadStatus::Done { video_id: "abc".into() }), "Uploaded");
    }
}
