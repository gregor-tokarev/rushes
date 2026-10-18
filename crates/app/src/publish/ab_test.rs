//! [`AbTests`]: runs the A/B tests of videos on YouTube (see `rushes_core::ab_test`).
//!
//! Every minute it looks at the videos with a running test: when a version's day
//! has come, it puts that version's title or thumbnail on YouTube and records when;
//! every few hours it reads the test's numbers (views and watch time from YouTube
//! Analytics, impressions and clicks from the channel's reach reports); after the
//! last day it keeps the winner, or A, and ends the test. It only works while Rushes
//! runs: a switch missed while it was closed happens at the next start, and the
//! numbers of that day are shared by the hours each version was up.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, TimeDelta, Utc};
use gpui_kit::{App, AppContext as _, Context, Entity, Global, Subscription, Task};
use rushes_core::ab_test::{
    AbTest, MAX_VERSIONS, MIN_VERSIONS, Showing, StopReason, TestOutcome, TestStep, TestSubject, TestVersion,
    pacific_midnight, version_letter,
};
use rushes_core::{Video, VideoId};
use rushes_youtube::{Error as YoutubeError, ReachReport, TitleChange, YoutubeClient};

use super::rules::{THUMBNAIL_MAX_BYTES, TITLE_MAX_CHARS};
use super::service::{TestAccess, YoutubeEvent, YoutubeService, describe_error};
use crate::state::AppState;
use crate::ui::{notification, push_notification};

/// How often running tests are looked at: a switch is due at most this late.
const TICK: Duration = Duration::from_secs(60);
/// YouTube updates a day's numbers about once a day: they are read this often.
const READ_INTERVAL: TimeDelta = TimeDelta::hours(3);
/// A Publish tab shown again reads them at most this often.
const READ_ON_SHOW: TimeDelta = TimeDelta::minutes(10);
/// A change YouTube didn't take is tried again after this.
const RETRY_DELAY: TimeDelta = TimeDelta::minutes(5);
/// Settings key of the channels' reach reporting jobs, by channel id.
const REACH_JOBS_KEY: &str = "youtube.reach_jobs";
/// What a test of thumbnails takes.
const IMAGE_EXTENSIONS: [&str; 3] = ["jpg", "jpeg", "png"];

/// What an ended test does once its kept version is up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Finish {
    winner: Option<usize>,
    chosen: bool,
}

/// What the runner is doing for one video's test, for this run of the app.
#[derive(Default)]
struct Work {
    /// The version on its way to YouTube.
    switching: Option<usize>,
    reading: bool,
    /// When the numbers were last read, or tried.
    read_at: Option<DateTime<Utc>>,
    /// When to try the change YouTube didn't take again.
    retry_at: Option<DateTime<Utc>>,
    /// Why the last change didn't happen, as the creator should read it.
    switch_problem: Option<String>,
    /// Why the numbers couldn't be read.
    numbers_problem: Option<String>,
    /// The version the creator chose to keep while another was on its way.
    pending_end: Option<usize>,
}

/// Runs every video's A/B test. One per app, after [`YoutubeService`].
pub struct AbTests {
    state: Entity<AppState>,
    service: Entity<YoutubeService>,
    /// Now; tests set another clock to move through a test's days.
    clock: Rc<dyn Fn() -> DateTime<Utc>>,
    work: HashMap<VideoId, Work>,
    /// The reach reporting job of each channel, by channel id.
    reach_jobs: HashMap<String, String>,
    _tick: Task<()>,
    _subscriptions: Vec<Subscription>,
}

struct GlobalAbTests(Entity<AbTests>);

impl Global for GlobalAbTests {}

/// What a read of a test's numbers brought.
struct Numbers {
    job: String,
    views: Vec<rushes_youtube::DailyViews>,
    /// Day, impressions, click rate and when YouTube made the report.
    reach: Vec<(NaiveDate, u64, f64, DateTime<Utc>)>,
}

