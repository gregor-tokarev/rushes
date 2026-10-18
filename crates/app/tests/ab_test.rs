//! A/B tests end to end against the local Google + YouTube emulators: allowing
//! what tests need, versions taking turns on YouTube day by day (on a clock the
//! tests move), the numbers read from YouTube Analytics and the reach reports, and
//! how a test ends. Skipped with a notice when Node or `emulators/node_modules` is
//! missing.

mod publish_support;

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, TimeDelta, Utc};
use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, Entity, Render, TestAppContext, Window, WindowBounds, WindowOptions,
    px, size,
};
use publish_support::{Emulators, Workspace, wait_until};
use rushes::publish::{AbTests, PublishPanel, TestAccess};
use rushes_core::ab_test::{AbTest, StopReason, TestOutcome, TestSubject, TestVersion, pacific_midnight};
use rushes_core::{Privacy, UploadStatus, VideoId};
use serde_json::json;

/// The runner, on a clock the test moves.
struct Runner {
    runner: Entity<AbTests>,
    now: Rc<Cell<DateTime<Utc>>>,
}

impl Runner {
    fn start(workspace: &Workspace, cx: &mut TestAppContext, now: DateTime<Utc>) -> Self {
        let state = workspace.state.clone();
        let runner = cx.update(|cx| AbTests::init(state, cx));
        let clock = Rc::new(Cell::new(now));
        let shared = clock.clone();
        runner.update(cx, |runner, _| runner.set_clock(move || shared.get()));
        Self { runner, now: clock }
    }

    /// Moves the clock to `at` and lets the runner act.
    fn at(&self, cx: &mut TestAppContext, at: DateTime<Utc>) {
        self.now.set(at);
        self.runner.update(cx, |runner, cx| runner.tick(cx));
    }
}

fn day(text: &str) -> NaiveDate {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap()
}

/// A moment of the Pacific day `date`, `hours` after its midnight.
fn on(date: &str, hours: i64) -> DateTime<Utc> {
    pacific_midnight(day(date)) + TimeDelta::hours(hours)
}

/// Signs in, allows what tests need, and uploads a public video titled `title`.
/// Returns the app's video and its YouTube id.
fn upload_public(workspace: &Workspace, cx: &mut TestAppContext, title: &str) -> (VideoId, String) {
    workspace.sign_in(cx, "creator@example.com");
    allow_tests(workspace, cx);
    upload(workspace, cx, title, Privacy::Public)
}

/// Signs the account in again with what tests need, as "Allow access…" does.
fn allow_tests(workspace: &Workspace, cx: &mut TestAppContext) {
    let service = workspace.service.clone();
    let key = workspace.account_key(cx, "creator@example.com");
    // A sign-in from before A/B tests can upload but not test.
    assert_eq!(service.read_with(cx, |s, _| s.test_access(&key)), TestAccess::NeedsPermission);
    service.update(cx, |service, cx| service.allow_tests(&key, cx));
    assert!(service.read_with(cx, |s, _| s.is_allowing_tests()));
    let url =
        wait_until(cx, "the consent page", |cx| service.read_with(cx, |s, _| s.sign_in_url().map(ToString::to_string)));
    assert!(url.contains("yt-analytics.readonly"), "{url}");
    publish_support::consent_in_browser(&url, "creator@example.com");
    wait_until(cx, "the permissions", |cx| {
        (service.read_with(cx, |s, _| s.test_access(&key)) == TestAccess::Allowed).then_some(())
    });
}

