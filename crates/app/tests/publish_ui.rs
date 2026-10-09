//! UI integration tests for the publish views: the Publish tab, the Uploads page,
//! the YouTube settings and the sidebar account badge in headless windows, driven
//! by clicks and typing. The sign-in-to-upload flow runs against the emulators and
//! is skipped with a notice when Node or `emulators/node_modules` is missing.

mod publish_support;

use std::time::Duration;

use chrono::Utc;
use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, Entity, Render, TestAppContext, Window, WindowBounds, WindowOptions,
    px, size,
};
use publish_support::{Emulators, Workspace, wait_until};
use videocrm::publish::{
    AccountBadge, BlockingReason, CHAPTERS_HEADING, Connection, PublishPanel, UploadsPage, YoutubeSettings,
};
use videocrm::route::Route;
use videocrm_core::settings::YoutubeBackend;
use videocrm_core::{Block, BlockKind, DocumentKind, Privacy, UploadId, UploadJob, UploadStatus, VideoId};

/// Longer than the views' save delay.
const SAVED: Duration = Duration::from_secs(1);

/// Opens `build` in a window tall enough to show the whole Publish tab.
fn open<V: Render>(
    cx: &mut TestAppContext,
    build: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
) -> (AnyWindowHandle, Entity<V>) {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(Default::default(), size(px(1280.), px(2000.))))),
        ..WindowOptions::default()
    };
    cx.update(|cx| gpui_kit::open_window(options, cx, build).expect("open the test window"))
}

fn window(cx: &mut TestAppContext, handle: AnyWindowHandle, f: impl FnOnce(&mut Window, &mut App)) {
    cx.update_window(handle, |_, window, cx| f(window, cx)).unwrap();
}

/// Lets the views' debounced saves run.
fn settle(cx: &mut TestAppContext) {
    cx.executor().advance_clock(SAVED);
    cx.run_until_parked();
}

#[gpui_kit::test]
fn the_publish_form_saves_metadata_and_explains_what_blocks_the_upload(cx: &mut TestAppContext) {
    let workspace = Workspace::open(cx, YoutubeBackend::default());
    let video_id = workspace.create_video(cx, "Desk setup 2026", |_| {});
    let (handle, panel) = open(cx, |window, cx| cx.new(|cx| PublishPanel::new(video_id, window, cx)));
    let reasons = |cx: &mut TestAppContext| panel.read_with(cx, |panel, cx| panel.blocking_reasons(cx));
    assert_eq!(reasons(cx), [BlockingReason::NotConnected, BlockingReason::NoFile]);

    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        // Disabled while something blocks it: an enabled button would report why it failed.
        window.click("upload-to-youtube", cx);
        assert!(window.notifications(cx).is_empty());
        // The title field is empty: uploads use the video's title until it is set.
        assert_eq!(window.find("publish-title").value(), Some(""));
        window.click("publish-title", cx);
        window.input("Desk setup <2026>", cx);
        // A comma finishes a tag, Enter finishes the last one.
        window.click("publish-tag-input", cx);
        window.input("desk, setup", cx);
        window.press("enter", cx);
    });
    // Nothing is written before the save delay ends.
    cx.run_until_parked();
    assert!(workspace.service.read_with(cx, |service, _| service.jobs().is_empty()), "no upload started");
    assert_eq!(workspace.video(cx, video_id).publish.title, "");
    settle(cx);
    let video = workspace.video(cx, video_id);
    assert_eq!(video.publish.title, "Desk setup <2026>");
    assert_eq!(video.publish.tags, ["desk", "setup"]);
    assert_eq!(reasons(cx), [BlockingReason::NotConnected, BlockingReason::NoFile, BlockingReason::TitleHasBrackets]);

    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("publish-tag-input").value(), Some(""));
        window.click("remove-tag-desk", cx);
        // Public, then scheduled: scheduling forces private and locks the choice.
        window.within("publish-privacy").click(2usize, cx);
        assert_eq!(window.within("publish-privacy").find(2usize).checked(), Some(true));
        window.click("publish-schedule", cx);
        assert_eq!(window.find("publish-schedule").checked(), Some(true));
        assert_eq!(window.within("publish-privacy").find(0usize).checked(), Some(true));
        // The privacy choice is locked while scheduled.
        window.within("publish-privacy").click(1usize, cx);
        assert_eq!(window.within("publish-privacy").find(0usize).checked(), Some(true));
        window.within("publish-made-for-kids").click(0usize, cx);
    });
    settle(cx);
    let publish = workspace.video(cx, video_id).publish;
    assert_eq!(publish.tags, ["setup"]);
    assert_eq!(publish.privacy, Privacy::Private);
    assert!(publish.publish_at.is_some_and(|at| at > Utc::now()), "scheduled in the future");
    assert!(publish.made_for_kids);
}

