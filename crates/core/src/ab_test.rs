//! A/B tests of a video's title and thumbnail on YouTube.
//!
//! YouTube Studio's own "Test & compare" has no API, so Rushes runs the test
//! itself: it puts one version on the video for a day at a time, switching at
//! midnight Pacific time (when YouTube's daily numbers turn over), and records
//! when each version went up. YouTube reports each day's thumbnail impressions,
//! clicks, views and watch time a day or two later; every day's numbers are
//! shared between the versions by how long each was up that day. The version that
//! earns the most watch time per impression wins, the measure closest to YouTube's
//! own "most watch time" when every version gets the same audience.
//!
//! Plain data and arithmetic: the app does the YouTube calls and keeps the test on
//! the video's [`YoutubeLink`](crate::YoutubeLink).

use std::path::PathBuf;

use chrono::{DateTime, Datelike as _, FixedOffset, NaiveDate, TimeDelta, Utc, Weekday};
use serde::{Deserialize, Serialize};

/// Versions a test compares, at least and at most.
pub const MIN_VERSIONS: usize = 2;
pub const MAX_VERSIONS: usize = 3;
/// Test lengths the creator picks from, in days.
pub const TEST_LENGTHS: [u32; 3] = [7, 14, 28];
pub const DEFAULT_TEST_DAYS: u32 = 14;
/// After its last day, a test waits this long for YouTube's last numbers before it
/// decides with what it has.
pub const FINISH_WAIT: TimeDelta = TimeDelta::days(4);
/// A version needs this much measured time and this many impressions before the
/// numbers can name a winner.
pub const MIN_MEASURED_DAYS: f64 = 2.0;
pub const MIN_IMPRESSIONS: f64 = 100.0;

/// What a test compares.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestSubject {
    Title,
    Thumbnail,
    /// A title and a thumbnail that go up together.
    Both,
}

impl TestSubject {
    pub const ALL: [TestSubject; 3] = [TestSubject::Title, TestSubject::Thumbnail, TestSubject::Both];

    pub fn label(self) -> &'static str {
        match self {
            TestSubject::Title => "Title",
            TestSubject::Thumbnail => "Thumbnail",
            TestSubject::Both => "Title and thumbnail",
        }
    }

    pub fn has_title(self) -> bool {
        matches!(self, TestSubject::Title | TestSubject::Both)
    }

    pub fn has_thumbnail(self) -> bool {
        matches!(self, TestSubject::Thumbnail | TestSubject::Both)
    }
}

/// One version a test puts on YouTube: a title, a thumbnail image, or both,
/// as the test's subject says.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TestVersion {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<PathBuf>,
}

/// From `from` on, YouTube showed the version at `version` (until the next showing).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Showing {
    pub version: usize,
    pub from: DateTime<Utc>,
}

/// What YouTube reported about the video for one Pacific-time day.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DayNumbers {
    pub date: NaiveDate,
    pub views: u64,
    pub watch_minutes: f64,
    /// Whether YouTube Analytics has reported the day's views and watch time. Until
    /// then they aren't zeros but unknown, and the day doesn't count.
    #[serde(default)]
    pub views_reported: bool,
    /// Thumbnail impressions, once the day's reach report is in. Until then the day
    /// doesn't count.
    pub impressions: Option<u64>,
    /// Impressions that were clicked.
    pub clicks: f64,
    /// When YouTube made the reach report the impressions come from; a newer report
    /// of the day replaces them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reach_report: Option<DateTime<Utc>>,
}

impl DayNumbers {
    fn new(date: NaiveDate) -> Self {
        Self {
            date,
            views: 0,
            watch_minutes: 0.0,
            views_reported: false,
            impressions: None,
            clicks: 0.0,
            reach_report: None,
        }
    }

    /// Whether both YouTube Analytics and the reach report have the day: only then
    /// does it count.
    pub fn is_measured(&self) -> bool {
        self.views_reported && self.impressions.is_some()
    }
}

/// How a test ended.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TestOutcome {
    /// The test ran to its end, or the creator ended it: `kept` stays on YouTube.
    Finished {
        at: DateTime<Utc>,
        kept: usize,
        /// The version the numbers clearly favoured, if any.
        winner: Option<usize>,
        /// The creator chose `kept`; otherwise the numbers did.
        chosen: bool,
    },
    /// The test stopped early and left YouTube as it was.
    Stopped { at: DateTime<Utc>, reason: StopReason },
}

impl TestOutcome {
    pub fn at(&self) -> DateTime<Utc> {
        match self {
            TestOutcome::Finished { at, .. } | TestOutcome::Stopped { at, .. } => *at,
        }
    }
}

/// Why a test stopped early.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum StopReason {
    /// Someone changed the title on YouTube (in YouTube Studio, say): Rushes
    /// doesn't overwrite it.
    TitleChanged { title: String },
    /// The creator stopped it.
    Cancelled,
    /// The video stopped being public, or scheduled to be: YouTube shows it to no one
    /// the test could measure.
    NotPublic,
}