/// Uploads a video titled `title`; the app's video and its YouTube id, once YouTube
/// said how it stands.
fn upload(workspace: &Workspace, cx: &mut TestAppContext, title: &str, privacy: Privacy) -> (VideoId, String) {
    let service = workspace.service.clone();
    let file = publish_support::video_file(workspace.dir.path(), &format!("{title}.mp4"));
    let video_id = workspace.create_video(cx, title, |video| {
        video.publish.privacy = privacy;
        video.publish.file = Some(file.clone());
    });
    let upload = service.update(cx, |service, cx| service.start_upload(video_id, cx)).unwrap();
    let youtube_id = wait_until(cx, "the upload", |cx| {
        cx.executor().advance_clock(Duration::from_secs(5));
        match service.read_with(cx, |s, _| s.job(upload).map(|job| job.status.clone())) {
            Some(UploadStatus::Done { video_id } | UploadStatus::Processing { video_id }) => Some(video_id),
            Some(UploadStatus::Failed { error }) => panic!("upload failed: {error}"),
            _ => None,
        }
    });
    // What YouTube says about the video decides whether a test can run.
    let link = workspace.video(cx, video_id).youtube.unwrap();
    service.update(cx, |service, cx| service.refresh_youtube_video(&link, cx));
    wait_until(cx, "YouTube's answer", |cx| service.read_with(cx, |s, _| s.youtube_video(&youtube_id).map(drop)));
    (video_id, youtube_id)
}

fn test_of(workspace: &Workspace, cx: &mut TestAppContext, video_id: VideoId) -> AbTest {
    workspace.video(cx, video_id).youtube.and_then(|link| link.ab_test).expect("a test")
}

/// Waits until the test has `count` showings, the last one on YouTube.
fn wait_for_showings(workspace: &Workspace, cx: &mut TestAppContext, video_id: VideoId, count: usize) -> AbTest {
    wait_until(cx, &format!("showing {count}"), |cx| {
        Some(test_of(workspace, cx, video_id)).filter(|test| test.showings.len() == count)
    })
}

fn title_on_youtube(emulators: &Emulators, youtube_id: &str) -> String {
    emulators.youtube_video(youtube_id)["snippet"]["title"].as_str().unwrap().to_owned()
}