#[gpui_kit::test]
fn chapters_come_from_the_script_headings(cx: &mut TestAppContext) {
    let workspace = Workspace::open(cx, YoutubeBackend::default());
    let video_id = workspace.create_video(cx, "Rust ownership in 60 seconds", |video| {
        video.publish.description = "One owner, many borrowers.".into();
    });
    workspace.state.update(cx, |state, cx| {
        let mut script = state.documents(video_id).into_iter().find(|doc| doc.kind == DocumentKind::Script).unwrap();
        script.blocks = vec![
            Block::new(BlockKind::Heading2, "Why ownership"),
            Block::paragraph(vec!["word"; 150].join(" ")),
            Block::new(BlockKind::Heading2, "Borrowing"),
        ];
        state.save_document(script, cx);
    });
    let (handle, _panel) = open(cx, |window, cx| cx.new(|cx| PublishPanel::new(video_id, window, cx)));
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("insert-chapters", cx);
    });
    settle(cx);
    assert_eq!(
        workspace.video(cx, video_id).publish.description,
        format!("One owner, many borrowers.\n\n{CHAPTERS_HEADING}\n0:00 Why ownership\n1:00 Borrowing")
    );
}

#[gpui_kit::test]
fn a_script_without_sections_offers_no_chapters(cx: &mut TestAppContext) {
    let workspace = Workspace::open(cx, YoutubeBackend::default());
    let video_id = workspace.create_video(cx, "Tokyo tech stores tour", |_| {});
    workspace.state.update(cx, |state, cx| {
        let mut script = state.documents(video_id).into_iter().find(|doc| doc.kind == DocumentKind::Script).unwrap();
        script.blocks = vec![Block::paragraph("Just one take, no sections.")];
        state.save_document(script, cx);
    });
    let (handle, _panel) = open(cx, |window, cx| cx.new(|cx| PublishPanel::new(video_id, window, cx)));
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        // Disabled: there is nothing to insert.
        window.click("insert-chapters", cx);
    });
    settle(cx);
    assert_eq!(workspace.video(cx, video_id).publish.description, "");
}

#[gpui_kit::test]
fn the_uploads_page_offers_what_each_job_allows(cx: &mut TestAppContext) {
    let workspace = Workspace::open(cx, YoutubeBackend::default());
    let (handle, _page) = open(cx, |window, cx| cx.new(|cx| UploadsPage::new(window, cx)));
    workspace.state.update(cx, |state, cx| state.navigate(Route::Uploads, cx));
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        // The empty state's way out.
        window.click("uploads-go-to-videos", cx);
    });
    assert_eq!(workspace.state.read_with(cx, |state, _| state.route().clone()), Route::default());

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
    let interrupted = job(UploadStatus::Uploading { sent: 10, total: 100 }, Some("http://127.0.0.1:1/upload/session"));
    let done = job(UploadStatus::Done { video_id: "dQw4w9WgXcQ".into() }, None);
    workspace.insert_jobs(cx, &[&interrupted, &done]);
    // A restart turns the unfinished job into a resumable failure.
    let service = workspace.restart(cx);
    let (handle, _page) = open(cx, |window, cx| cx.new(|cx| UploadsPage::new(window, cx)));
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("uploads-go-to-videos").is_none());
        // Resuming needs the account, which is not connected: the button is disabled
        // (an enabled one would report the failed resume).
        window.click(format!("upload-{}-resume", interrupted.id), cx);
        assert!(window.notifications(cx).is_empty());
        window.click(format!("upload-{}-open", done.id), cx);
    });
    cx.run_until_parked();
    assert!(!service.read_with(cx, |service, _| service.is_active(interrupted.id)));
    assert_eq!(
        service.read_with(cx, |service, _| service.job(interrupted.id).map(|job| job.status.clone())),
        Some(UploadStatus::Failed { error: videocrm::publish::INTERRUPTED_RESUMABLE.into() })
    );
    assert_eq!(cx.opened_url().as_deref(), Some("https://www.youtube.com/watch?v=dQw4w9WgXcQ"));
}