impl AbTests {
    /// Creates the runner and registers it globally. Call once at startup, after
    /// [`YoutubeService::init`].
    pub fn init(state: Entity<AppState>, cx: &mut App) -> Entity<Self> {
        let service = YoutubeService::global(cx);
        let runner = cx.new(|cx| {
            let tick = cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(TICK).await;
                    if this.update(cx, |this: &mut Self, cx| this.tick(cx)).is_err() {
                        break;
                    }
                }
            });
            let subscriptions = vec![cx.subscribe(&service, |this: &mut Self, _, event: &YoutubeEvent, cx| {
                // An account signed in again, or allowed what tests need: go on at once.
                if *event == YoutubeEvent::ConnectionChanged {
                    for work in this.work.values_mut() {
                        work.retry_at = None;
                    }
                    this.tick(cx);
                }
            })];
            let reach_jobs = match state.read(cx).read_setting::<HashMap<String, String>>(REACH_JOBS_KEY) {
                Ok(jobs) => jobs.unwrap_or_default(),
                Err(error) => {
                    log_line!("rushes: couldn't read the reach reporting jobs: {error}");
                    HashMap::new()
                }
            };
            Self {
                state: state.clone(),
                service: service.clone(),
                clock: Rc::new(Utc::now),
                work: HashMap::new(),
                reach_jobs,
                _tick: tick,
                _subscriptions: subscriptions,
            }
        });
        cx.set_global(GlobalAbTests(runner.clone()));
        runner.update(cx, |runner, cx| runner.tick(cx));
        runner
    }

    /// The runner, when the app has one (views tested on their own may not).
    pub fn try_global(cx: &App) -> Option<Entity<Self>> {
        cx.try_global::<GlobalAbTests>().map(|global| global.0.clone())
    }

    /// Replaces the clock, so tests can move through a test's days.
    pub fn set_clock(&mut self, clock: impl Fn() -> DateTime<Utc> + 'static) {
        self.clock = Rc::new(clock);
    }

    pub fn now(&self) -> DateTime<Utc> {
        (self.clock)()
    }

    /// The version on its way to YouTube for `video_id`'s test.
    pub fn switching(&self, video_id: VideoId) -> Option<usize> {
        self.work.get(&video_id).and_then(|work| work.switching)
    }

    /// Whether `video_id`'s numbers are being read.
    pub fn is_reading(&self, video_id: VideoId) -> bool {
        self.work.get(&video_id).is_some_and(|work| work.reading)
    }

    /// Why the last change of `video_id`'s test didn't reach YouTube; it is tried again.
    pub fn switch_problem(&self, video_id: VideoId) -> Option<&str> {
        self.work.get(&video_id).and_then(|work| work.switch_problem.as_deref())
    }

    /// Why the numbers of `video_id`'s test couldn't be read.
    pub fn numbers_problem(&self, video_id: VideoId) -> Option<&str> {
        self.work.get(&video_id).and_then(|work| work.numbers_problem.as_deref())
    }

    /// Whether `video` can have a test now, or why not.
    pub fn availability(&self, video: &Video, cx: &App) -> Result<(), &'static str> {
        let link = video.youtube.as_ref().ok_or("Upload the video to YouTube first.")?;
        if link.ab_test.as_ref().is_some_and(|test| !test.is_over()) {
            return Err("A test is running.");
        }
        let service = self.service.read(cx);
        let (privacy, publish_at) = match service.youtube_video(&link.video_id) {
            Some(info) => (info.privacy.as_str(), info.publish_at),
            None => (link.privacy.api_value(), None),
        };
        match (privacy, publish_at) {
            ("public", _) => Ok(()),
            ("private", Some(at)) if at > self.now() => Ok(()),
            _ => Err("A/B tests need a public video, or one scheduled to go public: YouTube doesn’t show private \
                 or unlisted videos in feeds and search."),
        }
    }

    /// Starts a test of `versions` on `video_id` for `days` days, putting A on YouTube
    /// at once. A scheduled video takes turns from when it goes public.
    pub fn start(
        &mut self,
        video_id: VideoId,
        subject: TestSubject,
        versions: Vec<TestVersion>,
        days: u32,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let problems = version_problems(subject, &versions, |path| std::fs::metadata(path).ok().map(|m| m.len()));
        if let Some(problem) = problems.into_iter().next() {
            return Err(problem);
        }
        let Some(mut video) = self.state.read(cx).video(video_id).cloned() else {
            return Err("The video is gone.".to_owned());
        };
        self.availability(&video, cx).map_err(str::to_owned)?;
        let Some(link) = video.youtube.as_mut() else { return Err("Upload the video to YouTube first.".to_owned()) };
        let service = self.service.read(cx);
        if let Some(problem) = access_problem(service.test_access(&link.channel_id), &link.channel_id, service) {
            return Err(problem);
        }
        let remote = service.youtube_video(&link.video_id);
        let original_title = remote.map(|info| info.title.clone());
        let publish_at = remote.and_then(|info| info.publish_at);
        let now = self.now();
        link.ab_test = Some(AbTest::new(subject, versions, days, original_title, now, publish_at));
        self.work.insert(video_id, Work::default());
        self.state.update(cx, |state, cx| state.update_video(video, cx));
        self.tick(cx);
        Ok(())
    }

    /// Ends `video_id`'s test, keeping `keep` on YouTube.
    pub fn end(&mut self, video_id: VideoId, keep: usize, cx: &mut Context<Self>) {
        let work = self.work.entry(video_id).or_default();
        if work.switching.is_some() {
            work.pending_end = Some(keep);
            return;
        }
        let Some(test) = self.running_test(video_id, cx) else { return };
        let verdict = test.verdict(self.now());
        let winner = verdict.leader.filter(|_| verdict.clear);
        self.switch(video_id, keep, Some(Finish { winner, chosen: true }), cx);
    }

    /// Reads `video_id`'s numbers again, unless they were read a moment ago.
    pub fn read_numbers(&mut self, video_id: VideoId, cx: &mut Context<Self>) {
        let now = self.now();
        let work = self.work.entry(video_id).or_default();
        if work.reading || work.read_at.is_some_and(|at| now < at + READ_ON_SHOW) {
            return;
        }
        self.read_numbers_now(video_id, cx);
    }

    /// Takes every running test a step: the due version goes up, the numbers are
    /// read when they are old, an ended test finishes.
    pub fn tick(&mut self, cx: &mut Context<Self>) {
        let now = self.now();
        let running: Vec<(VideoId, AbTest)> = self
            .state
            .read(cx)
            .rows()
            .iter()
            .filter_map(|row| {
                let test = row.video.youtube.as_ref()?.ab_test.as_ref()?;
                (!test.is_over()).then(|| (row.video.id, test.clone()))
            })
            .collect();
        for (video_id, test) in running {
            let work = self.work.entry(video_id).or_default();
            if work.switching.is_some() || work.reading {
                continue;
            }
            let stale = work.read_at.is_none_or(|at| now >= at + READ_INTERVAL);
            let retry_due = work.retry_at.is_none_or(|at| now >= at);
            let read_since_end = work.read_at.is_some_and(|at| at >= test.ends_at());
            match test.step(now) {
                TestStep::Show(version) if retry_due => self.switch(video_id, version, None, cx),
                TestStep::Finish => {
                    // The last numbers decide: read them once more after the end first.
                    if read_since_end {
                        if retry_due {
                            let (keep, winner) = test.version_to_keep(now);
                            self.switch(video_id, keep, Some(Finish { winner, chosen: false }), cx);
                        }
                    } else {
                        self.read_numbers_now(video_id, cx);
                    }
                    continue;
                }
                _ => {}
            }
            // Past the end, the last days' numbers decide when the test can.
            let due = stale || (now >= test.ends_at() && !read_since_end);
            if due && test.days_so_far(now).is_some() && !self.is_reading(video_id) {
                self.read_numbers_now(video_id, cx);
            }
        }
        // Videos deleted since take their work with them.
        let state = self.state.read(cx);
        self.work.retain(|video_id, _| state.video(*video_id).is_some());
    }

    fn running_test(&self, video_id: VideoId, cx: &App) -> Option<AbTest> {
        let test = self.state.read(cx).video(video_id)?.youtube.as_ref()?.ab_test.clone()?;
        (!test.is_over()).then_some(test)
    }

    /// Puts `version` of `video_id`'s test on YouTube; `then` ends the test once it is up.
    fn switch(&mut self, video_id: VideoId, version: usize, then: Option<Finish>, cx: &mut Context<Self>) {
        let Some(video) = self.state.read(cx).video(video_id).cloned() else { return };
        let Some(link) = video.youtube.clone() else { return };
        let Some(test) = link.ab_test.clone().filter(|test| !test.is_over()) else { return };
        let Some(target) = test.versions.get(version).cloned() else { return };
        if test.live_version() == Some(version) {
            if let Some(finish) = then {
                self.on_switched(video_id, test.created_at, version, Some(finish), Ok(None), cx);
            }
            return;
        }
        let service = self.service.read(cx);
        let client = match access_problem(service.test_access(&link.channel_id), &link.channel_id, service) {
            Some(problem) => Err(problem),
            None => service.client_for(&link.channel_id).ok_or_else(|| "Connect the account again.".to_owned()),
        };
        let client = match client {
            Ok(client) => client,
            Err(problem) => {
                let work = self.work.entry(video_id).or_default();
                work.switch_problem = Some(problem);
                cx.notify();
                return;
            }
        };
        // Until A is up, the title is whatever the video had: A replaces it as it is.
        let from = (!test.showings.is_empty()).then(|| test.live_title().map(str::to_owned)).flatten();
        self.work.entry(video_id).or_default().switching = Some(version);
        cx.notify();
        let (youtube_id, subject, created_at) = (link.video_id, test.subject, test.created_at);
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { put_up(&client, &youtube_id, subject, &target, from) }).await;
            this.update(cx, |this, cx| this.on_switched(video_id, created_at, version, then, result, cx)).ok();
        })
        .detach();
    }

    /// Records a change that reached YouTube (`Ok(None)`), a title someone changed
    /// elsewhere (`Ok(Some(title))`), which stops the test, or a failure, which is
    /// tried again.
    fn on_switched(
        &mut self,
        video_id: VideoId,
        created_at: DateTime<Utc>,
        version: usize,
        then: Option<Finish>,
        result: Result<Option<String>, YoutubeError>,
        cx: &mut Context<Self>,
    ) {
        let now = self.now();
        let work = self.work.entry(video_id).or_default();
        work.switching = None;
        let pending_end = work.pending_end.take();
        cx.notify();
        // The video may be gone, on YouTube again as another video, or its test over.
        let Some(mut video) = self.state.read(cx).video(video_id).cloned() else { return };
        let title = video.upload_title().to_owned();
        let Some(link) = video.youtube.as_mut() else { return };
        let channel = link.channel_id.clone();
        let Some(test) = link.ab_test.as_mut().filter(|test| test.created_at == created_at && !test.is_over()) else {
            return;
        };
        let announcement = match result {
            Ok(None) => {
                // Ending on the version that is up changes nothing on YouTube.
                if test.live_version() != Some(version) {
                    test.showings.push(Showing { version, from: now });
                }
                let image = test.versions[version].thumbnail.clone().filter(|_| test.subject.has_thumbnail());
                let announcement = then.map(|finish| {
                    test.outcome = Some(TestOutcome::Finished {
                        at: now,
                        kept: version,
                        winner: finish.winner,
                        chosen: finish.chosen,
                    });
                    ended_notice(&title, version, finish)
                });
                if let Some(image) = image {
                    link.thumbnail = Some(image);
                }
                let work = self.work.entry(video_id).or_default();
                work.switch_problem = None;
                work.retry_at = None;
                announcement
            }
            Ok(Some(changed)) => {
                test.outcome =
                    Some(TestOutcome::Stopped { at: now, reason: StopReason::TitleChanged { title: changed.clone() } });
                Some(Announcement::Stopped {
                    title: format!("The A/B test of “{title}” stopped"),
                    detail: format!("Its title was changed on YouTube to “{changed}”, so Rushes left it as it is."),
                })
            }
            Err(error) => {
                if error.needs_sign_in() && !error.is_missing_scope() {
                    self.service.update(cx, |service, cx| service.require_sign_in(&channel, cx));
                }
                // Deleted in YouTube Studio: asking YouTube about it drops the dead link.
                if matches!(error, YoutubeError::VideoNotFound) {
                    let link = link.clone();
                    self.service.update(cx, |service, cx| service.refresh_youtube_video(&link, cx));
                }
                let work = self.work.entry(video_id).or_default();
                work.switch_problem = Some(test_error(&error));
                work.retry_at = Some(now + RETRY_DELAY);
                if let Some(keep) = pending_end {
                    work.pending_end = Some(keep);
                }
                return;
            }
        };
        self.state.update(cx, |state, cx| state.update_video(video, cx));
        if let Some(announcement) = announcement {
            push_notification(cx, move |cx| match announcement {
                Announcement::Ended { title, detail } => notification::success(detail, cx).title(title),
                Announcement::Stopped { title, detail } => notification::warning(detail, cx).title(title),
            });
        }
        if let Some(keep) = pending_end {
            self.end(video_id, keep, cx);
        }
    }

    /// Reads the numbers of `video_id`'s test so far: views and watch time of every
    /// day, and the reach of the days whose report is new.
    fn read_numbers_now(&mut self, video_id: VideoId, cx: &mut Context<Self>) {
        let now = self.now();
        let Some(test) = self.running_test(video_id, cx) else { return };
        let Some((first, last)) = test.days_so_far(now) else { return };
        let Some(link) = self.state.read(cx).video(video_id).and_then(|video| video.youtube.clone()) else { return };
        let service = self.service.read(cx);
        let client = match access_problem(service.test_access(&link.channel_id), &link.channel_id, service) {
            Some(problem) => Err(problem),
            None => service.client_for(&link.channel_id).ok_or_else(|| "Connect the account again.".to_owned()),
        };
        let work = self.work.entry(video_id).or_default();
        work.read_at = Some(now);
        let client = match client {
            Ok(client) => client,
            Err(problem) => {
                work.numbers_problem = Some(problem);
                cx.notify();
                return;
            }
        };
        work.reading = true;
        cx.notify();
        let known: HashMap<NaiveDate, DateTime<Utc>> =
            test.numbers.iter().filter_map(|day| day.reach_report.map(|report| (day.date, report))).collect();
        let job = self.reach_jobs.get(&link.channel_id).cloned();
        let (youtube_id, created_at, channel) = (link.video_id, test.created_at, link.channel_id);
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { read(&client, &youtube_id, first, last, job, &known) }).await;
            this.update(cx, |this, cx| this.on_numbers(video_id, created_at, &channel, result, cx)).ok();
        })
        .detach();
    }

    fn on_numbers(
        &mut self,
        video_id: VideoId,
        created_at: DateTime<Utc>,
        channel: &str,
        result: Result<Numbers, YoutubeError>,
        cx: &mut Context<Self>,
    ) {
        let work = self.work.entry(video_id).or_default();
        work.reading = false;
        cx.notify();
        let numbers = match result {
            Ok(numbers) => {
                work.numbers_problem = None;
                numbers
            }
            Err(error) => {
                work.numbers_problem = Some(test_error(&error));
                // A job deleted elsewhere is made again the next time.
                if matches!(error, YoutubeError::Api { status: 404, .. }) && self.reach_jobs.remove(channel).is_some() {
                    self.save_reach_jobs(cx);
                }
                return;
            }
        };
        if self.reach_jobs.get(channel) != Some(&numbers.job) {
            self.reach_jobs.insert(channel.to_owned(), numbers.job.clone());
            self.save_reach_jobs(cx);
        }
        let Some(mut video) = self.state.read(cx).video(video_id).cloned() else { return };
        let Some(test) = video
            .youtube
            .as_mut()
            .and_then(|link| link.ab_test.as_mut())
            .filter(|test| test.created_at == created_at && !test.is_over())
        else {
            return;
        };
        let before = test.numbers.clone();
        for day in &numbers.views {
            test.set_views(day.date, day.views, day.watch_minutes);
        }
        for (date, impressions, click_rate, report) in numbers.reach {
            test.set_reach(date, impressions, click_rate, report);
        }
        if test.numbers != before {
            self.state.update(cx, |state, cx| state.update_video(video, cx));
        }
        // An ended test waited for these.
        self.tick(cx);
    }

    fn save_reach_jobs(&mut self, cx: &mut Context<Self>) {
        let jobs = self.reach_jobs.clone();
        // A refusal is reported by the state; this run still knows.
        self.state.update(cx, |state, cx| state.save_setting(REACH_JOBS_KEY, &jobs, cx));
    }
}

