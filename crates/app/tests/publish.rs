//! `YoutubeService` end to end against the local Google + YouTube emulators
//! (`emulators/`): sign-in, uploads with cancel and resume, recovery of interrupted
//! jobs, backend switches. Every test starts its own emulator pair on free ports and
//! is skipped with a notice when Node or `emulators/node_modules` is missing.

mod publish_support;

use std::cell::RefCell;
use std::io::Write as _;
use std::rc::Rc;
use std::time::{Duration, Instant};

use chrono::{Local, Utc};
use gpui_kit::{Entity, TestAppContext};
use publish_support::{Emulators, Workspace, wait_until};
use serde_json::json;
use videocrm::publish::BlockingReason;
use videocrm::publish::{
    Connection, INTERRUPTED, INTERRUPTED_RESUMABLE, ProcessingOutcome, Retired, UploadError, YoutubeEvent,
    YoutubeService, delete_upload_notice,
};
use videocrm::state::{AppEvent, SAVE_RETRY_DELAY};
use videocrm_core::settings::YoutubeBackend;
use videocrm_core::{Privacy, PublishChannel, UploadId, UploadJob, UploadStatus, VideoId, VideoStatus};
use videocrm_youtube::{Account, CredentialStore, Credentials, SavedLogin};

#[gpui_kit::test]
fn connects_through_the_browser_and_keeps_the_account(cx: &mut TestAppContext) {
    let Some(emulators) = Emulators::start() else { return };
    let workspace = Workspace::open(cx, emulators.backend());
    let service = workspace.service.clone();

    service.update(cx, |service, cx| service.connect(cx));
    let url = wait_until(cx, "the consent page", |cx| match service.read_with(cx, |s, _| s.connection().clone()) {
        Connection::Connecting { authorize_url: Some(url) } => Some(url.to_string()),
        _ => None,
    });
    assert_eq!(cx.opened_url().as_deref(), Some(url.as_str()), "the consent page opens in the browser");

    let page = publish_support::consent_in_browser(&url, "creator@example.com");
    assert!(page.contains("signed in"), "{page}");
    let account = wait_until(cx, "the connection", |cx| service.read_with(cx, |s, _| s.account().cloned()));
    assert_eq!(account.email, "creator@example.com");
    assert_eq!(account.channel_title.as_deref(), Some("Demo Creator"));
    assert_eq!(account.channel_handle.as_deref(), Some("@creator"));
    assert!(service.read_with(cx, |s, _| s.is_connected() && s.client().is_some()));

    let [(path, saved)] = workspace.saved_logins().try_into().expect("one saved sign-in");
    assert_eq!(saved.account.as_ref(), Some(&account));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // The channel picture is cached for the account badge.
    let avatar = wait_until(cx, "the avatar", |cx| service.read_with(cx, |s, _| s.avatar().map(ToOwned::to_owned)));
    assert!(avatar.starts_with(workspace.dir.path()) && avatar.is_file());

    // A restart finds the account again without a sign-in.
    let restarted = workspace.restart(cx);
    assert_eq!(restarted.read_with(cx, |s, _| s.account().cloned()), Some(account.clone()));

    restarted.update(cx, |service, cx| service.disconnect(account.key(), cx));
    cx.run_until_parked();
    assert_eq!(restarted.read_with(cx, |s, _| s.connection().clone()), Connection::Disconnected);
    assert!(workspace.saved_logins().is_empty());
}

#[gpui_kit::test]
fn cancelling_the_sign_in_stops_waiting(cx: &mut TestAppContext) {
    let Some(emulators) = Emulators::start() else { return };
    let workspace = Workspace::open(cx, emulators.backend());
    let service = workspace.service.clone();

    service.update(cx, |service, cx| service.connect(cx));
    let url = wait_until(cx, "the consent page", |cx| match service.read_with(cx, |s, _| s.connection().clone()) {
        Connection::Connecting { authorize_url: Some(url) } => Some(url.to_string()),
        _ => None,
    });
    service.update(cx, |service, cx| service.cancel(cx));
    assert_eq!(service.read_with(cx, |s, _| s.connection().clone()), Connection::Disconnected);
    // The loopback server is gone: finishing the consent now reaches nobody.
    std::thread::sleep(Duration::from_millis(200));
    assert!(publish_support::try_consent_in_browser(&url, "creator@example.com").is_err());
    cx.run_until_parked();
    assert_eq!(service.read_with(cx, |s, _| s.connection().clone()), Connection::Disconnected);
}