/// What the test should do next, at some moment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestStep {
    /// Put this version on YouTube: its day has come.
    Show(usize),
    /// The test is over: decide, keep the winner and finish.
    Finish,
    /// Nothing to do now.
    Wait,
}

/// One version's share of the numbers so far.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VersionResult {
    /// How long YouTube showed it during the test, in days.
    pub days_shown: f64,
    /// Of those, the days YouTube reported numbers for.
    pub days_measured: f64,
    pub impressions: f64,
    pub clicks: f64,
    pub views: f64,
    pub watch_minutes: f64,
}

impl VersionResult {
    /// Clicks per impression, once there are impressions.
    pub fn click_rate(&self) -> Option<f64> {
        (self.impressions > 0.0).then(|| self.clicks / self.impressions)
    }

    /// Seconds watched per impression: what the test compares.
    pub fn score(&self) -> Option<f64> {
        (self.impressions > 0.0).then(|| self.watch_minutes * 60.0 / self.impressions)
    }

    fn has_enough(&self) -> bool {
        self.days_measured >= MIN_MEASURED_DAYS - 1e-9 && self.impressions >= MIN_IMPRESSIONS
    }
}

/// What the numbers say so far.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Verdict {
    /// The version with the most watch time per impression, once every version has
    /// enough numbers to compare.
    pub leader: Option<usize>,
    /// How much more watch time per impression the leader earns than the next best,
    /// as a fraction (0.2 is 20 % more).
    pub lift: f64,
    /// Whether the lead is larger than chance would explain (95 % confidence).
    pub clear: bool,
}

/// An A/B test of a video on YouTube.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AbTest {
    pub subject: TestSubject,
    /// [`MIN_VERSIONS`] to [`MAX_VERSIONS`] versions, A first.
    pub versions: Vec<TestVersion>,
    /// How long the test runs, in days.
    pub days: u32,
    pub created_at: DateTime<Utc>,
    /// When the versions start taking turns: at once, or when a scheduled video
    /// goes public. A is up from the start of the test.
    pub starts_at: DateTime<Utc>,
    /// The video's title on YouTube when the test started, which putting A up replaces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_title: Option<String>,
    /// Which version YouTube showed from when, oldest first. Empty until A is up.
    #[serde(default)]
    pub showings: Vec<Showing>,
    /// YouTube's numbers, by day, oldest first.
    #[serde(default)]
    pub numbers: Vec<DayNumbers>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<TestOutcome>,
    /// The version whose title YouTube has when a switch of a title-and-thumbnail
    /// test put up its title but not its thumbnail, and couldn't take the title back.
    /// YouTube then shows neither version whole, and the next switch fixes that.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_up: Option<usize>,
}

impl AbTest {
    /// A test of `versions` for `days` days from `now`, or from `publish_at` when the
    /// video goes public later.
    pub fn new(
        subject: TestSubject,
        versions: Vec<TestVersion>,
        days: u32,
        original_title: Option<String>,
        now: DateTime<Utc>,
        publish_at: Option<DateTime<Utc>>,
    ) -> Self {
        Self {
            subject,
            versions,
            days,
            created_at: now,
            starts_at: publish_at.filter(|at| *at > now).unwrap_or(now),
            original_title,
            showings: Vec::new(),
            numbers: Vec::new(),
            outcome: None,
            title_up: None,
        }
    }

    /// Follows the video's schedule: the versions take turns once it is public. A
    /// video that goes public at `publish_at`, later, starts them then, also when
    /// they were due earlier; one that is public (`None`) starts them at once, unless
    /// they started already. Whether the start moved.
    pub fn follow_schedule(&mut self, publish_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
        if self.is_over() {
            return false;
        }
        let starts_at = match publish_at.filter(|at| *at > now) {
            Some(at) => at,
            None if self.starts_at > now => now,
            None => return false,
        };
        let moved = starts_at != self.starts_at;
        self.starts_at = starts_at;
        moved
    }

    /// When the last version's turn ends.
    pub fn ends_at(&self) -> DateTime<Utc> {
        self.starts_at + TimeDelta::days(i64::from(self.days))
    }

    /// When the versions stopped taking turns: the end, or when the test ended
    /// or stopped before it.
    fn turns_end(&self) -> DateTime<Utc> {
        self.outcome.as_ref().map_or(self.ends_at(), |outcome| outcome.at().min(self.ends_at()))
    }

    pub fn is_over(&self) -> bool {
        self.outcome.is_some()
    }

    /// The version on YouTube now, as far as Rushes put it there.
    pub fn live_version(&self) -> Option<usize> {
        self.showings.last().map(|showing| showing.version)
    }

