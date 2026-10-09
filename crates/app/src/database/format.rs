//! Text presentation of cell values: script stats, runtimes, dates and numbers.

use chrono::{DateTime, Datelike as _, NaiveDate, Utc};
use videocrm_core::UploadStatus;

/// "1,240 words · 8m 16s", or `None` when the script is empty.
pub fn script_summary(words: usize, seconds: u32) -> Option<String> {
    (words > 0).then(|| {
        let unit = if words == 1 { "word" } else { "words" };
        format!("{} {unit} · {}", group_thousands(words as u64), runtime(seconds))
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
        if ix > 0 && (digits.len() - ix) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// A number property value: thousands grouped, at most two decimals, no
/// trailing zeros.
pub fn number(value: f64) -> String {
    if !value.is_finite() {
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

/// A past timestamp relative to `now`: "Just now", "5m ago", "3h ago", then
/// calendar dates.
pub fn timestamp(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let elapsed = now - at;
    if elapsed.num_minutes() < 1 {
        "Just now".into()
    } else if elapsed.num_hours() < 1 {
        format!("{}m ago", elapsed.num_minutes())
    } else if elapsed.num_hours() < 24 {
        format!("{}h ago", elapsed.num_hours())
    } else {
        date(at.date_naive(), now.date_naive())
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
        assert_eq!(script_summary(1240, 496).as_deref(), Some("1,240 words · 8m 16s"));
        assert_eq!(script_summary(1, 1).as_deref(), Some("1 word · 1s"));
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
    fn timestamps_count_back_from_now() {
        let now = Utc.with_ymd_and_hms(2026, 10, 9, 12, 0, 0).unwrap();
        assert_eq!(timestamp(now, now), "Just now");
        assert_eq!(timestamp(now - chrono::Duration::minutes(5), now), "5m ago");
        assert_eq!(timestamp(now - chrono::Duration::hours(3), now), "3h ago");
        assert_eq!(timestamp(now - chrono::Duration::days(1), now), "Yesterday");
        assert_eq!(timestamp(now - chrono::Duration::days(40), now), "Aug 30");
    }

    #[test]
    fn uploads_show_progress() {
        assert_eq!(upload(&UploadStatus::Uploading { sent: 42, total: 100 }), "Uploading 42%");
        assert_eq!(upload(&UploadStatus::Uploading { sent: 0, total: 0 }), "Uploading");
        assert_eq!(upload(&UploadStatus::Done { video_id: "abc".into() }), "Uploaded");
    }
}