#[gpui_kit::test]
fn uploads_a_video_and_follows_processing(cx: &mut TestAppContext) {
    let Some(emulators) = Emulators::start() else { return };
    let workspace = Workspace::open(cx, emulators.backend());
    workspace.sign_in(cx, "creator@example.com");
    let service = workspace.service.clone();

    let file = publish_support::video_file(workspace.dir.path(), "final.mp4");
    let thumbnail = publish_support::png_file(workspace.dir.path(), "thumbnail.png");
    let video_id = workspace.create_video(cx, "Rust ownership in 60 seconds", |video| {
        video.publish.description = "One owner, many borrowers.".into();
        video.publish.tags = vec!["rust".into(), "ownership".into()];
        video.publish.category_id = "28".into();
        video.publish.privacy = Privacy::Public;
        video.publish.file = Some(file.clone());
        video.publish.thumbnail = Some(thumbnail);
    });

    let upload = service.update(cx, |service, cx| service.start_upload(video_id, cx)).unwrap();
    assert_eq!(
        service.update(cx, |service, cx| service.start_upload(video_id, cx)),
        Err(UploadError::AlreadyUploading)
    );
    let youtube_id = wait_until(cx, "the upload", |cx| {
        // Processing is polled every five seconds of app time.
        cx.executor().advance_clock(Duration::from_secs(5));
        match service.read_with(cx, |s, _| s.job(upload).map(|job| job.status.clone())) {
            Some(UploadStatus::Done { video_id }) => Some(video_id),
            Some(UploadStatus::Failed { error }) => panic!("upload failed: {error}"),
            _ => None,
        }
    });
    assert_eq!(service.read_with(cx, |s, _| s.processing(upload)), Some(ProcessingOutcome::Ready));
    assert!(!service.read_with(cx, |s, _| s.is_active(upload)));

    // The job is stored, the video links to YouTube and counts as published.
    let stored = workspace.stored_jobs(cx);
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].status, UploadStatus::Done { video_id: youtube_id.clone() });
    assert!(stored[0].session_url.is_some());
    let video = workspace.video(cx, video_id);
    let link = video.youtube.expect("the video links to YouTube");
    assert_eq!(link.video_id, youtube_id);
    assert_eq!(link.privacy, Privacy::Public);
    assert_eq!(video.status, VideoStatus::Published);
    let row = workspace.state.read_with(cx, |state, _| state.row(video_id).cloned()).unwrap();
    assert_eq!(row.latest_upload, Some(UploadStatus::Done { video_id: youtube_id.clone() }));

    // YouTube has exactly the file's bytes and the form's metadata.
    let uploads = emulators.uploads();
    let received = uploads.iter().find(|upload| upload["videoId"] == youtube_id.as_str()).expect("upload on YouTube");
    assert_eq!(received["bytes"], std::fs::metadata(&file).unwrap().len());
    assert_eq!(received["sha256"], publish_support::sha256(&file));
    let snippet = &received["snippet"];
    assert_eq!(snippet["title"], "Rust ownership in 60 seconds");
    assert_eq!(snippet["tags"], json!(["rust", "ownership"]));
    assert_eq!(snippet["categoryId"], "28");
    assert_eq!(received["status"]["privacyStatus"], "public");

    // The account is read again: the channel has a public video now.
    let videos = wait_until(cx, "the account refresh", |cx| {
        service.read_with(cx, |s, _| s.account().and_then(|account| account.video_count)).filter(|count| *count > 0)
    });
    assert_eq!(videos, 1);
    let saved = workspace.saved_logins().pop().unwrap().1;
    assert_eq!(saved.account.and_then(|account| account.video_count), Some(1));

    // The custom thumbnail was set after the upload.
    let client = service.read_with(cx, |s, _| s.client()).unwrap();
    let info = client.get_video(&youtube_id).unwrap().unwrap();
    assert!(info.thumbnail.is_some_and(|url| url.contains("?v=")), "custom thumbnail");
}

#[gpui_kit::test]
fn a_scheduled_private_upload_marks_the_video_scheduled(cx: &mut TestAppContext) {
    let Some(emulators) = Emulators::start() else { return };
    let workspace = Workspace::open(cx, emulators.backend());
    workspace.sign_in(cx, "creator@example.com");
    let service = workspace.service.clone();
    let file = publish_support::video_file(workspace.dir.path(), "final.mp4");
    let publish_at = Utc::now() + chrono::TimeDelta::days(2);
    let video_id = workspace.create_video(cx, "Desk setup 2026", |video| {
        video.publish.file = Some(file.clone());
        video.publish.privacy = Privacy::Private;
        video.publish.publish_at = Some(publish_at);
        // The planner had another day in mind.
        video.publish_date = Some(Local::now().date_naive());
    });

    let upload = service.update(cx, |service, cx| service.start_upload(video_id, cx)).unwrap();
    wait_until(cx, "the upload", |cx| {
        let status = service.read_with(cx, |s, _| s.job(upload).map(|job| job.status.clone()));
        matches!(status, Some(UploadStatus::Processing { .. } | UploadStatus::Done { .. })).then_some(())
    });
    let video = workspace.video(cx, video_id);
    assert_eq!(video.status, VideoStatus::Scheduled);
    // The schedule YouTube got is the publish date the planner shows.
    assert_eq!(video.publish_date, Some(publish_at.with_timezone(&Local).date_naive()));
    let received = emulators.uploads();
    assert_eq!(received[0]["status"]["privacyStatus"], "private");
    assert!(received[0]["status"]["publishAt"].is_string());
}

#[gpui_kit::test]
fn a_cancelled_upload_resumes_where_youtube_stopped(cx: &mut TestAppContext) {
    let Some(emulators) = Emulators::start() else { return };
    let workspace = Workspace::open(cx, emulators.backend());
    workspace.sign_in(cx, "creator@example.com");
    let service = workspace.service.clone();
    let file = publish_support::video_file(workspace.dir.path(), "final.mp4");
    let video_id = workspace.create_video(cx, "Keychron Q1 Max review", |video| {
        video.publish.file = Some(file.clone());
        video.publish.privacy = Privacy::Private;
    });

    // Every chunk fails, so the worker sits in its retry backoff.
    emulators.inject_fault(json!({"method": "PUT", "path": "/upload/youtube/v3/videos", "status": 503, "times": 1000}));
    let upload = service.update(cx, |service, cx| service.start_upload(video_id, cx)).unwrap();
    wait_until(cx, "the upload session", |cx| {
        service.read_with(cx, |s, _| s.job(upload).and_then(|job| job.session_url.clone()))
    });
    // The whole file went out in one chunk and YouTube kept none of it: the
    // progress is what YouTube holds, not the bytes that were in flight, from
    // the first retry on.
    wait_until(cx, "the first retry", |cx| service.read_with(cx, |s, _| s.retry(upload)));
    assert_eq!(service.read_with(cx, |s, _| s.progress(upload)), Some(0.0));
    wait_until(cx, "the second retry", |cx| {
        service.read_with(cx, |s, _| s.retry(upload).filter(|retry| retry.attempt >= 2))
    });
    assert_eq!(service.read_with(cx, |s, _| s.progress(upload)), Some(0.0));
    service.update(cx, |service, cx| service.cancel_upload(upload, cx));
    assert!(service.read_with(cx, |s, _| s.is_cancelling(upload)));
    let job = wait_until(cx, "the cancellation", |cx| {
        service.read_with(cx, |s, _| s.job(upload).filter(|job| job.status == UploadStatus::Cancelled).cloned())
    });
    assert!(job.session_url.is_some(), "a cancelled upload keeps its session");
    assert!(service.read_with(cx, |s, _| s.is_resumable(&job)));
    assert!(emulators.uploads().is_empty());

    emulators.clear_faults();
    service.update(cx, |service, cx| service.resume_upload(upload, cx)).unwrap();
    wait_until(cx, "the resumed upload", |cx| {
        let status = service.read_with(cx, |s, _| s.job(upload).map(|job| job.status.clone()));
        matches!(status, Some(UploadStatus::Processing { .. } | UploadStatus::Done { .. })).then_some(())
    });
    let received = emulators.uploads();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0]["sha256"], publish_support::sha256(&file));
    assert_eq!(service.update(cx, |service, cx| service.resume_upload(upload, cx)), Err(UploadError::NotResumable));
}