#[gpui_kit::test]
fn titles_take_turns_and_the_winner_stays(cx: &mut TestAppContext) {
    let Some(emulators) = Emulators::start() else { return };
    let workspace = Workspace::open(cx, emulators.backend());
    let (video_id, youtube_id) = upload_public(&workspace, cx, "How I bake bread");
    let start = on("2026-03-01", 1);
    let runner = Runner::start(&workspace, cx, start);

    let versions = vec![
        TestVersion { title: Some("How I bake bread".into()), thumbnail: None },
        TestVersion { title: Some("24 hours of sourdough".into()), thumbnail: None },
    ];
    // Thumbnails of a title test are left alone.
    let started = runner.runner.update(cx, |runner, cx| runner.start(video_id, TestSubject::Title, versions, 4, cx));
    assert_eq!(started, Ok(()));
    let again = runner.runner.update(cx, |runner, cx| {
        runner.start(video_id, TestSubject::Title, vec![TestVersion::default(), TestVersion::default()], 4, cx)
    });
    assert!(again.is_err(), "one test at a time");

    // A goes up at once: here, the title the video already has.
    let test = wait_for_showings(&workspace, cx, video_id, 1);
    assert_eq!(test.original_title.as_deref(), Some("How I bake bread"));
    assert_eq!(test.live_version(), Some(0));
    assert_eq!(title_on_youtube(&emulators, &youtube_id), "How I bake bread");

    // A B B A, a day each: each switch puts the title of the day's version up.
    for (date, version, title) in [("2026-03-02", 1, "24 hours of sourdough"), ("2026-03-04", 0, "How I bake bread")] {
        runner.at(cx, on(date, 0) + TimeDelta::minutes(1));
        let count = test_of(&workspace, cx, video_id).showings.len() + 1;
        let test = wait_for_showings(&workspace, cx, video_id, count);
        assert_eq!(test.live_version(), Some(version), "{date}");
        assert_eq!(title_on_youtube(&emulators, &youtube_id), title);
        // The day after a switch is the same version's again: nothing changes.
        runner.at(cx, on(date, 0) + TimeDelta::days(1) - TimeDelta::hours(1));
        cx.run_until_parked();
        assert_eq!(test_of(&workspace, cx, video_id).showings.len(), count);
    }

    // YouTube's numbers: B's days earn twice the watch time per impression.
    let numbers = |date: &str, minutes: f64| json!({"date": date, "views": 50, "minutesWatched": minutes, "impressions": 1000, "ctr": 0.05});
    emulators.set_analytics(
        &youtube_id,
        json!([
            numbers("2026-03-01", 100.0),
            numbers("2026-03-02", 200.0),
            numbers("2026-03-03", 210.0),
            numbers("2026-03-04", 104.0),
            numbers("2026-03-05", 100.0),
        ]),
    );

    // Over: the numbers are read once more, then B, the clear winner, goes back up.
    let end = test_of(&workspace, cx, video_id).ends_at();
    runner.at(cx, end + TimeDelta::minutes(1));
    let test = wait_until(cx, "the end", |cx| Some(test_of(&workspace, cx, video_id)).filter(AbTest::is_over));
    assert_eq!(
        test.outcome,
        Some(TestOutcome::Finished { at: end + TimeDelta::minutes(1), kept: 1, winner: Some(1), chosen: false })
    );
    assert_eq!(title_on_youtube(&emulators, &youtube_id), "24 hours of sourdough");
    assert_eq!(test.numbers.len(), 5);
    assert!(test.numbers.iter().all(|day| day.impressions == Some(1000)));
    let results = test.results(end);
    assert!(results[1].score().unwrap() > 1.8 * results[0].score().unwrap());
    let history: Vec<String> = emulators.youtube_video(&youtube_id)["history"]
        .as_array()
        .unwrap()
        .iter()
        .map(|showing| showing["title"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(history, ["How I bake bread", "24 hours of sourdough", "How I bake bread", "24 hours of sourdough"]);

    // A new test can follow.
    let video = workspace.video(cx, video_id);
    assert_eq!(runner.runner.read_with(cx, |runner, _| runner.availability(&video)), Ok(()));
    let job = workspace.state.read_with(cx, |state, _| {
        state.read_setting::<std::collections::HashMap<String, String>>("youtube.reach_jobs").unwrap()
    });
    assert_eq!(job.map(|jobs| jobs.len()), Some(1), "the channel's reach job is kept");
}

#[gpui_kit::test]
fn thumbnails_take_turns_and_ending_early_keeps_the_choice(cx: &mut TestAppContext) {
    let Some(emulators) = Emulators::start() else { return };
    let workspace = Workspace::open(cx, emulators.backend());
    let (video_id, youtube_id) = upload_public(&workspace, cx, "Desk tour");
    let runner = Runner::start(&workspace, cx, on("2026-05-10", 15));
    let images: Vec<PathBuf> =
        ["a.png", "b.png"].iter().map(|name| publish_support::png_file(workspace.dir.path(), name)).collect();
    // The two files must differ: the emulator tells thumbnails apart by their bytes.
    std::fs::write(&images[1], [std::fs::read(&images[1]).unwrap(), vec![0]].concat()).unwrap();
    let versions = images.iter().map(|path| TestVersion { title: None, thumbnail: Some(path.clone()) }).collect();
    let started =
        runner.runner.update(cx, |runner, cx| runner.start(video_id, TestSubject::Thumbnail, versions, 14, cx));
    assert_eq!(started, Ok(()));

    let sha =
        |emulators: &Emulators| emulators.youtube_video(&youtube_id)["thumbnail"]["sha256"].as_str().map(str::to_owned);
    wait_for_showings(&workspace, cx, video_id, 1);
    assert_eq!(sha(&emulators), Some(publish_support::sha256(&images[0])));
    // The link knows which image YouTube has; the title stays.
    assert_eq!(workspace.video(cx, video_id).youtube.unwrap().thumbnail.as_ref(), Some(&images[0]));
    runner.at(cx, on("2026-05-11", 0) + TimeDelta::seconds(30));
    wait_for_showings(&workspace, cx, video_id, 2);
    assert_eq!(sha(&emulators), Some(publish_support::sha256(&images[1])));
    assert_eq!(title_on_youtube(&emulators, &youtube_id), "Desk tour");

    // Choosing another image in the form waits for the test.
    let other = publish_support::png_file(workspace.dir.path(), "other.png");
    let mut video = workspace.video(cx, video_id);
    video.publish.thumbnail = Some(other);
    workspace.state.update(cx, |state, cx| state.update_video(video, cx));
    workspace.service.update(cx, |service, cx| service.set_youtube_thumbnail(video_id, cx));
    cx.run_until_parked();
    assert_eq!(sha(&emulators), Some(publish_support::sha256(&images[1])));

    // The creator ends it early and keeps A.
    runner.at(cx, on("2026-05-11", 9));
    runner.runner.update(cx, |runner, cx| runner.end(video_id, 0, cx));
    let test = wait_until(cx, "the end", |cx| Some(test_of(&workspace, cx, video_id)).filter(AbTest::is_over));
    assert!(matches!(test.outcome, Some(TestOutcome::Finished { kept: 0, winner: None, chosen: true, .. })));
    assert_eq!(sha(&emulators), Some(publish_support::sha256(&images[0])));
}

#[gpui_kit::test]
fn a_title_changed_in_studio_stops_the_test(cx: &mut TestAppContext) {
    let Some(emulators) = Emulators::start() else { return };
    let workspace = Workspace::open(cx, emulators.backend());
    let (video_id, youtube_id) = upload_public(&workspace, cx, "Original");
    let runner = Runner::start(&workspace, cx, on("2026-07-01", 12));
    let versions = vec![
        TestVersion { title: Some("Version A".into()), thumbnail: None },
        TestVersion { title: Some("Version B".into()), thumbnail: None },
    ];
    runner.runner.update(cx, |runner, cx| runner.start(video_id, TestSubject::Title, versions, 7, cx)).unwrap();
    wait_for_showings(&workspace, cx, video_id, 1);
    // A replaced the title the video had.
    assert_eq!(title_on_youtube(&emulators, &youtube_id), "Version A");

    let token = workspace.service.read_with(cx, |s, _| s.client()).unwrap().credentials().access_token;
    emulators.change_title(&token, &youtube_id, "Fixed in Studio");
    runner.at(cx, on("2026-07-02", 0) + TimeDelta::minutes(5));
    let test = wait_until(cx, "the stop", |cx| Some(test_of(&workspace, cx, video_id)).filter(AbTest::is_over));
    assert!(matches!(
        test.outcome,
        Some(TestOutcome::Stopped { reason: StopReason::TitleChanged { ref title }, .. }) if title == "Fixed in Studio"
    ));
    assert_eq!(title_on_youtube(&emulators, &youtube_id), "Fixed in Studio");
}

#[gpui_kit::test]
fn an_end_youtube_refused_is_tried_again(cx: &mut TestAppContext) {
    let Some(emulators) = Emulators::start() else { return };
    let workspace = Workspace::open(cx, emulators.backend());
    let (video_id, youtube_id) = upload_public(&workspace, cx, "Retry me");
    let start = on("2026-04-01", 9);
    let runner = Runner::start(&workspace, cx, start);
    let versions = vec![
        TestVersion { title: Some("Retry me".into()), thumbnail: None },
        TestVersion { title: Some("Second try".into()), thumbnail: None },
    ];
    runner.runner.update(cx, |runner, cx| runner.start(video_id, TestSubject::Title, versions, 14, cx)).unwrap();
    wait_for_showings(&workspace, cx, video_id, 1);

    // Ending on B, the day A is due: YouTube refuses the title once.
    emulators.inject_fault(json!({"method": "PUT", "path": "/youtube/v3/videos", "status": 500}));
    runner.runner.update(cx, |runner, cx| runner.end(video_id, 1, cx));
    wait_until(cx, "the refusal", |cx| {
        runner.runner.read_with(cx, |runner, _| runner.switch_problem(video_id).map(drop))
    });
    assert_eq!(
        runner.runner.read_with(cx, |runner, _| runner.switch_problem(video_id).map(|(version, _)| version)),
        Some(1)
    );
    assert!(!test_of(&workspace, cx, video_id).is_over());
    assert_eq!(title_on_youtube(&emulators, &youtube_id), "Retry me");

    // The end is tried again, though the schedule wants A today.
    runner.at(cx, start + TimeDelta::minutes(6));
    let test = wait_until(cx, "the end", |cx| Some(test_of(&workspace, cx, video_id)).filter(AbTest::is_over));
    assert!(matches!(test.outcome, Some(TestOutcome::Finished { kept: 1, chosen: true, .. })));
    assert_eq!(title_on_youtube(&emulators, &youtube_id), "Second try");
}

#[gpui_kit::test]
fn a_pair_that_went_up_halfway_is_put_right(cx: &mut TestAppContext) {
    let Some(emulators) = Emulators::start() else { return };
    let workspace = Workspace::open(cx, emulators.backend());
    let (video_id, youtube_id) = upload_public(&workspace, cx, "Pair");
    let runner = Runner::start(&workspace, cx, on("2026-06-01", 9));
    let images: Vec<PathBuf> =
        ["a.png", "b.png"].iter().map(|name| publish_support::png_file(workspace.dir.path(), name)).collect();
    std::fs::write(&images[1], [std::fs::read(&images[1]).unwrap(), vec![0]].concat()).unwrap();
    let versions = vec![
        TestVersion { title: Some("Pair".into()), thumbnail: Some(images[0].clone()) },
        TestVersion { title: Some("Pair, part two".into()), thumbnail: Some(images[1].clone()) },
    ];
    runner.runner.update(cx, |runner, cx| runner.start(video_id, TestSubject::Both, versions, 14, cx)).unwrap();
    wait_for_showings(&workspace, cx, video_id, 1);
    let sha = |emulators: &Emulators| {
        emulators.youtube_video(&youtube_id)["thumbnail"]["sha256"].as_str().map(str::to_owned).unwrap()
    };
    let a = publish_support::sha256(&images[0]);
    assert_eq!(sha(&emulators), a);
    let problem = |cx: &mut TestAppContext| {
        runner.runner.read_with(cx, |runner, _| runner.switch_problem(video_id).map(|(version, _)| version))
    };

    // B's turn: its title goes up, its thumbnail doesn't, and the title goes back.
    emulators.inject_fault(json!({"method": "POST", "path": "/upload/youtube/v3/thumbnails/set", "status": 500}));
    runner.at(cx, on("2026-06-02", 0) + TimeDelta::minutes(1));
    wait_until(cx, "the failed switch", |cx| problem(cx).filter(|version| *version == 1));
    assert_eq!((title_on_youtube(&emulators, &youtube_id), sha(&emulators)), ("Pair".to_owned(), a.clone()));
    let test = test_of(&workspace, cx, video_id);
    assert_eq!((test.showings.len(), test.title_up), (1, None));

    // Again, and now the title can't go back either: the test knows what YouTube has.
    emulators.inject_fault(json!({"method": "POST", "path": "/upload/youtube/v3/thumbnails/set", "status": 500}));
    emulators.inject_fault(json!({"method": "PUT", "path": "/youtube/v3/videos", "status": 500, "skip": 1}));
    runner.at(cx, on("2026-06-02", 0) + TimeDelta::minutes(7));
    let test = wait_until(cx, "the half switch", |cx| {
        Some(test_of(&workspace, cx, video_id)).filter(|test| test.title_up.is_some())
    });
    assert_eq!((test.title_up, test.live_version()), (Some(1), Some(0)));
    assert_eq!((title_on_youtube(&emulators, &youtube_id), sha(&emulators)), ("Pair, part two".to_owned(), a.clone()));

    // Ending on A puts A back whole.
    emulators.clear_faults();
    runner.runner.update(cx, |runner, cx| runner.end(video_id, 0, cx));
    let test = wait_until(cx, "the end", |cx| Some(test_of(&workspace, cx, video_id)).filter(AbTest::is_over));
    assert!(matches!(test.outcome, Some(TestOutcome::Finished { kept: 0, chosen: true, .. })));
    assert_eq!(test.title_up, None);
    assert_eq!((title_on_youtube(&emulators, &youtube_id), sha(&emulators)), ("Pair".to_owned(), a));
}

#[gpui_kit::test]
fn a_scheduled_test_follows_the_video_schedule(cx: &mut TestAppContext) {
    // No YouTube needed: the test follows what the video's link records YouTube having.
    let workspace = Workspace::open(cx, rushes_core::settings::YoutubeBackend::default());
    let now = on("2026-10-18", 9);
    let publish_at = on("2026-10-19", 9);
    let versions = vec![
        TestVersion { title: Some("One".into()), thumbnail: None },
        TestVersion { title: Some("Two".into()), thumbnail: None },
    ];
    let test = AbTest::new(TestSubject::Title, versions, 7, None, now, Some(publish_at));
    let video_id = workspace.create_video(cx, "Scheduled", |video| {
        video.youtube = Some(rushes_core::YoutubeLink {
            video_id: "sched".into(),
            url: "https://www.youtube.com/watch?v=sched".into(),
            channel_id: "UC1".into(),
            privacy: Privacy::Private,
            uploaded_at: now,
            thumbnail: None,
            publish_at: Some(publish_at),
            ab_test: Some(test),
        });
    });
    let runner = Runner::start(&workspace, cx, now);
    cx.run_until_parked();
    assert_eq!(test_of(&workspace, cx, video_id).starts_at, publish_at);

    // Postponed by two weeks in Visibility: the turns wait for the new time.
    let later = on("2026-11-02", 9);
    let mut video = workspace.video(cx, video_id);
    video.youtube.as_mut().unwrap().publish_at = Some(later);
    workspace.state.update(cx, |state, cx| state.update_video(video, cx));
    runner.at(cx, now + TimeDelta::minutes(1));
    let test = test_of(&workspace, cx, video_id);
    assert_eq!((test.starts_at, test.ends_at()), (later, later + TimeDelta::days(7)));

    // No longer scheduled: the test stops before it began.
    let mut video = workspace.video(cx, video_id);
    video.youtube.as_mut().unwrap().publish_at = None;
    workspace.state.update(cx, |state, cx| state.update_video(video, cx));
    runner.at(cx, now + TimeDelta::minutes(2));
    let test = test_of(&workspace, cx, video_id);
    assert!(matches!(test.outcome, Some(TestOutcome::Stopped { reason: StopReason::NotPublic, .. })));
}

#[gpui_kit::test]
fn private_videos_and_missing_permissions_say_why(cx: &mut TestAppContext) {
    let Some(emulators) = Emulators::start() else { return };
    let workspace = Workspace::open(cx, emulators.backend());
    workspace.sign_in(cx, "creator@example.com");
    let (private, _) = upload(&workspace, cx, "Private draft", Privacy::Private);
    let (video_id, _) = upload(&workspace, cx, "Public", Privacy::Public);
    let runner = Runner::start(&workspace, cx, Utc::now());
    let video = workspace.video(cx, private);
    let availability = runner.runner.read_with(cx, |runner, _| runner.availability(&video));
    assert!(availability.unwrap_err().starts_with("A/B tests need a public video"));

    // Public, but the sign-in lacks the permissions.
    let versions = vec![
        TestVersion { title: Some("One".into()), thumbnail: None },
        TestVersion { title: Some("Two".into()), thumbnail: None },
    ];
    let started = runner.runner.update(cx, |runner, cx| runner.start(video_id, TestSubject::Title, versions, 7, cx));
    let problem = started.unwrap_err();
    assert!(problem.contains("needs permission to change titles and read analytics"), "{problem}");
    assert!(workspace.video(cx, video_id).youtube.unwrap().ab_test.is_none());
}

/// Opens `build` in a window tall enough to show the whole Publish tab.
fn open<V: Render>(
    cx: &mut TestAppContext,
    build: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
) -> (AnyWindowHandle, Entity<V>) {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(Default::default(), size(px(1280.), px(2400.))))),
        ..WindowOptions::default()
    };
    cx.update(|cx| gpui_kit::open_window(options, cx, build).expect("open the test window"))
}