/// What to tell the creator about a test that ended or stopped.
enum Announcement {
    Ended { title: String, detail: String },
    Stopped { title: String, detail: String },
}

fn ended_notice(video_title: &str, kept: usize, finish: Finish) -> Announcement {
    let letter = version_letter(kept);
    let detail = match (finish.chosen, finish.winner) {
        (true, _) => format!("{letter} stays on YouTube."),
        (false, Some(_)) => format!("{letter} earned the most watch time per impression, so it stays on YouTube."),
        (false, None) => format!("No version did clearly better, so {letter} stays on YouTube."),
    };
    Announcement::Ended { title: format!("The A/B test of “{video_title}” is over"), detail }
}

/// Why tests can't use the account `key` now, as the creator should read it; `None`
/// when they can.
fn access_problem(access: TestAccess, key: &str, service: &YoutubeService) -> Option<String> {
    let name = service.find_account(key).map_or_else(|| "The video’s account".to_owned(), |account| account.name());
    Some(match access {
        TestAccess::Allowed => return None,
        TestAccess::NeedsPermission => {
            format!("Rushes needs permission to change titles and read analytics on {name}. Allow access to go on.")
        }
        TestAccess::SigningIn => "Finish signing in with your browser.".to_owned(),
        TestAccess::NeedsSignIn => format!("Sign in to {name} again to go on."),
        TestAccess::NotConnected => "Connect the YouTube account the video is on to go on.".to_owned(),
    })
}