#[gpui_kit::test]
fn an_upload_goes_on_while_another_program_locks_the_database(cx: &mut TestAppContext) {
    let Some(emulators) = Emulators::start() else { return };
    let workspace = Workspace::open(cx, emulators.backend());
    workspace.sign_in(cx, "creator@example.com");
    let service = workspace.service.clone();
    let file = publish_support::video_file(workspace.dir.path(), "final.mp4");
    let video_id = workspace.create_video(cx, "Keychron Q1 Max review", |video| {
        video.publish.file = Some(file.clone());
        video.publish.privacy = Privacy::Private;
    });
    // Every state the upload goes through, as the service shows it.
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();
    cx.update(|cx| {
        cx.subscribe(&service, move |service, event: &YoutubeEvent, cx| {
            if *event == YoutubeEvent::UploadsChanged
                && let Some(job) = service.read(cx).latest_job(video_id)
            {
                sink.borrow_mut().push(job.status.clone());
            }
        })
        .detach()
    });

    // Another program holds the write lock, as a backup or sync tool does.
    let database = workspace.dir.path().join("videocrm.db");
    let lock = rusqlite::Connection::open(&database).unwrap();
    lock.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let started = Instant::now();
    let upload = service.update(cx, |service, cx| service.start_upload(video_id, cx)).expect("the upload starts");
    assert!(started.elapsed() < Duration::from_secs(1), "starting took {:?}", started.elapsed());
    let youtube_id = wait_until(cx, "the upload", |cx| {
        match service.read_with(cx, |s, _| s.job(upload).map(|job| job.status.clone())) {
            Some(UploadStatus::Processing { video_id }) => Some(video_id),
            Some(UploadStatus::Failed { error }) => panic!("upload failed: {error}"),
            _ => None,
        }
    });
    assert!(seen.borrow().iter().any(|status| matches!(status, UploadStatus::Uploading { .. })), "{seen:?}");
    assert_eq!(emulators.uploads().len(), 1, "the video reached YouTube");
    assert!(workspace.stored_jobs(cx).is_empty(), "nothing is written during the lock");

    // The lock ends: the background writer's next attempt writes the job as the
    // service shows it, and the video's link.
    lock.execute_batch("COMMIT").unwrap();
    wait_until(cx, "the saved upload", |cx| {
        cx.executor().advance_clock(SAVE_RETRY_DELAY);
        (!workspace.state.read_with(cx, |state, _| state.has_unsaved_changes())).then_some(())
    });
    let shown = service.read_with(cx, |s, _| s.jobs().to_vec());
    assert_eq!(workspace.stored_jobs(cx), shown);
    let store = workspace.state.read_with(cx, |state, _| state.store());
    let link = store.lock().unwrap().get_video(video_id).unwrap().and_then(|video| video.youtube);
    assert_eq!(link.map(|link| link.video_id), Some(youtube_id));
}

/// Starts an upload of `video_id` that YouTube keeps failing, then cancels it once
/// the session is open. Returns the cancelled job.
fn cancel_a_failing_upload(
    cx: &mut TestAppContext,
    emulators: &Emulators,
    service: &Entity<YoutubeService>,
    video_id: VideoId,
) -> UploadJob {
    emulators.inject_fault(json!({"method": "PUT", "path": "/upload/youtube/v3/videos", "status": 503, "times": 1000}));
    let upload = service.update(cx, |service, cx| service.start_upload(video_id, cx)).unwrap();
    wait_until(cx, "the upload session", |cx| {
        service.read_with(cx, |s, _| s.job(upload).and_then(|job| job.session_url.clone()))
    });
    service.update(cx, |service, cx| service.cancel_upload(upload, cx));
    let job = wait_until(cx, "the cancellation", |cx| {
        service.read_with(cx, |s, _| s.job(upload).filter(|job| job.status == UploadStatus::Cancelled).cloned())
    });
    assert!(service.read_with(cx, |s, _| s.is_resumable(&job)));
    emulators.clear_faults();
    job
}