fn window(cx: &mut TestAppContext, handle: AnyWindowHandle, f: impl FnOnce(&mut Window, &mut App)) {
    cx.update_window(handle, |_, window, cx| f(window, cx)).unwrap();
}

/// One UI step on a settled window: dialogs done opening, the last step's effects run.
fn step(cx: &mut TestAppContext, handle: AnyWindowHandle, f: impl FnOnce(&mut Window, &mut App)) {
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    // A dialog slides in on the wall clock: a click while it moves misses its target.
    std::thread::sleep(Duration::from_millis(400));
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        f(window, cx);
    });
    cx.run_until_parked();
}

#[gpui_kit::test]
fn the_publish_tab_allows_sets_up_and_ends_a_test(cx: &mut TestAppContext) {
    let Some(emulators) = Emulators::start() else { return };
    let workspace = Workspace::open(cx, emulators.backend());
    workspace.sign_in(cx, "creator@example.com");
    let (video_id, youtube_id) = upload(&workspace, cx, "Desk setup 2026", Privacy::Public);
    let runner = Runner::start(&workspace, cx, on("2026-03-01", 10));
    let (handle, _panel) = open(cx, |window, cx| cx.new(|cx| PublishPanel::new(video_id, window, cx)));
    cx.run_until_parked();

    // The sign-in predates A/B tests: the section asks for access first.
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        let section = window.find("ab-test").bounds();
        let form = window.find("publish-title").bounds();
        assert!(section.bottom() < form.top(), "the test of the uploaded video leads");
        assert!(window.find("ab-access").visible());
        // Disabled until then.
        window.click("ab-set-up", cx);
        assert!(!window.has_active_dialog(cx));
        window.click("ab-allow", cx);
    });
    let url = wait_until(cx, "the consent page", |cx| {
        workspace.service.read_with(cx, |s, _| s.sign_in_url().map(ToString::to_string))
    });
    publish_support::consent_in_browser(&url, "creator@example.com");
    let key = workspace.account_key(cx, "creator@example.com");
    wait_until(cx, "the permissions", |cx| {
        (workspace.service.read_with(cx, |s, _| s.test_access(&key)) == TestAccess::Allowed).then_some(())
    });

    step(cx, handle, |window, cx| {
        assert!(window.try_find("ab-access").is_none());
        window.click("ab-set-up", cx);
    });
    step(cx, handle, |window, cx| {
        assert!(window.has_active_dialog(cx));
        // A starts as the video is on YouTube; B is empty, which Start says.
        assert_eq!(window.find(("ab-version-title", 0usize)).value(), Some("Desk setup 2026"));
        window.click("ab-setup-start", cx);
    });
    step(cx, handle, |window, cx| {
        assert!(window.has_active_dialog(cx));
        assert!(window.find("ab-setup-problems").visible());
        window.click(("ab-version-title", 1usize), cx);
        window.input("My desk setup for 2026, finally done", cx);
    });
    step(cx, handle, |window, cx| window.within("ab-setup-length").click(0usize, cx));
    step(cx, handle, |window, cx| window.click("ab-setup-start", cx));
    let test = wait_for_showings(&workspace, cx, video_id, 1);
    assert_eq!(test.days, 7);
    assert_eq!(test.versions[1].title.as_deref(), Some("My desk setup for 2026, finally done"));

    step(cx, handle, |window, cx| {
        assert!(!window.has_active_dialog(cx));
        assert!(window.find("ab-progress").visible());
        assert!(window.find(("ab-result", 0usize)).visible() && window.find(("ab-result", 1usize)).visible());
        assert!(window.find("ab-verdict").visible());
        // Ending asks which version stays.
        window.click("ab-end", cx);
    });
    step(cx, handle, |window, cx| {
        assert!(window.has_active_dialog(cx));
        window.within("ab-end-keep").click(1usize, cx);
    });
    step(cx, handle, |window, cx| window.click("ab-end-commit", cx));
    let test = wait_until(cx, "the end", |cx| Some(test_of(&workspace, cx, video_id)).filter(AbTest::is_over));
    assert!(matches!(test.outcome, Some(TestOutcome::Finished { kept: 1, chosen: true, .. })), "{:?}", test.outcome);
    assert_eq!(title_on_youtube(&emulators, &youtube_id), "My desk setup for 2026, finally done");
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.find("ab-outcome").visible());
        assert_eq!(window.find("ab-set-up").label(), Some("New test…"));
    });
    let _ = runner;
}
