//! Upload progress arithmetic: how often to report, speed, time left, and how to
//! write sizes and durations for people.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Progress reaches the UI at most this often (10 updates per second).
pub const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
/// Speed is averaged over this much recent history so it does not jitter per chunk.
const RATE_WINDOW: Duration = Duration::from_secs(5);
/// Below this much history the speed is not known yet.
const MIN_RATE_SPAN: Duration = Duration::from_millis(500);

/// Lets an event through at most once per interval.
#[derive(Debug)]
pub struct Throttle {
    interval: Duration,
    last: Option<Instant>,
}

impl Throttle {
    pub fn new(interval: Duration) -> Self {
        Self { interval, last: None }
    }

    /// Whether an event at `now` should be reported. Reporting it starts a new interval.
    pub fn ready(&mut self, now: Instant) -> bool {
        let ready = self.last.is_none_or(|last| now.saturating_duration_since(last) >= self.interval);
        if ready {
            self.last = Some(now);
        }
        ready
    }
}

/// Bytes sent so far plus the recent history needed for speed and time left.
#[derive(Clone, Debug)]
pub struct Transfer {
    sent: u64,
    total: u64,
    samples: VecDeque<(Instant, u64)>,
}

impl Transfer {
    pub fn new(total: u64) -> Self {
        Self { sent: 0, total, samples: VecDeque::new() }
    }

    pub fn sent(&self) -> u64 {
        self.sent
    }

    pub fn total(&self) -> u64 {
        self.total
    }

    /// Records that `sent` of `total` bytes were confirmed or in flight at `at`.
    pub fn record(&mut self, at: Instant, sent: u64, total: u64) {
        // The server may keep less than was sent; history before that is meaningless.
        if sent < self.sent || total != self.total {
            self.samples.clear();
        }
        self.sent = sent;
        self.total = total;
        self.samples.push_back((at, sent));
        while self.samples.len() > 2
            && self.samples.front().is_some_and(|(time, _)| at.duration_since(*time) > RATE_WINDOW)
        {
            self.samples.pop_front();
        }
    }

    /// Fraction done in `0.0..=1.0`.
    pub fn fraction(&self) -> f32 {
        if self.total == 0 { 0.0 } else { (self.sent as f64 / self.total as f64).clamp(0.0, 1.0) as f32 }
    }

    /// Recent speed in bytes per second; `None` until there is enough history.
    pub fn bytes_per_second(&self) -> Option<f64> {
        let (first_time, first_sent) = *self.samples.front()?;
        let (last_time, last_sent) = *self.samples.back()?;
        let span = last_time.duration_since(first_time);
        if span < MIN_RATE_SPAN {
            return None;
        }
        Some(last_sent.saturating_sub(first_sent) as f64 / span.as_secs_f64())
    }

    /// Time left at the recent speed; `None` while the speed is unknown or zero.
    pub fn eta(&self) -> Option<Duration> {
        let speed = self.bytes_per_second().filter(|speed| *speed > 0.0)?;
        Some(Duration::from_secs_f64(self.total.saturating_sub(self.sent) as f64 / speed))
    }
}

/// Decimal units, as file managers and YouTube Studio show sizes: `512 KB`, `12.4 MB`.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    while value >= 999.95 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    let decimals = if value >= 100.0 || unit == 0 { 0 } else { 1 };
    format!("{value:.decimals$} {}", UNITS[unit])
}

pub fn format_speed(bytes_per_second: f64) -> String {
    format!("{}/s", format_bytes(bytes_per_second.max(0.0).round() as u64))
}

/// `12 s left`, `about 4 min left`, `about 1 h 5 min left`.
pub fn format_eta(eta: Duration) -> String {
    let secs = eta.as_secs();
    match secs {
        0..60 => format!("{} s left", secs.max(1)),
        60..3600 => format!("about {} min left", secs.div_ceil(60)),
        _ => format!("about {} h {} min left", secs / 3600, secs % 3600 / 60),
    }
}

/// Video length as a player shows it: `0:42`, `12:05`, `1:02:03`.
pub fn format_duration(secs: f64) -> String {
    let total = secs.max(0.0).round() as u64;
    let (hours, minutes, seconds) = (total / 3600, total % 3600 / 60, total % 60);
    if hours > 0 { format!("{hours}:{minutes:02}:{seconds:02}") } else { format!("{minutes}:{seconds:02}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throttle_reports_at_most_once_per_interval() {
        let start = Instant::now();
        let mut throttle = Throttle::new(PROGRESS_INTERVAL);
        let reported = (0..100).filter(|ms| throttle.ready(start + Duration::from_millis(ms * 10))).count();
        // One second of events every 10 ms: 10 reports.
        assert_eq!(reported, 10);
        assert!(!throttle.ready(start + Duration::from_millis(995)));
        assert!(throttle.ready(start + Duration::from_millis(1000)));
    }

    #[test]
    fn speed_and_eta_follow_recent_history() {
        let start = Instant::now();
        let mut transfer = Transfer::new(10_000_000);
        transfer.record(start, 0, 10_000_000);
        assert_eq!(transfer.bytes_per_second(), None);
        assert_eq!(transfer.eta(), None);

        transfer.record(start + Duration::from_millis(400), 400_000, 10_000_000);
        assert_eq!(transfer.bytes_per_second(), None, "too little history");

        transfer.record(start + Duration::from_secs(2), 2_000_000, 10_000_000);
        assert_eq!(transfer.bytes_per_second(), Some(1_000_000.0));
        assert_eq!(transfer.eta(), Some(Duration::from_secs(8)));
        assert!((transfer.fraction() - 0.2).abs() < f32::EPSILON);

        // Samples older than the window drop out, so a speed-up shows quickly.
        transfer.record(start + Duration::from_secs(8), 8_000_000, 10_000_000);
        transfer.record(start + Duration::from_secs(9), 9_500_000, 10_000_000);
        assert_eq!(transfer.bytes_per_second(), Some(1_500_000.0));
    }

    #[test]
    fn going_backwards_restarts_the_speed_estimate() {
        let start = Instant::now();
        let mut transfer = Transfer::new(1000);
        transfer.record(start, 0, 1000);
        transfer.record(start + Duration::from_secs(1), 800, 1000);
        assert!(transfer.bytes_per_second().is_some());
        transfer.record(start + Duration::from_secs(2), 512, 1000);
        assert_eq!(transfer.sent(), 512);
        assert_eq!(transfer.bytes_per_second(), None);
        assert_eq!(Transfer::new(0).fraction(), 0.0);
    }

    #[test]
    fn formats_sizes_speeds_and_durations() {
        assert_eq!(format_bytes(999), "999 B");
        assert_eq!(format_bytes(1_000), "1 KB");
        assert_eq!(format_bytes(512_000), "512 KB");
        assert_eq!(format_bytes(12_400_000), "12.4 MB");
        assert_eq!(format_bytes(999_960_000), "1.0 GB");
        assert_eq!(format_bytes(1_270_000_000), "1.3 GB");
        assert_eq!(format_speed(8_200_000.0), "8.2 MB/s");
        assert_eq!(format_eta(Duration::from_millis(300)), "1 s left");
        assert_eq!(format_eta(Duration::from_secs(42)), "42 s left");
        assert_eq!(format_eta(Duration::from_secs(61)), "about 2 min left");
        assert_eq!(format_eta(Duration::from_secs(3900)), "about 1 h 5 min left");
        assert_eq!(format_duration(42.4), "0:42");
        assert_eq!(format_duration(725.0), "12:05");
        assert_eq!(format_duration(3723.0), "1:02:03");
    }
}