#[gpui_kit::test]
fn a_stopped_upload_resumes_only_the_file_it_started_with(cx: &mut TestAppContext) {
    let Some(emulators) = Emulators::start() else { return };
    let workspace = Workspace::open(cx, emulators.backend());
    workspace.sign_in(cx, "creator@example.com");
    let service = workspace.service.clone();
    let take = publish_support::video_file(workspace.dir.path(), "big-take.mp4");
    let cut = workspace.dir.path().join("real-final.mp4");
    std::fs::write(&cut, vec![7u8; 200_000]).unwrap();
    let video_id = workspace.create_video(cx, "Why Every Developer Should Learn SQLite", |video| {
        video.publish.file = Some(take.clone());
    });
    let job = cancel_a_failing_upload(cx, &emulators, &service, video_id);
    assert_eq!(job.file_stamp.map(|stamp| stamp.size), Some(std::fs::metadata(&take).unwrap().len()));

    // AI edit's "Use for upload" picks the newer cut.
    workspace.state.update(cx, |state, cx| {
        let mut video = state.video(video_id).unwrap().clone();
        video.publish.file = Some(cut.clone());
        state.update_video(video, cx);
    });
    cx.run_until_parked();
    let job = service.read_with(cx, |s, _| s.job(job.id).cloned()).unwrap();
    assert_eq!(job.session_url, None, "the session of the other file is dropped");
    assert!(!service.read_with(cx, |s, _| s.is_resumable(&job)));
    assert_eq!(service.read_with(cx, |s, _| s.retired(job.id)), Some(Retired::OtherFile));
    assert_eq!(service.update(cx, |service, cx| service.resume_upload(job.id, cx)), Err(UploadError::NotResumable));

    // A fresh upload sends the chosen cut, and only that reaches YouTube.
    let upload = service.update(cx, |service, cx| service.start_upload(video_id, cx)).unwrap();
    wait_until(cx, "the upload", |cx| {
        let status = service.read_with(cx, |s, _| s.job(upload).map(|job| job.status.clone()));
        matches!(status, Some(UploadStatus::Processing { .. } | UploadStatus::Done { .. })).then_some(())
    });
    let received = emulators.uploads();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0]["sha256"], publish_support::sha256(&cut));
}

#[gpui_kit::test]
fn a_file_changed_after_the_upload_stopped_starts_over(cx: &mut TestAppContext) {
    let Some(emulators) = Emulators::start() else { return };
    let workspace = Workspace::open(cx, emulators.backend());
    workspace.sign_in(cx, "creator@example.com");
    let service = workspace.service.clone();
    let file = publish_support::video_file(workspace.dir.path(), "final.mp4");
    let video_id = workspace.create_video(cx, "Keychron Q1 Max review", |video| {
        video.publish.file = Some(file.clone());
    });

    // Re-exported with a different length: Resume refuses before YouTube splices two files.
    let job = cancel_a_failing_upload(cx, &emulators, &service, video_id);
    std::fs::OpenOptions::new().append(true).open(&file).unwrap().write_all(&[0; 1000]).unwrap();
    assert_eq!(
        service.update(cx, |service, cx| service.resume_upload(job.id, cx)),
        Err(UploadError::Stale(Retired::FileChanged))
    );
    let stored = workspace.stored_jobs(cx).into_iter().find(|stored| stored.id == job.id).unwrap();
    assert_eq!(stored.session_url, None);
    assert!(!service.read_with(cx, |s, _| s.is_resumable(&stored)));

    // Same size, other bytes: the modification time tells. Coming back to the
    // app finds it without a Resume attempt.
    let job = cancel_a_failing_upload(cx, &emulators, &service, video_id);
    let mut bytes = std::fs::read(&file).unwrap();
    bytes[0] ^= 0xff;
    std::fs::write(&file, &bytes).unwrap();
    let later = std::time::SystemTime::now() + Duration::from_secs(60);
    std::fs::File::options().write(true).open(&file).unwrap().set_modified(later).unwrap();
    service.update(cx, |service, cx| service.check_stopped_uploads(cx));
    wait_until(cx, "the changed file to be noticed", |cx| {
        service.read_with(cx, |s, _| (s.retired(job.id) == Some(Retired::FileChanged)).then_some(()))
    });
    assert!(!service.read_with(cx, |s, _| s.is_resumable(s.job(job.id).unwrap())));
    assert!(emulators.uploads().is_empty(), "nothing spliced reached YouTube");
}

#[gpui_kit::test]
fn a_session_youtube_refuses_on_resume_is_dropped(cx: &mut TestAppContext) {
    let Some(emulators) = Emulators::start() else { return };
    let workspace = Workspace::open(cx, emulators.backend());
    workspace.sign_in(cx, "creator@example.com");
    let file = publish_support::video_file(workspace.dir.path(), "final.mp4");
    let video_id = workspace.create_video(cx, "Keychron Q1 Max review", |video| {
        video.publish.file = Some(file.clone());
    });
    let job = cancel_a_failing_upload(cx, &emulators, &workspace.service, video_id);
    // A job stored before file stamps existed, whose file then grew: only YouTube can tell.
    let store = workspace.state.read_with(cx, |state, _| state.store());
    let mut legacy = job.clone();
    legacy.file_stamp = None;
    store.lock().unwrap().update_upload(&mut legacy).unwrap();
    std::fs::OpenOptions::new().append(true).open(&file).unwrap().write_all(&[0; 1000]).unwrap();
    let service = workspace.restart(cx);

    service.update(cx, |service, cx| service.resume_upload(job.id, cx)).unwrap();
    let failed = wait_until(cx, "the refused resume", |cx| {
        service.read_with(cx, |s, _| {
            s.job(job.id).filter(|job| matches!(job.status, UploadStatus::Failed { .. })).cloned()
        })
    });
    // 400 badContent: said plainly, and Resume is not offered again.
    assert_eq!(
        failed.status,
        UploadStatus::Failed {
            error: "YouTube can’t continue this upload. Upload the video again to send it from the beginning.".into()
        }
    );
    assert_eq!(failed.session_url, None);
    assert!(!service.read_with(cx, |s, _| s.is_resumable(&failed)));
}

