//! How a video does on YouTube, day by day: views and watch time from the YouTube
//! Analytics API, thumbnail impressions and their click rate from the channel's reach
//! reports in the YouTube Reporting API. Both need
//! [`TEST_SCOPES`](crate::TEST_SCOPES), and both APIs turned on in the Google Cloud
//! project of the OAuth client.

use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::client::YoutubeClient;
use crate::error::{Error, Result};
use crate::http::{self, bearer};

/// The reach report: thumbnail impressions and click rate per video and day.
pub const REACH_REPORT_TYPE: &str = "channel_reach_basic_a1";
/// Name of the reporting job Rushes creates when the channel has none.
const REACH_JOB_NAME: &str = "Rushes reach";

/// Views and watch time of one video on one day (a Pacific-time day, as YouTube
/// Analytics counts them).
#[derive(Clone, Debug, PartialEq)]
pub struct DailyViews {
    pub date: NaiveDate,
    pub views: u64,
    pub watch_minutes: f64,
}

/// How often YouTube showed the thumbnail of one video on one day, and the share of
/// those impressions that were clicked.
#[derive(Clone, Debug, PartialEq)]
pub struct DailyReach {
    pub date: NaiveDate,
    pub impressions: u64,
    /// Clicks per impression, from 0 to 1.
    pub click_rate: f64,
}

/// One daily reach report of the channel. YouTube may publish a day again (with
/// corrected numbers): the report with the latest `created` counts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReachReport {
    pub id: String,
    /// The day the report covers, from midnight Pacific time.
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub created: DateTime<Utc>,
    download_url: String,
}