    /// The title YouTube has, as far as Rushes knows: the live version's (or the one
    /// a half-done switch left, see [`Self::title_up`]), else the one the video had
    /// before the test.
    pub fn live_title(&self) -> Option<&str> {
        self.title_up
            .or(self.live_version())
            .and_then(|version| self.versions.get(version))
            .and_then(|version| version.title.as_deref())
            .or(self.original_title.as_deref())
    }

    /// Which version's turn it is at `at`: A until the start, then one per Pacific
    /// day, each in turn and then back in reverse (A B B A A B …, A B C C B A …),
    /// so a version is never always the one on the busy first days.
    pub fn version_at(&self, at: DateTime<Utc>) -> usize {
        if at < self.starts_at || self.versions.is_empty() {
            return 0;
        }
        let turn = (pacific_day(at) - pacific_day(self.starts_at)).num_days().max(0) as usize;
        let count = self.versions.len();
        let (round, place) = (turn / count, turn % count);
        if round % 2 == 0 { place } else { count - 1 - place }
    }

    /// What to do at `now`.
    pub fn step(&self, now: DateTime<Utc>) -> TestStep {
        if self.is_over() {
            return TestStep::Wait;
        }
        if now >= self.ends_at() {
            let complete = self.last_day().is_some_and(|last| self.measured_through().is_some_and(|day| day >= last));
            return if complete || now >= self.ends_at() + FINISH_WAIT { TestStep::Finish } else { TestStep::Wait };
        }
        let due = self.version_at(now);
        // A half-done switch shows no version whole: the due one goes up again.
        if self.live_version() == Some(due) && self.title_up.is_none() { TestStep::Wait } else { TestStep::Show(due) }
    }

    /// The Pacific days the test covers so far at `now`, first and last; `None`
    /// before it starts.
    pub fn days_so_far(&self, now: DateTime<Utc>) -> Option<(NaiveDate, NaiveDate)> {
        let last = now.min(self.turns_end());
        (last > self.starts_at).then(|| (pacific_day(self.starts_at), pacific_day(last - TimeDelta::seconds(1))))
    }

    /// The test's last Pacific day.
    fn last_day(&self) -> Option<NaiveDate> {
        (self.turns_end() > self.starts_at).then(|| pacific_day(self.turns_end() - TimeDelta::seconds(1)))
    }

    /// The last day up to which every day of the test has all its numbers.
    pub fn measured_through(&self) -> Option<NaiveDate> {
        let first = pacific_day(self.starts_at);
        let mut through = None;
        let mut day = first;
        loop {
            match self.numbers.iter().find(|numbers| numbers.date == day) {
                Some(numbers) if numbers.is_measured() => through = Some(day),
                _ => return through,
            }
            day = day.succ_opt()?;
        }
    }

    /// Records Analytics' views and watch time of `date`.
    pub fn set_views(&mut self, date: NaiveDate, views: u64, watch_minutes: f64) {
        let day = self.day_mut(date);
        day.views = views;
        day.watch_minutes = watch_minutes.max(0.0);
        day.views_reported = true;
    }

    /// Records an answer of YouTube Analytics: `days` (date, views, watch minutes),
    /// one per day it has numbers for. It leaves out days without views, but also
    /// the days it hasn't counted yet: a test day before the last one it answered
    /// for had no views, one after it is still unknown.
    pub fn record_views(&mut self, days: &[(NaiveDate, u64, f64)]) {
        for (date, views, minutes) in days {
            self.set_views(*date, *views, *minutes);
        }
        let Some(latest) = days.iter().map(|(date, ..)| *date).max() else { return };
        let mut date = pacific_day(self.starts_at);
        while date < latest {
            if !self.numbers.iter().any(|day| day.date == date && day.views_reported) {
                self.set_views(date, 0, 0.0);
            }
            let Some(next) = date.succ_opt() else { break };
            date = next;
        }
    }

    /// When the reach report the impressions of `date` come from was made.
    pub fn reach_report(&self, date: NaiveDate) -> Option<DateTime<Utc>> {
        self.numbers.iter().find(|numbers| numbers.date == date).and_then(|numbers| numbers.reach_report)
    }

    /// Records the impressions and click rate of `date` from the reach report made at
    /// `report`. An older report than the one recorded changes nothing; `false` then.
    pub fn set_reach(&mut self, date: NaiveDate, impressions: u64, click_rate: f64, report: DateTime<Utc>) -> bool {
        if self.reach_report(date).is_some_and(|recorded| recorded >= report) {
            return false;
        }
        let day = self.day_mut(date);
        day.impressions = Some(impressions);
        day.clicks = impressions as f64 * click_rate.clamp(0.0, 1.0);
        day.reach_report = Some(report);
        true
    }

    fn day_mut(&mut self, date: NaiveDate) -> &mut DayNumbers {
        let ix = match self.numbers.binary_search_by_key(&date, |numbers| numbers.date) {
            Ok(ix) => ix,
            Err(ix) => {
                self.numbers.insert(ix, DayNumbers::new(date));
                ix
            }
        };
        &mut self.numbers[ix]
    }