#[gpui_kit::test]
fn starting_over_retires_the_stopped_upload(cx: &mut TestAppContext) {
    let Some(emulators) = Emulators::start() else { return };
    let workspace = Workspace::open(cx, emulators.backend());
    workspace.sign_in(cx, "creator@example.com");
    let service = workspace.service.clone();
    let file = publish_support::video_file(workspace.dir.path(), "final.mp4");
    let video_id = workspace.create_video(cx, "Stop Using print() to Debug Python", |video| {
        video.publish.file = Some(file.clone());
    });
    let stopped = cancel_a_failing_upload(cx, &emulators, &service, video_id);

    // "Start over" while YouTube still fails, so the new upload keeps running.
    emulators.inject_fault(json!({"method": "PUT", "path": "/upload/youtube/v3/videos", "status": 503, "times": 1000}));
    let fresh = service.update(cx, |service, cx| service.start_upload(video_id, cx)).unwrap();
    let stopped = service.read_with(cx, |s, _| s.job(stopped.id).cloned()).unwrap();
    assert_eq!(stopped.session_url, None, "the abandoned session can’t send a second copy");
    assert_eq!(service.read_with(cx, |s, _| s.retired(stopped.id)), Some(Retired::Replaced));
    assert!(!service.read_with(cx, |s, _| s.is_resumable(&stopped)));
    // Like starting, resuming refuses while the video uploads.
    assert_eq!(
        service.update(cx, |service, cx| service.resume_upload(stopped.id, cx)),
        Err(UploadError::AlreadyUploading)
    );

    emulators.clear_faults();
    wait_until(cx, "the new upload", |cx| {
        let status = service.read_with(cx, |s, _| s.job(fresh).map(|job| job.status.clone()));
        matches!(status, Some(UploadStatus::Processing { .. } | UploadStatus::Done { .. })).then_some(())
    });
    assert_eq!(emulators.uploads().len(), 1, "one copy on YouTube");
}