#[gpui_kit::test]
fn youtube_settings_choose_the_backend_and_save_the_oauth_client(cx: &mut TestAppContext) {
    let workspace = Workspace::open(cx, YoutubeBackend::default());
    let (handle, _settings) = open(cx, |window, cx| cx.new(|cx| YoutubeSettings::new(window, cx)));
    let backend = |cx: &mut TestAppContext| workspace.state.read_with(cx, |state, _| state.settings().youtube.clone());

    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(window.within("youtube-backend").find(1usize).checked(), Some(true));
        assert!(window.find("youtube-google-url").visible());
        window.within("youtube-backend").click(0usize, cx);
    });
    assert_eq!(backend(cx), YoutubeBackend::Google { client_id: String::new(), client_secret: String::new() });
    assert!(workspace.service.read_with(cx, |service, _| service.config().is_err()), "no client yet");

    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("youtube-google-url").is_none());
        window.click("youtube-client-id", cx);
        window.input("1234-abc.apps.googleusercontent.com", cx);
        window.click("youtube-client-secret", cx);
        window.input("GOCSPX-secret", cx);
        // The secret is masked, so its value is not exposed either.
        assert_eq!(window.find("youtube-client-secret").value(), None);
    });
    settle(cx);
    assert_eq!(
        backend(cx),
        YoutubeBackend::Google {
            client_id: "1234-abc.apps.googleusercontent.com".into(),
            client_secret: "GOCSPX-secret".into()
        }
    );
    let config = workspace.service.read_with(cx, |service, _| service.config().ok().cloned());
    assert!(
        config.is_some_and(|config| config.is_google() && config.client_id == "1234-abc.apps.googleusercontent.com")
    );
}

#[gpui_kit::test]
fn the_account_badge_opens_the_youtube_settings(cx: &mut TestAppContext) {
    let workspace = Workspace::open(cx, YoutubeBackend::default());
    let (handle, _badge) = open(cx, |window, cx| cx.new(|cx| AccountBadge::new(window, cx)));
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("account-badge").label(), Some("Connect YouTube. Open YouTube settings"));
        window.click("account-badge", cx);
    });
    assert_eq!(workspace.state.read_with(cx, |state, _| state.route().clone()), Route::Settings);
}

#[gpui_kit::test]
fn connects_and_uploads_from_the_publish_tab(cx: &mut TestAppContext) {
    let Some(emulators) = Emulators::start() else { return };
    let workspace = Workspace::open(cx, emulators.backend());
    let file = publish_support::video_file(workspace.dir.path(), "final.mp4");
    let video_id: VideoId = workspace.create_video(cx, "Keychron Q1 Max review", |video| {
        video.publish.file = Some(file.clone());
        video.publish.privacy = Privacy::Unlisted;
    });
    let (handle, panel) = open(cx, |window, cx| cx.new(|cx| PublishPanel::new(video_id, window, cx)));
    // The file details are read in the background.
    cx.run_until_parked();

    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.within("publish-connection").click("connect", cx);
    });
    let service = workspace.service.clone();
    let url = wait_until(cx, "the consent page", |cx| match service.read_with(cx, |s, _| s.connection().clone()) {
        Connection::Connecting { authorize_url: Some(url) } => Some(url.to_string()),
        _ => None,
    });
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        // No browser on this machine: the link can be copied instead.
        window.within("publish-connection").click("copy-link", cx);
    });
    assert_eq!(cx.read_from_clipboard().and_then(|item| item.text()).as_deref(), Some(url.as_str()));
    publish_support::consent_in_browser(&url, "creator@example.com");
    wait_until(cx, "the connection", |cx| service.read_with(cx, |s, _| s.is_connected().then_some(())));
    assert_eq!(panel.read_with(cx, |panel, cx| panel.blocking_reasons(cx)), []);

    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("upload-to-youtube", cx);
    });
    let youtube_id = wait_until(cx, "the upload", |cx| {
        match service.read_with(cx, |s, _| s.latest_job(video_id).map(|job| job.status.clone())) {
            Some(UploadStatus::Processing { video_id } | UploadStatus::Done { video_id }) => Some(video_id),
            Some(UploadStatus::Failed { error }) => panic!("upload failed: {error}"),
            _ => None,
        }
    });
    window(cx, handle, |window, cx| {
        // "Uploaded to YouTube" is announced; it covers the side column, so dismiss it first.
        assert_eq!(window.notifications(cx).len(), 1);
        window.clear_notifications(cx);
    });
    settle(cx);
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.notifications(cx).is_empty());
        assert!(window.try_find("upload-to-youtube").is_none(), "the upload button gives way to the result");
        window.click("open-on-youtube", cx);
    });
    let link = workspace.video(cx, video_id).youtube.expect("the video links to YouTube");
    assert_eq!(link.video_id, youtube_id);
    assert_eq!(cx.opened_url(), Some(link.url));
    assert_eq!(emulators.uploads().len(), 1);
}
