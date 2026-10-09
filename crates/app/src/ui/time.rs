//! The one way the app writes a point in time.

use chrono::{DateTime, Datelike as _, Local, Utc};

/// When something happened, relative to today in local time: `Today, 14:02`,
/// `Yesterday, 09:10`, `Oct 7, 14:02`, or `Oct 7, 2025` in another year.
pub fn format_when(at: DateTime<Utc>, now: DateTime<Local>) -> String {
    let at = at.with_timezone(&Local);
    match (now.date_naive() - at.date_naive()).num_days() {
        0 => format!("Today, {}", at.format("%H:%M")),
        1 => format!("Yesterday, {}", at.format("%H:%M")),
        _ if at.year() == now.year() => at.format("%b %-d, %H:%M").to_string(),
        _ => at.format("%b %-d, %Y").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use chrono::{NaiveDate, TimeZone as _};

    use super::*;

    #[test]
    fn times_read_relative_to_today() {
        let local = |y, m, d, h, min| {
            let naive = NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, min, 0).unwrap();
            Local.from_local_datetime(&naive).earliest().unwrap()
        };
        let now = local(2026, 10, 9, 18, 30);
        let utc = |at: DateTime<Local>| at.with_timezone(&Utc);
        assert_eq!(format_when(utc(local(2026, 10, 9, 14, 2)), now), "Today, 14:02");
        assert_eq!(format_when(utc(local(2026, 10, 8, 9, 10)), now), "Yesterday, 09:10");
        assert_eq!(format_when(utc(local(2026, 10, 1, 7, 5)), now), "Oct 1, 07:05");
        assert_eq!(format_when(utc(local(2025, 12, 31, 23, 0)), now), "Dec 31, 2025");
    }
}