#[gpui_kit::test]
fn a_session_the_connection_cannot_use_is_not_offered_again(cx: &mut TestAppContext) {
    let Some(emulators) = Emulators::start() else { return };
    let workspace = Workspace::open(cx, emulators.backend());
    workspace.sign_in(cx, "creator@example.com");
    let file = publish_support::video_file(workspace.dir.path(), "final.mp4");
    let video_id = workspace.create_video(cx, "Keychron Q1 Max review", |video| {
        video.publish.file = Some(file.clone());
    });
    // Cancelled while VideoCRM talked to another YouTube server, before a backend switch.
    let job = UploadJob {
        id: UploadId::new(),
        video_id,
        file,
        file_stamp: None,
        status: UploadStatus::Cancelled,
        session_url: Some("http://127.0.0.1:1/upload/youtube/v3/videos?uploadType=resumable&upload_id=old".into()),
        channel: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    workspace.insert_jobs(cx, &[&job]);
    let service = workspace.restart(cx);
    assert!(service.read_with(cx, |s, _| s.is_connected() && s.is_resumable(s.job(job.id).unwrap())));

    service.update(cx, |service, cx| service.resume_upload(job.id, cx)).unwrap();
    let failed = wait_until(cx, "the refused resume", |cx| {
        service.read_with(cx, |s, _| {
            s.job(job.id).filter(|job| matches!(job.status, UploadStatus::Failed { .. })).cloned()
        })
    });
    assert_eq!(failed.session_url, None, "the session is dropped");
    assert!(!service.read_with(cx, |s, _| s.is_resumable(&failed)), "Resume is not offered again");
    assert_eq!(workspace.stored_jobs(cx)[0].session_url, None);
    assert!(emulators.uploads().is_empty());
}

#[gpui_kit::test]
fn refuses_uploads_that_youtube_would_reject(cx: &mut TestAppContext) {
    let workspace = Workspace::open(cx, YoutubeBackend::default());
    let service = workspace.service.clone();
    let video_id = workspace.create_video(cx, "Tokyo <tech> stores", |video| {
        video.publish.privacy = Privacy::Public;
        video.publish.publish_at = Some(Utc::now() - chrono::TimeDelta::minutes(1));
    });
    let Err(UploadError::Blocked(reasons)) = service.update(cx, |service, cx| service.start_upload(video_id, cx))
    else {
        panic!("the upload must not start");
    };
    let messages: Vec<String> = reasons.iter().map(ToString::to_string).collect();
    assert_eq!(
        messages,
        [
            "Connect a YouTube account.",
            "Choose the video file to upload.",
            "Remove < and > from the title.",
            "Scheduled videos must stay private until they go live.",
            "Pick a publish time in the future.",
        ]
    );
    assert!(service.read_with(cx, |s, _| s.jobs().is_empty()));
}

#[gpui_kit::test]
fn interrupted_uploads_become_resumable_failures_at_startup(cx: &mut TestAppContext) {
    let workspace = Workspace::open(cx, YoutubeBackend::default());
    // Resume continues the file the video still has chosen.
    let file = workspace.dir.path().join("final.mp4");
    let video_id =
        workspace.create_video(cx, "Why I left macOS for Linux", |video| video.publish.file = Some(file.clone()));
    let job = |status: UploadStatus, session_url: Option<&str>| UploadJob {
        id: UploadId::new(),
        video_id,
        file: workspace.dir.path().join("final.mp4"),
        file_stamp: None,
        status,
        session_url: session_url.map(str::to_owned),
        channel: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let mid_transfer = job(UploadStatus::Uploading { sent: 10, total: 100 }, Some("http://127.0.0.1:1/upload/session"));
    let not_started = job(UploadStatus::Pending, None);
    let processing = job(UploadStatus::Processing { video_id: "abc".into() }, Some("http://127.0.0.1:1/s"));
    let done = job(UploadStatus::Done { video_id: "def".into() }, None);
    workspace.insert_jobs(cx, &[&mid_transfer, &not_started, &processing, &done]);

    let service = workspace.restart(cx);
    let status =
        |id: UploadId, cx: &mut TestAppContext| service.read_with(cx, |s, _| s.job(id).unwrap().status.clone());
    assert_eq!(status(mid_transfer.id, cx), UploadStatus::Failed { error: INTERRUPTED_RESUMABLE.into() });
    assert_eq!(status(not_started.id, cx), UploadStatus::Failed { error: INTERRUPTED.into() });
    assert_eq!(status(processing.id, cx), UploadStatus::Done { video_id: "abc".into() });
    assert_eq!(status(done.id, cx), UploadStatus::Done { video_id: "def".into() });
    let resumable =
        |id: UploadId, cx: &mut TestAppContext| service.read_with(cx, |s, _| s.is_resumable(s.job(id).unwrap()));
    assert!(resumable(mid_transfer.id, cx));
    assert!(!resumable(not_started.id, cx));
    // The change is stored, not just shown.
    let stored = workspace.stored_jobs(cx);
    assert!(stored.iter().all(|job| !matches!(job.status, UploadStatus::Pending | UploadStatus::Uploading { .. })));
}

#[gpui_kit::test]
fn switching_the_backend_signs_out(cx: &mut TestAppContext) {
    let emulator = YoutubeBackend::Emulator { google_url: String::new(), youtube_url: String::new() };
    let workspace = Workspace::open(cx, emulator);
    let config = workspace.service.read_with(cx, |s, _| s.config().ok().cloned()).unwrap();
    let account = Account {
        email: "creator@example.com".into(),
        name: Some("Demo Creator".into()),
        picture: None,
        channel_id: Some("UC1".into()),
        channel_title: Some("Demo Creator".into()),
        channel_handle: None,
        channel_thumbnail: None,
        subscriber_count: Some(3),
        video_count: Some(1),
        backend: config.backend_label(),
    };
    let credentials = Credentials {
        access_token: "google_token".into(),
        refresh_token: Some("refresh".into()),
        expires_at: Utc::now() + chrono::TimeDelta::hours(1),
        scope: "openid".into(),
        id_token: None,
    };
    // Saved by a release that kept one account: it moves next to the others.
    let path = workspace.dir.path().join("credentials.json");
    CredentialStore::new(&path).save(&SavedLogin { credentials, account: Some(account.clone()) }).unwrap();
    let service = workspace.restart(cx);
    assert_eq!(service.read_with(cx, |s, _| s.account().cloned()), Some(account));
    assert!(!path.exists());
    assert_eq!(workspace.saved_logins().len(), 1);

    workspace.state.update(cx, |state, cx| {
        state.update_settings(cx, |settings| {
            settings.youtube = YoutubeBackend::Google { client_id: "id".into(), client_secret: String::new() }
        })
    });
    cx.run_until_parked();
    assert_eq!(service.read_with(cx, |s, _| s.connection().clone()), Connection::Disconnected);
    assert!(workspace.saved_logins().is_empty(), "the other backend’s tokens are forgotten");
    let problem = service.read_with(cx, |s, _| s.config().err().cloned()).unwrap();
    assert_eq!(problem.as_ref(), "Add the client ID and client secret of your Google OAuth client.");

    service.update(cx, |service, cx| service.connect(cx));
    assert!(matches!(service.read_with(cx, |s, _| s.connection().clone()), Connection::Failed(_)));
}

#[gpui_kit::test]
fn deleting_a_video_cancels_its_upload_without_errors(cx: &mut TestAppContext) {
    let Some(emulators) = Emulators::start() else { return };
    let workspace = Workspace::open(cx, emulators.backend());
    workspace.sign_in(cx, "creator@example.com");
    let errors: Rc<RefCell<Vec<String>>> = Rc::default();
    let sink = errors.clone();
    let _subscription = cx.update(|cx| {
        cx.subscribe(&workspace.state, move |_, event: &AppEvent, _| {
            if let AppEvent::Error(message) = event {
                sink.borrow_mut().push(message.to_string());
            }
        })
    });
    let service = workspace.service.clone();
    let file = publish_support::video_file(workspace.dir.path(), "final.mp4");
    let video_id = workspace.create_video(cx, "Keychron Q1 Max review", |video| {
        video.publish.file = Some(file.clone());
        video.publish.privacy = Privacy::Private;
    });
    let other = workspace.create_video(cx, "Desk setup 2026", |_| {});
    // Every chunk fails, so the upload is still running when the video goes.
    emulators.inject_fault(json!({"method": "PUT", "path": "/upload/youtube/v3/videos", "status": 503, "times": 1000}));
    let upload = service.update(cx, |service, cx| service.start_upload(video_id, cx)).unwrap();
    wait_until(cx, "the upload session", |cx| {
        service.read_with(cx, |s, _| s.job(upload).and_then(|job| job.session_url.clone()))
    });
    // The delete confirmation says what happens to the upload.
    cx.update(|cx| {
        assert!(delete_upload_notice(&[video_id], cx).is_some_and(|notice| notice.contains("cancels the upload")));
        assert_eq!(delete_upload_notice(&[other], cx), None);
    });

    workspace.state.update(cx, |state, cx| state.delete_videos(&[video_id], cx));
    cx.run_until_parked();
    assert!(service.read_with(cx, |s, _| s.is_cancelling(upload)), "deleting the video stops its upload");
    assert_eq!(service.read_with(cx, |s, _| s.latest_job(video_id).cloned()), None, "the job went with the video");
    wait_until(cx, "the worker to stop", |cx| service.read_with(cx, |s, _| (!s.is_active(upload)).then_some(())));
    emulators.clear_faults();
    assert!(errors.borrow().is_empty(), "{:?}", errors.borrow());
    assert!(emulators.uploads().is_empty(), "nothing reached YouTube");
    assert!(workspace.stored_jobs(cx).is_empty());
}

#[gpui_kit::test]
fn the_uploads_of_a_deleted_video_leave_the_list(cx: &mut TestAppContext) {
    let workspace = Workspace::open(cx, YoutubeBackend::default());
    let video_id = workspace.create_video(cx, "Old upload", |_| {});
    let done = UploadJob {
        id: UploadId::new(),
        video_id,
        file: workspace.dir.path().join("final.mp4"),
        file_stamp: None,
        status: UploadStatus::Done { video_id: "abc".into() },
        session_url: None,
        channel: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    workspace.insert_jobs(cx, &[&done]);
    let service = workspace.restart(cx);
    assert_eq!(service.read_with(cx, |s, _| s.jobs().len()), 1);
    workspace.state.update(cx, |state, cx| state.delete_videos(&[video_id], cx));
    cx.run_until_parked();
    assert!(service.read_with(cx, |s, _| s.jobs().is_empty()), "the Uploads page no longer lists it");
}

#[gpui_kit::test]
fn each_video_uploads_to_its_own_account(cx: &mut TestAppContext) {
    let Some(emulators) = Emulators::start() else { return };
    let workspace = Workspace::open(cx, emulators.backend());
    workspace.sign_in(cx, "creator@example.com");
    workspace.sign_in(cx, "editor@example.com");
    let service = workspace.service.clone();
    let creator = workspace.account_key(cx, "creator@example.com");
    let editor = workspace.account_key(cx, "editor@example.com");
    // Listed by name; the first account signed in stays the default.
    let names = service.read_with(cx, |s, _| s.accounts().iter().map(|account| account.name()).collect::<Vec<_>>());
    assert_eq!(names, ["Demo Creator", "Second Channel"]);
    let default = |cx: &mut TestAppContext| {
        service.read_with(cx, |s, _| s.default_account().map(|account| account.key().to_owned()))
    };
    assert_eq!(default(cx), Some(creator.clone()));
    assert_eq!(workspace.saved_logins().len(), 2, "one credentials file per account");

    let file = publish_support::video_file(workspace.dir.path(), "final.mp4");
    let unchosen = workspace.create_video(cx, "Desk tour", |video| video.publish.file = Some(file.clone()));
    let chosen = workspace.create_video(cx, "Behind the scenes", |video| {
        video.publish.file = Some(file.clone());
        video.publish.channel = Some(PublishChannel { id: editor.clone(), name: "Second Channel".into() });
    });
    let uploads = [unchosen, chosen]
        .map(|video_id| service.update(cx, |service, cx| service.start_upload(video_id, cx)).unwrap());
    let youtube_ids = uploads.map(|upload| {
        wait_until(cx, "the upload", |cx| {
            cx.executor().advance_clock(Duration::from_secs(5));
            match service.read_with(cx, |s, _| s.job(upload).map(|job| job.status.clone())) {
                Some(UploadStatus::Done { video_id }) => Some(video_id),
                Some(UploadStatus::Failed { error }) => panic!("upload failed: {error}"),
                _ => None,
            }
        })
    });

    // YouTube has each video on its own channel, and so do the jobs and the links.
    let received = emulators.uploads();
    let channel_of = |youtube_id: &str| {
        let upload = received.iter().find(|upload| upload["videoId"] == youtube_id).expect("upload on YouTube");
        upload["channelId"].as_str().unwrap().to_owned()
    };
    assert_eq!(channel_of(&youtube_ids[0]), creator);
    assert_eq!(channel_of(&youtube_ids[1]), editor);
    let jobs = workspace.stored_jobs(cx);
    let job_channel = |upload: UploadId| jobs.iter().find(|job| job.id == upload).and_then(|job| job.channel.clone());
    assert_eq!(job_channel(uploads[0]), Some(creator.clone()));
    assert_eq!(job_channel(uploads[1]), Some(editor.clone()));
    assert_eq!(workspace.video(cx, chosen).youtube.unwrap().channel_id, editor);
    // The video that went to the default account stays with it, whatever the default becomes.
    let unchosen_video = workspace.video(cx, unchosen);
    assert_eq!(unchosen_video.youtube.unwrap().channel_id, creator);
    assert_eq!(
        unchosen_video.publish.channel,
        Some(PublishChannel { id: creator.clone(), name: "Demo Creator".into() })
    );
    service.update(cx, |service, cx| service.set_default(&editor, cx));
    assert_eq!(default(cx), Some(editor.clone()));
    assert_eq!(service.read_with(cx, |s, cx| s.channel_for(&workspace.video_in(cx, unchosen))), Some(creator));
}

#[gpui_kit::test]
fn the_default_account_and_the_choices_survive_sign_outs(cx: &mut TestAppContext) {
    let Some(emulators) = Emulators::start() else { return };
    let workspace = Workspace::open(cx, emulators.backend());
    workspace.sign_in(cx, "creator@example.com");
    // Signing in to an account again replaces its sign-in rather than adding it twice.
    workspace.sign_in(cx, "creator@example.com");
    assert_eq!(workspace.service.read_with(cx, |s, _| s.accounts().len()), 1);
    assert_eq!(workspace.saved_logins().len(), 1);
    workspace.sign_in(cx, "editor@example.com");
    let creator = workspace.account_key(cx, "creator@example.com");
    let editor = workspace.account_key(cx, "editor@example.com");
    workspace.service.update(cx, |service, cx| service.set_default(&editor, cx));
    let file = publish_support::video_file(workspace.dir.path(), "final.mp4");
    let chosen = workspace.create_video(cx, "Behind the scenes", |video| {
        video.publish.file = Some(file.clone());
        video.publish.channel = Some(PublishChannel { id: editor.clone(), name: "Second Channel".into() });
    });
    let unchosen = workspace.create_video(cx, "Desk tour", |video| video.publish.file = Some(file.clone()));

    // A restart signs both accounts in again and keeps the default.
    let service = workspace.restart(cx);
    let default = |cx: &mut TestAppContext| {
        service.read_with(cx, |s, _| s.default_account().map(|account| account.key().to_owned()))
    };
    assert_eq!(service.read_with(cx, |s, _| s.accounts().len()), 2);
    assert_eq!(default(cx), Some(editor.clone()));

    // Disconnecting the default hands the videos without a choice to the other
    // account, by choice; the video that chose it keeps the choice and says so.
    service.update(cx, |service, cx| service.disconnect(&editor, cx));
    cx.run_until_parked();
    assert_eq!(default(cx), Some(creator.clone()));
    assert_eq!(workspace.saved_logins().len(), 1);
    assert_eq!(
        service.read_with(cx, |s, cx| s.blocking_reasons(chosen, cx)),
        [BlockingReason::AccountGone("Second Channel".into())]
    );
    assert!(matches!(
        service.update(cx, |service, cx| service.start_upload(chosen, cx)),
        Err(UploadError::Blocked(reasons)) if reasons == [BlockingReason::AccountGone("Second Channel".into())]
    ));
    assert_eq!(service.read_with(cx, |s, cx| s.blocking_reasons(unchosen, cx)), []);
    let restarted = workspace.restart(cx);
    assert_eq!(
        restarted.read_with(cx, |s, _| s.default_account().map(|account| account.key().to_owned())),
        Some(creator)
    );
}

#[gpui_kit::test]
fn stopped_uploads_from_the_one_account_release_resume_with_that_account(cx: &mut TestAppContext) {
    let emulator = YoutubeBackend::Emulator { google_url: String::new(), youtube_url: String::new() };
    let workspace = Workspace::open(cx, emulator);
    let config = workspace.service.read_with(cx, |s, _| s.config().ok().cloned()).unwrap();
    let account = Account {
        email: "creator@example.com".into(),
        name: Some("Demo Creator".into()),
        picture: None,
        channel_id: Some("UC1".into()),
        channel_title: Some("Demo Creator".into()),
        channel_handle: None,
        channel_thumbnail: None,
        subscriber_count: None,
        video_count: None,
        backend: config.backend_label(),
    };
    let credentials = Credentials {
        access_token: "google_token".into(),
        refresh_token: Some("refresh".into()),
        expires_at: Utc::now() + chrono::TimeDelta::hours(1),
        scope: "openid".into(),
        id_token: None,
    };
    let path = workspace.dir.path().join("credentials.json");
    CredentialStore::new(&path).save(&SavedLogin { credentials, account: Some(account) }).unwrap();
    let file = publish_support::video_file(workspace.dir.path(), "final.mp4");
    let video_id = workspace.create_video(cx, "Desk tour", |video| video.publish.file = Some(file.clone()));
    let job = |status: UploadStatus, session_url: Option<&str>| UploadJob {
        id: UploadId::new(),
        video_id,
        file: file.clone(),
        file_stamp: None,
        status,
        session_url: session_url.map(str::to_owned),
        channel: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let stopped = job(UploadStatus::Uploading { sent: 10, total: 100 }, Some("http://127.0.0.1:1/upload/session"));
    let done = job(UploadStatus::Done { video_id: "abc".into() }, None);
    workspace.insert_jobs(cx, &[&stopped, &done]);

    // The release kept one account, so the stopped upload went to it: it stays
    // with it, whichever account becomes the default later.
    let service = workspace.restart(cx);
    cx.run_until_parked();
    let channel = |id: UploadId, cx: &mut TestAppContext| {
        service.read_with(cx, |s, _| s.job(id).and_then(|job| job.channel.clone()))
    };
    assert_eq!(channel(stopped.id, cx).as_deref(), Some("UC1"));
    // Finished uploads are left alone: who knows which account they went to.
    assert_eq!(channel(done.id, cx), None);
    let stored = workspace.stored_jobs(cx);
    assert_eq!(stored.iter().find(|job| job.id == stopped.id).and_then(|job| job.channel.as_deref()), Some("UC1"));
}

#[gpui_kit::test]
fn disconnecting_an_account_that_signs_in_again_stops_the_sign_in(cx: &mut TestAppContext) {
    let Some(emulators) = Emulators::start() else { return };
    let workspace = Workspace::open(cx, emulators.backend());
    workspace.sign_in(cx, "creator@example.com");
    let service = workspace.service.clone();
    let key = workspace.account_key(cx, "creator@example.com");
    service.update(cx, |service, cx| service.reconnect(&key, cx));
    let url =
        wait_until(cx, "the consent page", |cx| service.read_with(cx, |s, _| s.sign_in_url().map(ToString::to_string)));

    service.update(cx, |service, cx| service.disconnect(&key, cx));
    assert!(!service.read_with(cx, |s, _| s.is_signing_in()));
    // Finishing the consent now brings nothing back.
    std::thread::sleep(Duration::from_millis(200));
    assert!(publish_support::try_consent_in_browser(&url, "creator@example.com").is_err());
    cx.run_until_parked();
    assert!(service.read_with(cx, |s, _| s.accounts().is_empty()));
    assert!(workspace.saved_logins().is_empty());
}