/// A failure of a test's YouTube call, as the creator should read it.
fn test_error(error: &YoutubeError) -> String {
    match error {
        error if error.is_missing_scope() => {
            "Rushes isn’t allowed to change titles or read analytics on this account. Allow access to go on.".to_owned()
        }
        error if error.is_api_disabled() => "Turn on the YouTube Analytics API and the YouTube Reporting API in your \
             Google Cloud project (APIs & Services › Library). Rushes tries again in a few hours."
            .to_owned(),
        YoutubeError::VideoNotFound => "YouTube doesn’t have this video anymore.".to_owned(),
        error => describe_error(error),
    }
}

/// Puts `version` on the YouTube video `youtube_id`: its title, replacing `from`,
/// then its thumbnail. `Ok(Some(title))` when someone changed the title elsewhere.
fn put_up(
    client: &Arc<YoutubeClient>,
    youtube_id: &str,
    subject: TestSubject,
    version: &TestVersion,
    from: Option<String>,
) -> Result<Option<String>, YoutubeError> {
    if subject.has_title()
        && let Some(title) = &version.title
        && let TitleChange::Changed(current) = client.replace_title(youtube_id, from.as_deref(), title)?
    {
        return Ok(Some(current));
    }
    if subject.has_thumbnail()
        && let Some(image) = &version.thumbnail
    {
        client.set_thumbnail(youtube_id, image)?;
    }
    Ok(None)
}

