//! `YoutubeService` end to end against the local Google + YouTube emulators
//! (`emulators/`): sign-in, uploads with cancel and resume, recovery of interrupted
//! jobs, backend switches. Every test starts its own emulator pair on free ports and
//! is skipped with a notice when Node or `emulators/node_modules` is missing.

mod publish_support;

use std::time::Duration;

use chrono::Utc;
use gpui_kit::TestAppContext;
use publish_support::{Emulators, Workspace, wait_until};
use serde_json::json;
use videocrm::publish::{Connection, INTERRUPTED, INTERRUPTED_RESUMABLE, ProcessingOutcome, UploadError};
use videocrm_core::settings::YoutubeBackend;
use videocrm_core::{Privacy, UploadId, UploadJob, UploadStatus, VideoStatus};
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

    let saved = CredentialStore::new(workspace.dir.path().join("credentials.json")).load().unwrap().unwrap();
    assert_eq!(saved.account.as_ref(), Some(&account));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(workspace.dir.path().join("credentials.json")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // The channel picture is cached for the account badge.
    let avatar = wait_until(cx, "the avatar", |cx| service.read_with(cx, |s, _| s.avatar().map(ToOwned::to_owned)));
    assert!(avatar.starts_with(workspace.dir.path()) && avatar.is_file());

    // A restart finds the account again without a sign-in.
    let restarted = workspace.restart(cx);
    assert_eq!(restarted.read_with(cx, |s, _| s.account().cloned()), Some(account));

    restarted.update(cx, |service, cx| service.disconnect(cx));
    cx.run_until_parked();
    assert_eq!(restarted.read_with(cx, |s, _| s.connection().clone()), Connection::Disconnected);
    assert!(!workspace.dir.path().join("credentials.json").exists());
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
    let saved = CredentialStore::new(workspace.dir.path().join("credentials.json")).load().unwrap().unwrap();
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
    let video_id = workspace.create_video(cx, "Desk setup 2026", |video| {
        video.publish.file = Some(file.clone());
        video.publish.privacy = Privacy::Private;
        video.publish.publish_at = Some(Utc::now() + chrono::TimeDelta::days(2));
    });

    let upload = service.update(cx, |service, cx| service.start_upload(video_id, cx)).unwrap();
    wait_until(cx, "the upload", |cx| {
        let status = service.read_with(cx, |s, _| s.job(upload).map(|job| job.status.clone()));
        matches!(status, Some(UploadStatus::Processing { .. } | UploadStatus::Done { .. })).then_some(())
    });
    assert_eq!(workspace.video(cx, video_id).status, VideoStatus::Scheduled);
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
    let video_id = workspace.create_video(cx, "Why I left macOS for Linux", |_| {});
    let job = |status: UploadStatus, session_url: Option<&str>| UploadJob {
        id: UploadId::new(),
        video_id,
        file: workspace.dir.path().join("final.mp4"),
        status,
        session_url: session_url.map(str::to_owned),
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
    let workspace = Workspace::open(cx, YoutubeBackend::default());
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
    let path = workspace.dir.path().join("credentials.json");
    CredentialStore::new(&path).save(&SavedLogin { credentials, account: Some(account.clone()) }).unwrap();
    let service = workspace.restart(cx);
    assert_eq!(service.read_with(cx, |s, _| s.account().cloned()), Some(account));

    workspace.state.update(cx, |state, cx| {
        state.update_settings(cx, |settings| {
            settings.youtube = YoutubeBackend::Google { client_id: "id".into(), client_secret: String::new() }
        })
    });
    cx.run_until_parked();
    assert_eq!(service.read_with(cx, |s, _| s.connection().clone()), Connection::Disconnected);
    assert!(!path.exists(), "the other backend's tokens are forgotten");
    let problem = service.read_with(cx, |s, _| s.config().err().cloned()).unwrap();
    assert_eq!(problem.as_ref(), "Add the client ID and client secret of your Google OAuth client.");

    service.update(cx, |service, cx| service.connect(cx));
    assert!(matches!(service.read_with(cx, |s, _| s.connection().clone()), Connection::Failed(_)));
}