    /// How long each version was up within `from..until`, in seconds, counting
    /// only the test's own time (from its start until its turns ended).
    fn shown_within(&self, from: DateTime<Utc>, until: DateTime<Utc>) -> Vec<f64> {
        let mut shown = vec![0.0; self.versions.len()];
        let (start, end) = (from.max(self.starts_at), until.min(self.turns_end()));
        for (ix, showing) in self.showings.iter().enumerate() {
            let next = self.showings.get(ix + 1).map_or(DateTime::<Utc>::MAX_UTC, |next| next.from);
            let (a, b) = (showing.from.max(start), next.min(end));
            if b > a
                && let Some(seconds) = shown.get_mut(showing.version)
            {
                *seconds += (b - a).num_milliseconds() as f64 / 1000.0;
            }
        }
        shown
    }

    /// Each version's numbers so far at `now`.
    pub fn results(&self, now: DateTime<Utc>) -> Vec<VersionResult> {
        let mut results = vec![VersionResult::default(); self.versions.len()];
        for (result, seconds) in results.iter_mut().zip(self.shown_within(self.starts_at, now)) {
            result.days_shown = seconds / 86_400.0;
        }
        for (day, shares) in self.measured_shares() {
            let impressions = day.impressions.unwrap_or(0) as f64;
            for (result, share) in results.iter_mut().zip(shares) {
                result.days_measured += share;
                result.impressions += share * impressions;
                result.clicks += share * day.clicks;
                result.views += share * day.views as f64;
                result.watch_minutes += share * day.watch_minutes;
            }
        }
        results
    }

    /// The days with all their numbers, each with every version's share of it: the
    /// part of the day it was up.
    fn measured_shares(&self) -> impl Iterator<Item = (&DayNumbers, Vec<f64>)> {
        self.numbers.iter().filter(|day| day.is_measured()).map(|day| {
            let (start, end) = (pacific_midnight(day.date), pacific_midnight(day.date + TimeDelta::days(1)));
            let length = (end - start).num_milliseconds() as f64 / 1000.0;
            (day, self.shown_within(start, end).into_iter().map(|seconds| seconds / length).collect())
        })
    }

    /// What the numbers say at `now`.
    pub fn verdict(&self, now: DateTime<Utc>) -> Verdict {
        let results = self.results(now);
        let none = Verdict { leader: None, lift: 0.0, clear: false };
        if results.len() < MIN_VERSIONS || !results.iter().all(VersionResult::has_enough) {
            return none;
        }
        let scores: Vec<f64> = results.iter().map(|result| result.score().unwrap_or(0.0)).collect();
        let mut order: Vec<usize> = (0..scores.len()).collect();
        // The earlier version wins a tie.
        order.sort_by(|a, b| scores[*b].total_cmp(&scores[*a]).then(a.cmp(b)));
        let (leader, runner_up) = (order[0], order[1]);
        if scores[leader] <= 0.0 {
            return none;
        }
        let lift = if scores[runner_up] > 0.0 { scores[leader] / scores[runner_up] - 1.0 } else { f64::INFINITY };
        let spreads = self.score_spreads();
        // 95 % for the one comparison, Bonferroni-corrected for two.
        let alpha = if scores.len() > 2 { 0.025 } else { 0.05 };
        let clear = order[1..].iter().all(|other| {
            let difference = scores[leader] - scores[*other];
            difference > 0.0 && welch_p_value(difference, spreads[leader], spreads[*other]) < alpha
        });
        Verdict { leader: Some(leader), lift, clear }
    }

    /// How uncertain each version's score (seconds per impression) is, from how much
    /// it varies from day to day: the ratio estimator's variance, with days as
    /// samples, and how many days that is.
    fn score_spreads(&self) -> Vec<(f64, f64)> {
        let count = self.versions.len();
        let mut samples: Vec<Vec<(f64, f64)>> = vec![Vec::new(); count];
        for (day, shares) in self.measured_shares() {
            let impressions = day.impressions.unwrap_or(0) as f64;
            for (version, share) in shares.into_iter().enumerate() {
                if share > 0.0 {
                    samples[version].push((share * day.watch_minutes * 60.0, share * impressions));
                }
            }
        }
        samples
            .into_iter()
            .map(|days| {
                let n = days.len() as f64;
                let (seconds, impressions) =
                    days.iter().fold((0.0, 0.0), |(s, i), (seconds, impressions)| (s + seconds, i + impressions));
                if n < 2.0 || impressions <= 0.0 {
                    return (f64::INFINITY, n);
                }
                let ratio = seconds / impressions;
                let residuals: f64 = days.iter().map(|(s, i)| (s - ratio * i).powi(2)).sum();
                (n / (n - 1.0) * residuals / impressions.powi(2), n)
            })
            .collect()
    }