impl ReachReport {
    /// A report as the Reporting API lists it, for tests of code that reads them.
    pub fn new(id: &str, start: DateTime<Utc>, end: DateTime<Utc>, created: DateTime<Utc>, url: &str) -> Self {
        Self { id: id.to_owned(), start, end, created, download_url: url.to_owned() }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AnalyticsReport {
    #[serde(default)]
    column_headers: Vec<ColumnHeader>,
    #[serde(default)]
    rows: Vec<Vec<Value>>,
}

#[derive(Deserialize)]
struct ColumnHeader {
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobList {
    #[serde(default)]
    jobs: Vec<Job>,
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Job {
    id: String,
    report_type_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReportList {
    #[serde(default)]
    reports: Vec<ReportResource>,
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReportResource {
    id: String,
    start_time: DateTime<Utc>,
    end_time: DateTime<Utc>,
    create_time: DateTime<Utc>,
    download_url: String,
}

impl YoutubeClient {
    /// Views and watch time of the channel's video `video_id` for each day from
    /// `start` to `end` (inclusive) that YouTube has numbers for, by day.
    pub fn daily_views(&self, video_id: &str, start: NaiveDate, end: NaiveDate) -> Result<Vec<DailyViews>> {
        let url = format!("{}/reports", self.config.analytics_base);
        let (start, end) = (start.format("%Y-%m-%d").to_string(), end.format("%Y-%m-%d").to_string());
        let filter = format!("video=={video_id}");
        let report: AnalyticsReport = self.get_json(
            &url,
            &[
                ("ids", "channel==MINE"),
                ("startDate", &start),
                ("endDate", &end),
                ("metrics", "views,estimatedMinutesWatched"),
                ("dimensions", "day"),
                ("filters", &filter),
                ("sort", "day"),
            ],
        )?;
        parse_daily_views(&report)
    }

    /// The id of the channel's reporting job for [`REACH_REPORT_TYPE`]; creates
    /// the job when there is none. YouTube makes the first reports of a new job
    /// within two days, with the 30 days before it.
    pub fn reach_job(&self) -> Result<String> {
        let url = format!("{}/jobs", self.config.reporting_base);
        let mut page_token: Option<String> = None;
        loop {
            let query: Vec<(&str, &str)> = page_token.iter().map(|token| ("pageToken", token.as_str())).collect();
            let page: JobList = self.get_json(&url, &query)?;
            if let Some(job) = page.jobs.into_iter().find(|job| job.report_type_id == REACH_REPORT_TYPE) {
                return Ok(job.id);
            }
            match page.next_page_token {
                Some(token) => page_token = Some(token),
                None => break,
            }
        }
        let body = json!({ "reportTypeId": REACH_REPORT_TYPE, "name": REACH_JOB_NAME }).to_string();
        let response = self.authorized(|token| {
            self.agent
                .post(&url)
                .header("Authorization", bearer(token))
                .header("Content-Type", "application/json")
                .send(body.as_bytes())
        })?;
        let job: Job = http::json(response)?;
        Ok(job.id)
    }

    /// The reports of the reporting job `job_id` that cover days from `since` on,
    /// oldest day first. A day YouTube published again appears once per version.
    pub fn reach_reports(&self, job_id: &str, since: DateTime<Utc>) -> Result<Vec<ReachReport>> {
        let url = format!("{}/jobs/{job_id}/reports", self.config.reporting_base);
        let since = since.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let mut reports = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let mut query = vec![("startTimeAtOrAfter", since.as_str())];
            if let Some(token) = &page_token {
                query.push(("pageToken", token));
            }
            let page: ReportList = self.get_json(&url, &query)?;
            reports.extend(page.reports.into_iter().map(|report| ReachReport {
                id: report.id,
                start: report.start_time,
                end: report.end_time,
                created: report.create_time,
                download_url: report.download_url,
            }));
            match page.next_page_token {
                Some(token) => page_token = Some(token),
                None => break,
            }
        }
        reports.sort_by_key(|report| (report.start, report.created));
        Ok(reports)
    }

    /// The reach of `video_id` in `report`. A report without a row for the video
    /// means YouTube didn't show its thumbnail that day: no impressions.
    pub fn reach_of(&self, report: &ReachReport, video_id: &str) -> Result<DailyReach> {
        // The download needs the account's token, so it only goes to the Reporting API.
        if !is_within(&report.download_url, &self.config.reporting_base) {
            return Err(Error::Protocol(format!("unexpected report address {}", report.download_url)));
        }
        let response = self
            .authorized(|token| self.agent.get(&report.download_url).header("Authorization", bearer(token)).call())?;
        let csv = http::text(response)?;
        parse_reach(&csv, video_id, report.day())
    }
}

impl ReachReport {
    /// The Pacific-time day the report covers. It starts at midnight Pacific
    /// time, standard or daylight: noon after the start is that day either way.
    pub fn day(&self) -> NaiveDate {
        let pacific_standard = chrono::FixedOffset::west_opt(8 * 3600).expect("a valid offset");
        (self.start + chrono::TimeDelta::hours(12)).with_timezone(&pacific_standard).date_naive()
    }
}

/// Whether `url` is an address under `base` (`https://host/v1/…` under `https://host/v1`).
fn is_within(url: &str, base: &str) -> bool {
    url.strip_prefix(base.trim_end_matches('/')).is_some_and(|rest| rest.starts_with('/'))
}

fn parse_daily_views(report: &AnalyticsReport) -> Result<Vec<DailyViews>> {
    let column = |name: &str| {
        report
            .column_headers
            .iter()
            .position(|header| header.name == name)
            .ok_or_else(|| Error::Protocol(format!("the analytics report has no {name} column")))
    };
    let (day, views, minutes) = (column("day")?, column("views")?, column("estimatedMinutesWatched")?);
    report
        .rows
        .iter()
        .map(|row| {
            let date = row
                .get(day)
                .and_then(Value::as_str)
                .and_then(|text| NaiveDate::parse_from_str(text, "%Y-%m-%d").ok())
                .ok_or_else(|| Error::Protocol("an analytics row has no valid day".to_owned()))?;
            let number = |ix: usize| row.get(ix).and_then(Value::as_f64).unwrap_or(0.0).max(0.0);
            Ok(DailyViews { date, views: number(views).round() as u64, watch_minutes: number(minutes) })
        })
        .collect()
}

/// The row of `video_id` in a reach report CSV (`date,channel_id,video_id,
/// video_thumbnail_impressions,video_thumbnail_impressions_ctr`, in any order).
fn parse_reach(csv: &str, video_id: &str, day: NaiveDate) -> Result<DailyReach> {
    let mut lines = csv.lines().map(str::trim).filter(|line| !line.is_empty());
    let header: Vec<&str> = lines.next().unwrap_or_default().split(',').map(str::trim).collect();
    let column = |name: &str| {
        header
            .iter()
            .position(|column| *column == name)
            .ok_or_else(|| Error::Protocol(format!("the reach report has no {name} column")))
    };
    let (video, impressions, ctr) =
        (column("video_id")?, column("video_thumbnail_impressions")?, column("video_thumbnail_impressions_ctr")?);
    let date = column("date").ok();
    let mut reach = DailyReach { date: day, impressions: 0, click_rate: 0.0 };
    let mut clicks = 0.0;
    for line in lines {
        let fields: Vec<&str> = line.split(',').map(str::trim).collect();
        if fields.get(video) != Some(&video_id) {
            continue;
        }
        if let Some(date) =
            date.and_then(|ix| fields.get(ix)).and_then(|text| NaiveDate::parse_from_str(text, "%Y%m%d").ok())
        {
            reach.date = date;
        }
        let shown: u64 =
            fields.get(impressions).and_then(|text| text.parse::<f64>().ok()).unwrap_or(0.0).max(0.0) as u64;
        let mut rate = fields.get(ctr).and_then(|text| text.parse::<f64>().ok()).unwrap_or(0.0).max(0.0);
        // The documentation calls it a percentage and defines it as clicks over
        // impressions; above 1 it can only be a percentage.
        if rate > 1.0 {
            rate /= 100.0;
        }
        // A video can have one row per traffic detail; they add up.
        reach.impressions += shown;
        clicks += shown as f64 * rate.min(1.0);
    }
    if reach.impressions > 0 {
        reach.click_rate = clicks / reach.impressions as f64;
    }
    Ok(reach)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_analytics_rows_by_column_name() {
        let json = r#"{"kind":"youtubeAnalytics#resultTable","columnHeaders":[
            {"name":"day","columnType":"DIMENSION","dataType":"STRING"},
            {"name":"estimatedMinutesWatched","columnType":"METRIC","dataType":"INTEGER"},
            {"name":"views","columnType":"METRIC","dataType":"INTEGER"}],
            "rows":[["2026-10-01",120,45],["2026-10-02",80.5,30]]}"#;
        let report: AnalyticsReport = serde_json::from_str(json).unwrap();
        let days = parse_daily_views(&report).unwrap();
        assert_eq!(days.len(), 2);
        assert_eq!(
            days[0],
            DailyViews { date: NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(), views: 45, watch_minutes: 120.0 }
        );
        assert_eq!(days[1].watch_minutes, 80.5);

        // A video without views that week has no rows.
        let empty: AnalyticsReport = serde_json::from_str(
            r#"{"columnHeaders":[{"name":"day"},{"name":"views"},{"name":"estimatedMinutesWatched"}]}"#,
        )
        .unwrap();
        assert!(parse_daily_views(&empty).unwrap().is_empty());
        let odd: AnalyticsReport = serde_json::from_str(r#"{"columnHeaders":[{"name":"day"}],"rows":[]}"#).unwrap();
        assert!(matches!(parse_daily_views(&odd), Err(Error::Protocol(_))));
    }

    #[test]
    fn finds_the_video_in_a_reach_report() {
        let day = NaiveDate::from_ymd_opt(2026, 10, 3).unwrap();
        let csv = "date,channel_id,video_id,video_thumbnail_impressions,video_thumbnail_impressions_ctr\n\
                   20261003,UC1,other,500,0.1\n\
                   20261003,UC1,abc,1000,0.05\n";
        let reach = parse_reach(csv, "abc", day).unwrap();
        assert_eq!(reach.date, day);
        assert_eq!(reach.impressions, 1000);
        assert!((reach.click_rate - 0.05).abs() < 1e-9);

        // No row: YouTube didn't show the thumbnail that day.
        let none = parse_reach(csv, "missing", day).unwrap();
        assert_eq!((none.impressions, none.click_rate), (0, 0.0));

        // Percentages, columns in another order, and rows that add up.
        let csv = "video_id,video_thumbnail_impressions_ctr,date,channel_id,video_thumbnail_impressions\n\
                   abc,4,20261003,UC1,100\nabc,6,20261003,UC1,100\n";
        let reach = parse_reach(csv, "abc", day).unwrap();
        assert_eq!(reach.impressions, 200);
        assert!((reach.click_rate - 0.05).abs() < 1e-9);

        assert!(matches!(parse_reach("date,video_id\n", "abc", day), Err(Error::Protocol(_))));
    }

    #[test]
    fn reports_download_only_from_the_reporting_api() {
        let base = "https://youtubereporting.googleapis.com/v1";
        assert!(is_within("https://youtubereporting.googleapis.com/v1/media/CHANNEL/x?alt=media", base));
        assert!(!is_within("https://youtubereporting.googleapis.com/v10/media", base));
        assert!(!is_within("https://youtubereporting.googleapis.com.evil.example/v1/media", base));
        assert!(!is_within("https://example.com/v1/media", base));
    }

    #[test]
    fn a_report_covers_the_pacific_day_it_starts() {
        let at = |text: &str| DateTime::parse_from_rfc3339(text).unwrap().with_timezone(&Utc);
        // Midnight PDT and midnight PST.
        let summer = ReachReport::new(
            "1",
            at("2026-07-01T07:00:00Z"),
            at("2026-07-02T07:00:00Z"),
            at("2026-07-03T00:00:00Z"),
            "",
        );
        assert_eq!(summer.day(), NaiveDate::from_ymd_opt(2026, 7, 1).unwrap());
        let winter = ReachReport::new(
            "2",
            at("2026-12-01T08:00:00Z"),
            at("2026-12-02T08:00:00Z"),
            at("2026-12-03T00:00:00Z"),
            "",
        );
        assert_eq!(winter.day(), NaiveDate::from_ymd_opt(2026, 12, 1).unwrap());
    }
}