/// The numbers of the days `first` to `last`; the reach only of days whose newest
/// report is newer than the one in `known`. `job` is the channel's reach job, when known.
fn read(
    client: &Arc<YoutubeClient>,
    youtube_id: &str,
    first: NaiveDate,
    last: NaiveDate,
    job: Option<String>,
    known: &HashMap<NaiveDate, DateTime<Utc>>,
) -> Result<Numbers, YoutubeError> {
    let views = client.daily_views(youtube_id, first, last)?;
    let job = match job {
        Some(job) => job,
        None => client.reach_job()?,
    };
    let mut newest: BTreeMap<NaiveDate, ReachReport> = BTreeMap::new();
    for report in client.reach_reports(&job, pacific_midnight(first))? {
        let day = report.day();
        if day < first || day > last || newest.get(&day).is_some_and(|newer| newer.created >= report.created) {
            continue;
        }
        newest.insert(day, report);
    }
    let mut reach = Vec::new();
    for (day, report) in newest {
        if known.get(&day).is_some_and(|known| *known >= report.created) {
            continue;
        }
        let numbers = client.reach_of(&report, youtube_id)?;
        reach.push((day, numbers.impressions, numbers.click_rate, report.created));
    }
    Ok(Numbers { job, views, reach })
}

/// What stops `versions` from being tested, in form order: each version needs
/// what `subject` tests, valid for YouTube and different from the others.
/// `size_of` reads an image's size, `None` when it is missing.
pub fn version_problems(
    subject: TestSubject,
    versions: &[TestVersion],
    size_of: impl Fn(&Path) -> Option<u64>,
) -> Vec<String> {
    let mut problems = Vec::new();
    if versions.len() < MIN_VERSIONS || versions.len() > MAX_VERSIONS {
        problems.push(format!("A test compares {MIN_VERSIONS} or {MAX_VERSIONS} versions."));
    }
    let mut titles: Vec<String> = Vec::new();
    let mut images: Vec<&PathBuf> = Vec::new();
    for (ix, version) in versions.iter().enumerate() {
        let letter = version_letter(ix);
        if subject.has_title() {
            let title = version.title.as_deref().unwrap_or_default().trim();
            let length = title.chars().count();
            if title.is_empty() {
                problems.push(format!("Give version {letter} a title."));
            } else if length > TITLE_MAX_CHARS {
                problems
                    .push(format!("Shorten the title of {letter} to {TITLE_MAX_CHARS} characters (it has {length})."));
            } else if title.contains(['<', '>']) {
                problems.push(format!("Remove < and > from the title of {letter}."));
            } else if let Some(same) = titles.iter().position(|other| other.eq_ignore_ascii_case(title)) {
                problems.push(format!("Versions {} and {letter} have the same title.", version_letter(same)));
            }
            titles.push(title.to_owned());
        }
        if subject.has_thumbnail() {
            match &version.thumbnail {
                None => problems.push(format!("Choose a thumbnail for version {letter}.")),
                Some(path) => {
                    let image = path.extension().and_then(|ext| ext.to_str()).map(str::to_ascii_lowercase);
                    if !image.is_some_and(|ext| IMAGE_EXTENSIONS.contains(&ext.as_str())) {
                        problems.push(format!("The thumbnail of {letter} must be a JPG or PNG image."));
                    } else {
                        match size_of(path) {
                            None => problems.push(format!("The thumbnail of {letter} is missing.")),
                            Some(size) if size > THUMBNAIL_MAX_BYTES => {
                                problems.push(format!("The thumbnail of {letter} is larger than YouTube’s 2 MB limit."))
                            }
                            Some(_) => {}
                        }
                    }
                    if let Some(same) = images.iter().position(|other| *other == path) {
                        problems
                            .push(format!("Versions {} and {letter} have the same thumbnail.", version_letter(same)));
                    }
                    images.push(path);
                }
            }
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    fn title(text: &str) -> TestVersion {
        TestVersion { title: Some(text.to_owned()), thumbnail: None }
    }

    fn image(name: &str) -> TestVersion {
        TestVersion { title: None, thumbnail: Some(PathBuf::from(name)) }
    }

    #[test]
    fn versions_need_what_the_test_compares() {
        let sizes = |path: &Path| match path.to_str() {
            Some("big.png") => Some(3 * 1024 * 1024),
            Some("gone.jpg") => None,
            _ => Some(1000),
        };
        assert!(version_problems(TestSubject::Title, &[title("One"), title("Two")], sizes).is_empty());
        assert_eq!(
            version_problems(TestSubject::Title, &[title("One"), title(" one "), title("")], sizes),
            vec!["Versions A and B have the same title.", "Give version C a title."]
        );
        assert_eq!(
            version_problems(TestSubject::Title, &[title("One")], sizes),
            vec!["A test compares 2 or 3 versions."]
        );
        let long = "x".repeat(101);
        assert_eq!(
            version_problems(TestSubject::Title, &[title(&long), title("a <b>")], sizes),
            vec!["Shorten the title of A to 100 characters (it has 101).", "Remove < and > from the title of B."]
        );
        assert_eq!(
            version_problems(TestSubject::Thumbnail, &[image("a.png"), image("a.png"), image("big.png")], sizes),
            vec![
                "Versions A and B have the same thumbnail.",
                "The thumbnail of C is larger than YouTube’s 2 MB limit."
            ]
        );
        assert_eq!(
            version_problems(TestSubject::Thumbnail, &[image("a.gif"), image("gone.jpg")], sizes),
            vec!["The thumbnail of A must be a JPG or PNG image.", "The thumbnail of B is missing."]
        );
        let both = [TestVersion { title: Some("One".into()), thumbnail: None }, title("Two")];
        assert_eq!(
            version_problems(TestSubject::Both, &both, sizes),
            vec!["Choose a thumbnail for version A.", "Choose a thumbnail for version B."]
        );
    }
}