    /// The version a test that runs out keeps: the clear winner, else A.
    pub fn version_to_keep(&self, now: DateTime<Utc>) -> (usize, Option<usize>) {
        let verdict = self.verdict(now);
        match verdict.leader.filter(|_| verdict.clear) {
            Some(winner) => (winner, Some(winner)),
            None => (0, None),
        }
    }
}

/// The two-sided p-value of Welch's t-test of `difference` between two scores,
/// each given as its variance and the number of days it comes from. Few days make
/// the variances uncertain too, which the degrees of freedom account for.
fn welch_p_value(difference: f64, (a, days_a): (f64, f64), (b, days_b): (f64, f64)) -> f64 {
    let spread = (a + b).sqrt();
    if !spread.is_finite() {
        return 1.0;
    }
    if spread <= 0.0 {
        return if difference > 0.0 { 0.0 } else { 1.0 };
    }
    let t = difference.abs() / spread;
    let freedom = (a + b).powi(2) / (a * a / (days_a - 1.0) + b * b / (days_b - 1.0));
    // The upper tail of Student's t, twice.
    regularized_incomplete_beta(freedom / 2.0, 0.5, freedom / (freedom + t * t))
}

/// I_x(a, b), by its continued fraction (Numerical Recipes, `betai`).
fn regularized_incomplete_beta(a: f64, b: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let front = (ln_gamma(a + b) - ln_gamma(a) - ln_gamma(b) + a * x.ln() + b * (1.0 - x).ln()).exp();
    if x < (a + 1.0) / (a + b + 2.0) {
        front * beta_fraction(a, b, x) / a
    } else {
        1.0 - front * beta_fraction(b, a, 1.0 - x) / b
    }
}

/// The continued fraction of the incomplete beta function, by Lentz's method.
fn beta_fraction(a: f64, b: f64, x: f64) -> f64 {
    const TINY: f64 = 1e-300;
    let nonzero = |value: f64| if value.abs() < TINY { TINY } else { value };
    let mut c = 1.0;
    let mut d = 1.0 / nonzero(1.0 - (a + b) * x / (a + 1.0));
    let mut h = d;
    for m in 1..300 {
        let m = f64::from(m);
        let even = m * (b - m) * x / ((a + 2.0 * m - 1.0) * (a + 2.0 * m));
        d = 1.0 / nonzero(1.0 + even * d);
        c = nonzero(1.0 + even / c);
        h *= d * c;
        let odd = -(a + m) * (a + b + m) * x / ((a + 2.0 * m) * (a + 2.0 * m + 1.0));
        d = 1.0 / nonzero(1.0 + odd * d);
        c = nonzero(1.0 + odd / c);
        let step = d * c;
        h *= step;
        if (step - 1.0).abs() < 1e-12 {
            break;
        }
    }
    h
}

/// ln Γ(x) for x > 0, by Lanczos' approximation (g = 7).
fn ln_gamma(x: f64) -> f64 {
    const COEFFICIENTS: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.520_368_121_885_1,
        -1_259.139_216_722_402_8,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_12,
        9.984_369_578_019_572e-6,
        1.505_632_735_149_311_6e-7,
    ];
    if x < 0.5 {
        let pi = std::f64::consts::PI;
        return (pi / (pi * x).sin()).ln() - ln_gamma(1.0 - x);
    }
    let x = x - 1.0;
    let t = x + 7.5;
    let sum = COEFFICIENTS[1..]
        .iter()
        .enumerate()
        .fold(COEFFICIENTS[0], |sum, (ix, coefficient)| sum + coefficient / (x + ix as f64 + 1.0));
    0.5 * (2.0 * std::f64::consts::PI).ln() + (x + 0.5) * t.ln() - t + sum.ln()
}

/// Letter of the version at `ix`: A, B, C.
pub fn version_letter(ix: usize) -> char {
    char::from(b'A' + (ix.min(25) as u8))
}

/// Pacific time's offset from UTC at `at`: daylight time (UTC−7) from 2:00 on the
/// second Sunday of March to 2:00 on the first Sunday of November, standard time
/// (UTC−8) otherwise.
pub fn pacific_offset(at: DateTime<Utc>) -> FixedOffset {
    let year = at.year();
    let sunday = |month, nth| NaiveDate::from_weekday_of_month_opt(year, month, Weekday::Sun, nth);
    // 2:00 PST and 2:00 PDT, in UTC.
    let daylight = match (sunday(3, 2), sunday(11, 1)) {
        (Some(start), Some(end)) => {
            let start = start.and_hms_opt(10, 0, 0).map(|time| time.and_utc());
            let end = end.and_hms_opt(9, 0, 0).map(|time| time.and_utc());
            start.is_some_and(|start| at >= start) && end.is_some_and(|end| at < end)
        }
        _ => false,
    };
    FixedOffset::west_opt(if daylight { 7 * 3600 } else { 8 * 3600 }).expect("a valid offset")
}

/// The Pacific-time day `at` falls on: YouTube's daily numbers count these days.
pub fn pacific_day(at: DateTime<Utc>) -> NaiveDate {
    at.with_timezone(&pacific_offset(at)).date_naive()
}

/// Midnight Pacific time at the start of `date`.
pub fn pacific_midnight(date: NaiveDate) -> DateTime<Utc> {
    let daylight = date.and_hms_opt(7, 0, 0).expect("a valid time").and_utc();
    if pacific_offset(daylight).local_minus_utc() == -7 * 3600 {
        daylight
    } else {
        date.and_hms_opt(8, 0, 0).expect("a valid time").and_utc()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text).unwrap().with_timezone(&Utc)
    }

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap()
    }

    fn titles(count: usize) -> Vec<TestVersion> {
        (0..count).map(|ix| TestVersion { title: Some(format!("Title {ix}")), thumbnail: None }).collect()
    }

    /// A two-version title test that started at midnight Pacific on 1 Oct 2026.
    fn october_test(count: usize, days: u32) -> AbTest {
        let start = pacific_midnight(date("2026-10-01"));
        AbTest::new(TestSubject::Title, titles(count), days, Some("Before".into()), start, None)
    }

    #[test]
    fn pacific_days_follow_daylight_saving_time() {
        assert_eq!(pacific_midnight(date("2026-07-01")), at("2026-07-01T07:00:00Z"));
        assert_eq!(pacific_midnight(date("2026-12-01")), at("2026-12-01T08:00:00Z"));
        // The switch days themselves start on the time before the switch.
        assert_eq!(pacific_midnight(date("2026-03-08")), at("2026-03-08T08:00:00Z"));
        assert_eq!(pacific_midnight(date("2026-03-09")), at("2026-03-09T07:00:00Z"));
        assert_eq!(pacific_midnight(date("2026-11-01")), at("2026-11-01T07:00:00Z"));
        assert_eq!(pacific_midnight(date("2026-11-02")), at("2026-11-02T08:00:00Z"));
        assert_eq!(pacific_day(at("2026-10-02T06:59:59Z")), date("2026-10-01"));
        assert_eq!(pacific_day(at("2026-10-02T07:00:00Z")), date("2026-10-02"));
        assert_eq!(pacific_day(at("2026-12-02T07:59:59Z")), date("2026-12-01"));
        assert_eq!(version_letter(2), 'C');
    }

    #[test]
    fn versions_take_turns_by_pacific_day() {
        let test = october_test(2, 14);
        let day = |n: i64| pacific_midnight(date("2026-10-01") + TimeDelta::days(n)) + TimeDelta::hours(3);
        let turns: String = (0..8).map(|n| version_letter(test.version_at(day(n)))).collect();
        assert_eq!(turns, "ABBAABBA");
        let three = october_test(3, 14);
        let turns: String = (0..9).map(|n| version_letter(three.version_at(day(n)))).collect();
        assert_eq!(turns, "ABCCBAABC");
        // Before a scheduled video goes public, A waits.
        let publish = at("2026-10-05T16:00:00Z");
        let scheduled = AbTest::new(TestSubject::Title, titles(2), 7, None, at("2026-10-01T12:00:00Z"), Some(publish));
        assert_eq!(scheduled.starts_at, publish);
        assert_eq!(scheduled.version_at(at("2026-10-03T12:00:00Z")), 0);
        assert_eq!(scheduled.ends_at(), at("2026-10-12T16:00:00Z"));
    }

    #[test]
    fn a_scheduled_test_follows_the_publish_time() {
        let now = at("2026-10-18T12:00:00Z");
        let tomorrow = at("2026-10-19T16:00:00Z");
        let mut test = AbTest::new(TestSubject::Title, titles(2), 7, None, now, Some(tomorrow));
        assert_eq!((test.starts_at, test.ends_at()), (tomorrow, at("2026-10-26T16:00:00Z")));
        // Postponed by two weeks: the turns wait for it.
        let later = at("2026-11-02T16:00:00Z");
        assert!(test.follow_schedule(Some(later), now));
        assert_eq!((test.starts_at, test.ends_at()), (later, at("2026-11-09T16:00:00Z")));
        assert!(!test.follow_schedule(Some(later), now));
        // Published early: the turns start at once.
        let early = at("2026-10-20T09:00:00Z");
        assert!(test.follow_schedule(None, early));
        assert_eq!(test.starts_at, early);
        // Public and started: nothing to move.
        assert!(!test.follow_schedule(None, early + TimeDelta::hours(1)));
        assert_eq!(test.starts_at, early);
        // Postponed past a start that came while YouTube didn't hear of it yet: the
        // video isn't public, so the turns wait again.
        assert!(test.follow_schedule(Some(later), early + TimeDelta::hours(2)));
        assert_eq!(test.starts_at, later);
    }

    #[test]
    fn steps_put_up_the_due_version_then_finish() {
        let mut test = october_test(2, 2);
        let start = test.starts_at;
        assert_eq!(test.step(start), TestStep::Show(0));
        test.showings.push(Showing { version: 0, from: start });
        assert_eq!(test.step(start + TimeDelta::hours(5)), TestStep::Wait);
        let tomorrow = pacific_midnight(date("2026-10-02")) + TimeDelta::minutes(1);
        assert_eq!(test.step(tomorrow), TestStep::Show(1));
        test.showings.push(Showing { version: 1, from: tomorrow });
        // Over, but YouTube hasn't reported the last day yet: wait, then decide anyway.
        let end = test.ends_at();
        assert_eq!(test.step(end), TestStep::Wait);
        assert_eq!(test.step(end + FINISH_WAIT), TestStep::Finish);
        test.set_reach(date("2026-10-01"), 100, 0.05, end);
        test.record_views(&[(date("2026-10-01"), 5, 20.0)]);
        assert_eq!(test.measured_through(), Some(date("2026-10-01")));
        test.set_reach(date("2026-10-02"), 100, 0.05, end);
        test.record_views(&[(date("2026-10-02"), 5, 20.0)]);
        assert_eq!(test.step(end + TimeDelta::hours(1)), TestStep::Finish);
        test.outcome = Some(TestOutcome::Finished { at: end, kept: 0, winner: None, chosen: false });
        assert_eq!(test.step(end + FINISH_WAIT), TestStep::Wait);
    }

    #[test]
    fn days_are_shared_by_how_long_each_version_was_up() {
        let mut test = october_test(2, 14);
        let start = test.starts_at;
        test.showings.push(Showing { version: 0, from: start });
        // Rushes was closed at midnight and put B up at 6:00 the next morning.
        let late = pacific_midnight(date("2026-10-02")) + TimeDelta::hours(6);
        test.showings.push(Showing { version: 1, from: late });
        test.set_views(date("2026-10-01"), 100, 400.0);
        test.set_reach(date("2026-10-01"), 1000, 0.05, start + TimeDelta::days(2));
        test.set_views(date("2026-10-02"), 200, 800.0);
        test.set_reach(date("2026-10-02"), 2000, 0.04, start + TimeDelta::days(3));

        let now = pacific_midnight(date("2026-10-03"));
        let [a, b] = test.results(now).try_into().unwrap();
        assert!((a.days_shown - 1.25).abs() < 1e-9 && (b.days_shown - 0.75).abs() < 1e-9);
        assert!((a.impressions - (1000.0 + 500.0)).abs() < 1e-6);
        assert!((b.impressions - 1500.0).abs() < 1e-6);
        assert!((a.clicks - (50.0 + 20.0)).abs() < 1e-6);
        assert!((a.views - 150.0).abs() < 1e-6 && (b.watch_minutes - 600.0).abs() < 1e-6);
        assert!((a.click_rate().unwrap() - 70.0 / 1500.0).abs() < 1e-9);
        assert!((b.score().unwrap() - 600.0 * 60.0 / 1500.0).abs() < 1e-9);
        assert_eq!(a.days_measured + b.days_measured, 2.0);

        // A newer report of a day replaces it; an older one doesn't.
        assert!(!test.set_reach(date("2026-10-01"), 5, 0.5, start));
        assert!(test.set_reach(date("2026-10-01"), 1200, 0.05, start + TimeDelta::days(4)));
        assert_eq!(test.numbers[0].impressions, Some(1200));
    }

    /// `days` of numbers where B earns `lift` more watch time per impression, with
    /// some day-to-day noise.
    fn measured(count: usize, days: i64, scores: &[f64]) -> AbTest {
        let mut test = october_test(count, days as u32);
        for n in 0..days {
            let day = date("2026-10-01") + TimeDelta::days(n);
            let version = test.version_at(pacific_midnight(day));
            test.showings.push(Showing { version, from: pacific_midnight(day) });
            let impressions = 1000 + (n as u64 * 137) % 400;
            let noise = 1.0 + ((n * 7919) % 11 - 5) as f64 / 100.0;
            let minutes = impressions as f64 * scores[version] * noise / 60.0;
            test.set_views(day, impressions / 20, minutes);
            test.set_reach(day, impressions, 0.05, pacific_midnight(day) + TimeDelta::days(2));
        }
        test
    }

    #[test]
    fn a_clear_lead_names_the_winner() {
        let test = measured(2, 14, &[10.0, 12.0]);
        let now = test.ends_at();
        let verdict = test.verdict(now);
        assert_eq!(verdict.leader, Some(1));
        assert!((verdict.lift - 0.2).abs() < 0.03, "{}", verdict.lift);
        assert!(verdict.clear);
        assert_eq!(test.version_to_keep(now), (1, Some(1)));

        let three = measured(3, 14, &[10.0, 10.2, 13.0]);
        assert_eq!(three.verdict(three.ends_at()).leader, Some(2));
        assert!(three.verdict(three.ends_at()).clear);
    }

    #[test]
    fn a_small_lead_or_few_numbers_keep_a() {
        let close = measured(2, 14, &[10.0, 10.1]);
        let verdict = close.verdict(close.ends_at());
        assert_eq!(verdict.leader, Some(1));
        assert!(!verdict.clear);
        assert_eq!(close.version_to_keep(close.ends_at()), (0, None));

        // One day each isn't enough to compare.
        let short = measured(2, 2, &[10.0, 20.0]);
        assert_eq!(short.verdict(short.ends_at()).leader, None);
        assert_eq!(short.version_to_keep(short.ends_at()), (0, None));
    }

    #[test]
    fn welch_p_values_match_the_t_table() {
        // Two-sided 5 % points of Student's t: 2.571 (5 df), 2.228 (10), 1.960 (∞).
        let p = |t: f64, freedom: f64| regularized_incomplete_beta(freedom / 2.0, 0.5, freedom / (freedom + t * t));
        assert!((p(2.571, 5.0) - 0.05).abs() < 1e-3, "{}", p(2.571, 5.0));
        assert!((p(2.228, 10.0) - 0.05).abs() < 1e-3);
        assert!((p(1.96, 1e6) - 0.05).abs() < 1e-3);
        assert!((p(0.0, 7.0) - 1.0).abs() < 1e-12);
        assert!((ln_gamma(5.0) - 24f64.ln()).abs() < 1e-10);
        assert!((ln_gamma(0.5) - std::f64::consts::PI.sqrt().ln()).abs() < 1e-10);
    }

    #[test]
    fn a_week_of_days_needs_more_than_a_normal_lead() {
        // A B B A A B B, 1,000 impressions a day: A earns 10, 12 and 11 s per
        // impression, B 12, 14, 12 and 14. The lead is 2.45 standard errors, short of
        // what 4.8 degrees of freedom need at 95 % (2.6).
        let mut test = october_test(2, 7);
        let scores = [10.0, 12.0, 14.0, 11.0, 12.0, 12.0, 14.0];
        for (n, score) in scores.iter().enumerate() {
            let day = date("2026-10-01") + TimeDelta::days(n as i64);
            let version = test.version_at(pacific_midnight(day));
            test.showings.push(Showing { version, from: pacific_midnight(day) });
            test.set_views(day, 50, 1000.0 * score / 60.0);
            test.set_reach(day, 1000, 0.05, pacific_midnight(day) + TimeDelta::days(2));
        }
        let verdict = test.verdict(test.ends_at());
        assert_eq!(verdict.leader, Some(1));
        assert!(!verdict.clear);
    }

    #[test]
    fn a_day_counts_once_analytics_and_the_reach_report_have_it() {
        let mut test = october_test(2, 2);
        test.showings.push(Showing { version: 0, from: test.starts_at });
        let end = test.ends_at();
        test.set_reach(date("2026-10-01"), 100, 0.05, end);
        test.set_reach(date("2026-10-02"), 100, 0.05, end);
        // The reach of both days is in, Analytics has only the first: not done yet.
        test.record_views(&[(date("2026-10-01"), 5, 20.0)]);
        assert_eq!(test.measured_through(), Some(date("2026-10-01")));
        assert_eq!(test.step(end + TimeDelta::hours(1)), TestStep::Wait);
        assert_eq!(test.results(end)[0].days_measured, 1.0);
        // Analytics leaves out a day without views when it has a later one.
        let mut gap = october_test(2, 3);
        gap.record_views(&[(date("2026-10-01"), 5, 20.0), (date("2026-10-03"), 4, 10.0)]);
        let second = gap.numbers.iter().find(|day| day.date == date("2026-10-02")).unwrap();
        assert!(second.views_reported && second.views == 0);
    }

    #[test]
    fn a_half_done_switch_puts_the_due_version_up_again() {
        let mut test = AbTest::new(
            TestSubject::Both,
            titles(2),
            7,
            Some("Before".into()),
            pacific_midnight(date("2026-10-01")),
            None,
        );
        test.showings.push(Showing { version: 0, from: test.starts_at });
        let later = test.starts_at + TimeDelta::hours(2);
        assert_eq!(test.step(later), TestStep::Wait);
        // B's title went up, its thumbnail didn't, and the title couldn't go back.
        test.title_up = Some(1);
        assert_eq!(test.live_title(), Some("Title 1"));
        assert_eq!(test.step(later), TestStep::Show(0));
    }

    #[test]
    fn the_live_title_is_what_rushes_put_up() {
        let mut test = october_test(2, 14);
        assert_eq!(test.live_title(), Some("Before"));
        test.showings.push(Showing { version: 1, from: test.starts_at });
        assert_eq!(test.live_title(), Some("Title 1"));
        let json = serde_json::to_string(&test).unwrap();
        assert_eq!(serde_json::from_str::<AbTest>(&json).unwrap(), test);
    }
}
