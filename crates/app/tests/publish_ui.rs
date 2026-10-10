//! UI integration tests for the publish views: the Publish tab, the Uploads page,
//! the YouTube settings and the sidebar account badge in headless windows, driven
//! by clicks and typing. The sign-in-to-upload flow runs against the emulators and
//! is skipped with a notice when Node or `emulators/node_modules` is missing.

mod publish_support;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use chrono::Utc;
use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, Entity, ExternalPaths, FileDropEvent, PlatformInput, Render,
    TestAppContext, Window, WindowBounds, WindowOptions, px, size,
};
use publish_support::{Emulators, Workspace, wait_until};
use serde_json::json;
use videocrm::ai_edit::{AiEditPanel, PipelineService};
use videocrm::publish::{
    AccountBadge, BlockingReason, CHAPTERS_HEADING, Connection, PublishPanel, UploadsPage, YoutubeSettings,
};
use videocrm::route::{Route, SettingsSection, VideoTab};
use videocrm::state::{AppEvent, Severity};
use videocrm_core::settings::YoutubeBackend;
use videocrm_core::{
    Block, BlockKind, DocumentKind, PipelineRun, Privacy, RunId, RunStatus, UploadId, UploadJob, UploadStatus, VideoId,
    YoutubeLink,
};
use videocrm_youtube::{CredentialStore, RetryCause};

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
fn a_narrow_publish_tab_starts_with_the_upload_card(cx: &mut TestAppContext) {
    let workspace = Workspace::open(cx, YoutubeBackend::default());
    let video_id = workspace.create_video(cx, "Desk setup 2026", |_| {});
    let options = WindowOptions {
        // The page column at the app's minimum window size.
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(Default::default(), size(px(560.), px(2400.))))),
        ..WindowOptions::default()
    };
    let (handle, _panel) = cx
        .update(|cx| {
            gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| PublishPanel::new(video_id, window, cx)))
        })
        .unwrap();
    // The first frame measures the panel, the next one lays it out for that width.
    for _ in 0..2 {
        window(cx, handle, |window, cx| window.render_frame(cx));
        cx.run_until_parked();
    }
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        let panel = window.find("publish-panel").bounds();
        let title = window.find("publish-title").bounds();
        let upload = window.find("publish-upload").bounds();
        let account = window.find("publish-account").bounds();
        // The next step and what blocks it come first; the other cards follow the form.
        assert!(upload.bottom() < title.top(), "the Upload card leads: {upload:?} vs {title:?}");
        assert!(account.top() > title.bottom(), "the other cards follow the form: {account:?} vs {title:?}");
        assert!(upload.right() <= panel.right(), "nothing reaches past the column: {upload:?} vs {panel:?}");
    });
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

    // Not scheduled after all: the earlier choice comes back.
    window(cx, handle, |window, cx| {
        window.click("publish-schedule", cx);
        assert_eq!(window.within("publish-privacy").find(2usize).checked(), Some(true));
    });
    settle(cx);
    let publish = workspace.video(cx, video_id).publish;
    assert_eq!((publish.privacy, publish.publish_at), (Privacy::Public, None));
}

#[gpui_kit::test]
fn a_pending_form_edit_keeps_the_final_cut_ai_edit_chose(cx: &mut TestAppContext) {
    let workspace = Workspace::open(cx, YoutubeBackend::default());
    let video_id = workspace.create_video(cx, "Desk setup 2026", |_| {});
    let (handle, panel) = open(cx, |window, cx| cx.new(|cx| PublishPanel::new(video_id, window, cx)));
    // The creator types the YouTube title ...
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("publish-title", cx);
        window.input("My title", cx);
    });
    cx.run_until_parked();
    // ... while the AI edit render finishes and picks its final cut, within the save delay.
    let cut = publish_support::video_file(workspace.dir.path(), "final.mp4");
    workspace.state.update(cx, |state, cx| {
        let mut video = state.video(video_id).unwrap().clone();
        video.publish.file = Some(cut.clone());
        state.update_video(video, cx);
    });
    cx.run_until_parked();
    // The form shows the new file right away and keeps the typed title.
    assert_eq!(panel.read_with(cx, |panel, cx| panel.blocking_reasons(cx)), [BlockingReason::NotConnected]);
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("publish-title").value(), Some("My title"));
    });
    settle(cx);
    let publish = workspace.video(cx, video_id).publish;
    assert_eq!(publish.title, "My title");
    assert_eq!(publish.file, Some(cut), "the form’s save leaves the final cut in place");
}

#[gpui_kit::test]
fn long_blocking_reasons_wrap_inside_the_upload_card(cx: &mut TestAppContext) {
    let workspace = Workspace::open(cx, YoutubeBackend::default());
    let video_id = workspace.create_video(cx, "Desk setup 2026", |video| {
        video.publish.title = format!("<{}>", "Desk setup ".repeat(10));
        video.publish.description = "é".repeat(2672);
        video.publish.tags = (0..60).map(|ix| format!("desk tag {ix}")).collect();
        video.publish.publish_at = Some(Utc::now() - chrono::TimeDelta::days(1));
    });
    let (handle, panel) = open(cx, |window, cx| cx.new(|cx| PublishPanel::new(video_id, window, cx)));
    let reasons = panel.read_with(cx, |panel, cx| panel.blocking_reasons(cx));
    assert!(reasons.len() >= 6, "{reasons:?}");
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        let list = window.find("upload-blockers").bounds();
        for ix in 0..reasons.len() {
            let reason = window.find(("upload-blocker", ix)).bounds();
            assert!(reason.right() <= list.right(), "reason {ix} runs past the card: {reason:?} vs {list:?}");
        }
    });
}

#[gpui_kit::test]
fn a_chosen_file_counts_as_missing_only_once_it_is_known_to_be(cx: &mut TestAppContext) {
    let workspace = Workspace::open(cx, YoutubeBackend::default());
    let file = publish_support::video_file(workspace.dir.path(), "final.mp4");
    let video_id = workspace.create_video(cx, "Desk setup 2026", |video| video.publish.file = Some(file.clone()));
    let (_handle, panel) = open(cx, |window, cx| cx.new(|cx| PublishPanel::new(video_id, window, cx)));
    let reasons = |cx: &mut TestAppContext| panel.read_with(cx, |panel, cx| panel.blocking_reasons(cx));
    // The file details are still being read: no "no longer exists" in the meantime.
    assert_eq!(reasons(cx), [BlockingReason::NotConnected]);
    cx.run_until_parked();
    assert_eq!(reasons(cx), [BlockingReason::NotConnected]);

    // Chosen elsewhere, and gone from the disk.
    workspace.state.update(cx, |state, cx| {
        let mut video = state.video(video_id).unwrap().clone();
        video.publish.file = Some(workspace.dir.path().join("deleted.mp4"));
        state.update_video(video, cx);
    });
    cx.run_until_parked();
    assert_eq!(reasons(cx), [BlockingReason::NotConnected, BlockingReason::FileMissing]);
}

#[gpui_kit::test]
fn a_finished_upload_says_why_it_cannot_go_up_again(cx: &mut TestAppContext) {
    let workspace = Workspace::open(cx, YoutubeBackend::default());
    let youtube_id = "dQw4w9WgXcQ";
    let video_id = workspace.create_video(cx, "Desk setup 2026", |video| {
        video.youtube = Some(YoutubeLink {
            video_id: youtube_id.into(),
            url: format!("https://www.youtube.com/watch?v={youtube_id}"),
            channel_id: "UC1".into(),
            privacy: Privacy::Public,
            uploaded_at: Utc::now(),
        });
    });
    let done = UploadJob {
        id: UploadId::new(),
        video_id,
        file: workspace.dir.path().join("final.mp4"),
        file_stamp: None,
        status: UploadStatus::Done { video_id: youtube_id.into() },
        session_url: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    workspace.insert_jobs(cx, &[&done]);
    workspace.restart(cx);
    let (handle, _panel) = open(cx, |window, cx| cx.new(|cx| PublishPanel::new(video_id, window, cx)));
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.find("open-on-youtube").visible());
        // Not connected and no file: "Upload again" is disabled (an enabled one would ask
        // to confirm a second copy), and the reasons are listed.
        window.click("upload-again", cx);
        assert!(!window.has_active_dialog(cx));
        assert!(window.find("upload-blockers").visible());
    });
}

#[gpui_kit::test]
fn after_a_failed_reupload_the_button_still_says_upload_again(cx: &mut TestAppContext) {
    let Some(emulators) = Emulators::start() else { return };
    let workspace = Workspace::open(cx, emulators.backend());
    workspace.sign_in(cx, "creator@example.com");
    let file = publish_support::video_file(workspace.dir.path(), "final.mp4");
    let youtube_id = "dQw4w9WgXcQ";
    let video_id = workspace.create_video(cx, "Rust Ownership Explained", |video| {
        video.publish.file = Some(file.clone());
        video.youtube = Some(YoutubeLink {
            video_id: youtube_id.into(),
            url: format!("https://www.youtube.com/watch?v={youtube_id}"),
            channel_id: "UC1".into(),
            privacy: Privacy::Private,
            uploaded_at: Utc::now(),
        });
    });
    // "Upload again…" ran out of quota.
    let failed = UploadJob {
        id: UploadId::new(),
        video_id,
        file: file.clone(),
        file_stamp: None,
        status: UploadStatus::Failed { error: "The YouTube API quota for today is used up.".into() },
        session_url: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    workspace.insert_jobs(cx, &[&failed]);
    let service = workspace.restart(cx);
    wait_until(cx, "the connection", |cx| service.read_with(cx, |s, _| s.is_connected().then_some(())));
    let (handle, _panel) = open(cx, |window, cx| cx.new(|cx| PublishPanel::new(video_id, window, cx)));
    cx.run_until_parked();
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("upload-to-youtube").label(), Some("Upload again…"));
        // It opens the confirmation, as its ellipsis says.
        window.click("upload-to-youtube", cx);
        assert!(window.has_active_dialog(cx));
    });
}

#[gpui_kit::test]
fn quitting_saves_the_last_form_edits(cx: &mut TestAppContext) {
    let workspace = Workspace::open(cx, YoutubeBackend::default());
    let video_id = workspace.create_video(cx, "Desk setup 2026", |_| {});
    let store = workspace.state.read_with(cx, |state, _| state.store());
    let (handle, _panel) = open(cx, |window, cx| cx.new(|cx| PublishPanel::new(video_id, window, cx)));
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("publish-title", cx);
        window.input("Desk tour", cx);
    });
    cx.run_until_parked();
    assert_eq!(workspace.video(cx, video_id).publish.title, "", "still within the save delay");
    cx.quit();
    let saved = store.lock().unwrap().get_video(video_id).unwrap().unwrap();
    assert_eq!(saved.publish.title, "Desk tour");
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
            Block::paragraph(vec!["word"; 150].join(" ")),
            Block::new(BlockKind::Heading2, "Lifetimes"),
        ];
        state.save_document(script, cx);
    });
    let (handle, _panel) = open(cx, |window, cx| cx.new(|cx| PublishPanel::new(video_id, window, cx)));
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.find("chapters-estimate").visible());
        window.click("insert-chapters", cx);
    });
    settle(cx);
    assert_eq!(
        workspace.video(cx, video_id).publish.description,
        format!("One owner, many borrowers.\n\n{CHAPTERS_HEADING}\n0:00 Why ownership\n1:00 Borrowing\n2:00 Lifetimes")
    );
}

#[gpui_kit::test]
fn only_chapters_youtube_shows_are_offered(cx: &mut TestAppContext) {
    let workspace = Workspace::open(cx, YoutubeBackend::default());
    let file = publish_support::video_file(workspace.dir.path(), "final.mp4");
    let video_id = workspace.create_video(cx, "Stop Using print() to Debug Python", |video| {
        video.format = videocrm_core::VideoFormat::Short;
        video.publish.file = Some(file.clone());
    });
    let script = |sections: usize, cx: &mut TestAppContext| {
        workspace.state.update(cx, |state, cx| {
            let mut script =
                state.documents(video_id).into_iter().find(|doc| doc.kind == DocumentKind::Script).unwrap();
            script.blocks = (0..sections)
                .flat_map(|ix| {
                    [
                        Block::new(BlockKind::Heading2, format!("Part {ix}")),
                        Block::paragraph(vec!["word"; 50].join(" ")),
                    ]
                })
                .collect();
            state.save_document(script, cx);
        });
    };
    script(3, cx);
    let (handle, _panel) = open(cx, |window, cx| cx.new(|cx| PublishPanel::new(video_id, window, cx)));
    cx.run_until_parked();
    let unavailable = |handle: AnyWindowHandle, cx: &mut TestAppContext| {
        let mut shown = false;
        window(cx, handle, |window, cx| {
            window.render_frame(cx);
            // Disabled: inserting would add a list YouTube ignores.
            window.click("insert-chapters", cx);
            shown = window.find("chapters-unavailable").visible();
        });
        settle(cx);
        shown && workspace.video(cx, video_id).publish.description.is_empty()
    };
    assert!(unavailable(handle, cx), "Shorts have no chapters");

    workspace.state.update(cx, |state, cx| {
        let mut video = state.video(video_id).unwrap().clone();
        video.format = videocrm_core::VideoFormat::LongForm;
        state.update_video(video, cx);
    });
    script(2, cx);
    assert!(unavailable(handle, cx), "two chapters are too few");

    script(3, cx);
    cx.run_until_parked();
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        // Three 20 s sections end at 0:40; the one-second test clip is far shorter.
        if videocrm_pipeline::probe_media(&file).is_some() {
            assert!(window.find("chapters-past-the-end").visible());
        }
        window.click("insert-chapters", cx);
    });
    settle(cx);
    assert!(workspace.video(cx, video_id).publish.description.contains("0:40 Part 2"));
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

    // Resume continues the file the video still has chosen; the finished upload is on YouTube.
    let file = workspace.dir.path().join("final.mp4");
    let video_id = workspace.create_video(cx, "Why I left macOS for Linux", |video| {
        video.publish.file = Some(file.clone());
        video.youtube = Some(YoutubeLink {
            video_id: "dQw4w9WgXcQ".into(),
            url: "https://www.youtube.com/watch?v=dQw4w9WgXcQ".into(),
            channel_id: "UC1".into(),
            privacy: Privacy::Unlisted,
            uploaded_at: Utc::now(),
        });
    });
    let job = |status: UploadStatus, session_url: Option<&str>| UploadJob {
        id: UploadId::new(),
        video_id,
        file: workspace.dir.path().join("final.mp4"),
        file_stamp: None,
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

    // Like a Pipelines row, the row itself opens the video, on its Publish tab.
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click(format!("upload-{}", done.id), cx);
    });
    assert_eq!(
        workspace.state.read_with(cx, |state, _| state.route().clone()),
        Route::Video { id: video_id, tab: VideoTab::Publish }
    );
}

#[gpui_kit::test]
fn buttons_in_upload_rows_act_on_their_own(cx: &mut TestAppContext) {
    let Some(emulators) = Emulators::start() else { return };
    let workspace = Workspace::open(cx, emulators.backend());
    workspace.sign_in(cx, "creator@example.com");
    let service = workspace.service.clone();
    let file = publish_support::video_file(workspace.dir.path(), "take2.mp4");
    let video_id = workspace.create_video(cx, "Stop Using print() to Debug Python", |video| {
        video.publish.file = Some(file.clone());
    });
    // YouTube keeps failing, so the upload keeps running until it is cancelled.
    emulators.inject_fault(json!({"method": "PUT", "path": "/upload/youtube/v3/videos", "status": 503, "times": 1000}));
    let upload = service.update(cx, |service, cx| service.start_upload(video_id, cx)).unwrap();
    wait_until(cx, "the upload session", |cx| {
        service.read_with(cx, |s, _| s.job(upload).and_then(|job| job.session_url.clone()))
    });
    workspace.state.update(cx, |state, cx| state.navigate(Route::Uploads, cx));
    let route = |cx: &mut TestAppContext| workspace.state.read_with(cx, |state, _| state.route().clone());
    let (handle, _page) = open(cx, |window, cx| cx.new(|cx| UploadsPage::new(window, cx)));

    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click(format!("upload-{upload}-cancel"), cx);
    });
    assert_eq!(route(cx), Route::Uploads, "Cancel doesn’t open the video");
    wait_until(cx, "the cancellation", |cx| {
        service.read_with(cx, |s, _| s.job(upload).filter(|job| job.status == UploadStatus::Cancelled).map(|_| ()))
    });
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click(format!("upload-{upload}-resume"), cx);
    });
    assert_eq!(route(cx), Route::Uploads, "Resume doesn’t open the video");
    assert!(service.read_with(cx, |s, _| s.is_active(upload)));

    // From the keyboard: Enter on the row's button presses the button.
    let button = format!("upload-{upload}-cancel");
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        // Tab order: the row, then its button.
        for _ in 0..4 {
            if window.find(button.clone()).focused() == Some(true) {
                break;
            }
            window.focus_next(cx);
            window.render_frame(cx);
        }
        assert_eq!(window.find(button.clone()).focused(), Some(true));
        window.press("enter", cx);
    });
    assert!(service.read_with(cx, |s, _| s.is_cancelling(upload)));
    assert_eq!(route(cx), Route::Uploads);
    emulators.clear_faults();

    // The row itself still opens the video.
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click(format!("upload-{upload}"), cx);
    });
    assert_eq!(route(cx), Route::Video { id: video_id, tab: VideoTab::Publish });
}

#[gpui_kit::test]
fn a_narrow_uploads_page_keeps_each_jobs_action_in_view(cx: &mut TestAppContext) {
    let workspace = Workspace::open(cx, YoutubeBackend::default());
    // Resume continues the file the video still has chosen.
    let file = workspace.dir.path().join("final.mp4");
    let video_id =
        workspace.create_video(cx, "Why I left macOS for Linux", |video| video.publish.file = Some(file.clone()));
    let job = UploadJob {
        id: UploadId::new(),
        video_id,
        file: workspace.dir.path().join("final.mp4"),
        file_stamp: None,
        status: UploadStatus::Uploading { sent: 10, total: 100 },
        session_url: Some("http://127.0.0.1:1/upload/session".into()),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    workspace.insert_jobs(cx, &[&job]);
    workspace.restart(cx);
    let options = WindowOptions {
        // The page column at the app's minimum window size, beside the sidebar.
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(Default::default(), size(px(703.), px(600.))))),
        ..WindowOptions::default()
    };
    let (handle, _page) = cx
        .update(|cx| gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| UploadsPage::new(window, cx))))
        .unwrap();
    // The first frame measures the table, the next one drops what does not fit.
    for _ in 0..2 {
        window(cx, handle, |window, cx| window.render_frame(cx));
        cx.run_until_parked();
    }
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        let row = window.find(format!("upload-{}", job.id)).bounds();
        let resume = window.find(format!("upload-{}-resume", job.id)).bounds();
        assert!(resume.right() <= row.right(), "the action stays in the row: {resume:?} vs {row:?}");
    });
}

#[gpui_kit::test]
fn youtube_settings_start_with_the_google_client_and_hide_the_emulator(cx: &mut TestAppContext) {
    // A fresh install: the creator's own YouTube account, not the developer emulator.
    let workspace = Workspace::open(cx, YoutubeBackend::default());
    let (handle, settings) = open(cx, |window, cx| cx.new(|cx| YoutubeSettings::new(window, cx)));
    // The Settings page shows a group per section; one that came and went as
    // the client is typed would scroll the page back to the top.
    let groups = |cx: &mut TestAppContext| settings.read_with(cx, |settings, cx| settings.sections(cx).len());
    let groups_before = groups(cx);
    let backend = |cx: &mut TestAppContext| workspace.state.read_with(cx, |state, _| state.settings().youtube.clone());
    let problem = |cx: &mut TestAppContext| {
        workspace.service.read_with(cx, |service, _| service.config().err().map(|problem| problem.to_string()))
    };
    assert_eq!(backend(cx), YoutubeBackend::Google { client_id: String::new(), client_secret: String::new() });

    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        // The setup steps unfold and link to the console pages; the fields come next,
        // Connect after them.
        assert!(window.try_find("google-step-2").is_none_or(|step| !step.visible()));
        window.click("youtube-setup-steps", cx);
        window.click("google-step-2", cx);
        let fields = window.find("youtube-client-secret").bounds();
        assert!(window.within("youtube-connection").find("connect").bounds().top() > fields.bottom());
        // The emulator is a folded developer option.
        assert!(window.try_find("youtube-use-emulator").is_none_or(|switch| !switch.visible()));
        assert!(window.try_find("youtube-google-url").is_none());
        // Disabled until the client is filled in (an enabled one would fail to connect).
        window.within("youtube-connection").click("connect", cx);
    });
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://console.cloud.google.com/apis/library/youtube.googleapis.com")
    );
    assert_eq!(workspace.service.read_with(cx, |service, _| service.connection().clone()), Connection::Disconnected);
    assert_eq!(problem(cx).as_deref(), Some("Add the client ID and client secret of your Google OAuth client."));

    // The secret pasted into both fields is caught before any browser opens.
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("youtube-client-id", cx);
        window.input("GOCSPX-secret", cx);
        window.click("youtube-client-secret", cx);
        window.input("GOCSPX-secret", cx);
        // The secret is masked, so its value is not exposed either.
        assert_eq!(window.find("youtube-client-secret").value(), None);
    });
    settle(cx);
    assert!(problem(cx).is_some_and(|problem| problem.contains("isn’t a Google OAuth client ID")));

    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("youtube-client-id", cx);
        window.press("secondary-a", cx);
        window.input("1234-abc.apps.googleusercontent.com", cx);
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
    assert_eq!(groups(cx), groups_before, "a valid client adds no settings group");
    // Connect can be clicked now: what Google will show comes with it.
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        let connect = window.within("youtube-connection").find("connect").bounds();
        assert!(window.find("youtube-expectations").bounds().top() > connect.bottom());
    });

    // Developers unfold the emulator option and switch to it.
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("youtube-developer", cx);
        window.click("youtube-use-emulator", cx);
    });
    assert_eq!(backend(cx), YoutubeBackend::Emulator { google_url: String::new(), youtube_url: String::new() });
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.find("youtube-google-url").visible());
        assert!(window.try_find("youtube-client-id").is_none());
        assert!(window.try_find("youtube-expectations").is_none());
    });
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
    assert_eq!(
        workspace.state.read_with(cx, |state, _| state.route().clone()),
        Route::Settings { section: SettingsSection::Youtube }
    );
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
        // No browser on this machine: the link can be copied instead, and copying says so.
        window.within("publish-connection").click("copy-link", cx);
        assert_eq!(window.notifications(cx).len(), 1);
        window.clear_notifications(cx);
    });
    settle(cx);
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

#[gpui_kit::test]
fn an_outage_shows_the_retry_on_the_upload_card(cx: &mut TestAppContext) {
    let Some(emulators) = Emulators::start() else { return };
    let workspace = Workspace::open(cx, emulators.backend());
    workspace.sign_in(cx, "creator@example.com");
    let file = publish_support::video_file(workspace.dir.path(), "final.mp4");
    let video_id = workspace.create_video(cx, "Keychron Q1 Max review", |video| {
        video.publish.file = Some(file.clone());
        video.publish.privacy = Privacy::Private;
    });
    let (handle, _panel) = open(cx, |window, cx| cx.new(|cx| PublishPanel::new(video_id, window, cx)));
    cx.run_until_parked();
    // YouTube answers every chunk with 503 Backend Error.
    emulators.inject_fault(json!({"method": "PUT", "path": "/upload/youtube/v3/videos", "status": 503, "times": 1000}));
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("upload-to-youtube", cx);
    });
    let service = workspace.service.clone();
    let retry = wait_until(cx, "the retry", |cx| {
        service.read_with(cx, |s, _| s.latest_job(video_id).and_then(|job| s.retry(job.id)))
    });
    assert_eq!((retry.attempt, retry.cause), (1, RetryCause::Server));
    assert_eq!(retry.headline(), "YouTube isn’t responding");
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        // The card says what is going on instead of a frozen "Uploading… 100%".
        assert!(window.find("upload-retrying").visible());
        window.click("cancel-upload", cx);
    });
    let job = wait_until(cx, "the cancellation", |cx| {
        service
            .read_with(cx, |s, _| s.latest_job(video_id).filter(|job| job.status == UploadStatus::Cancelled).cloned())
    });
    assert!(service.read_with(cx, |s, _| s.retry(job.id).is_none() && s.is_resumable(&job)));
}

#[gpui_kit::test]
fn ai_edit_keeps_the_upload_file_while_the_video_uploads(cx: &mut TestAppContext) {
    let Some(emulators) = Emulators::start() else { return };
    let workspace = Workspace::open(cx, emulators.backend());
    workspace.sign_in(cx, "creator@example.com");
    let take = publish_support::video_file(workspace.dir.path(), "take.mp4");
    let cut = workspace.dir.path().join("cut.mp4");
    std::fs::write(&cut, vec![7u8; 200_000]).unwrap();
    let video_id = workspace.create_video(cx, "Desk setup 2026", |video| video.publish.file = Some(take.clone()));
    // A finished AI edit of the video made `cut`.
    let now = Utc::now();
    let run = PipelineRun {
        id: RunId::new(),
        video_id,
        status: RunStatus::Succeeded,
        brief: "Cut it tight.".into(),
        footage_dir: workspace.dir.path().join("footage"),
        session_id: Some("session".into()),
        summary: Some("Cut to 8:12.".into()),
        output: Some(cut.clone()),
        error: None,
        cost_usd: None,
        created_at: now,
        updated_at: now,
    };
    workspace.state.read_with(cx, |state, _| state.store()).lock().unwrap().insert_run(&run).unwrap();
    let state = workspace.state.clone();
    cx.update(|cx| PipelineService::init(state, cx));
    let (handle, _panel) = open(cx, |window, cx| cx.new(|cx| AiEditPanel::new(video_id, window, cx)));
    cx.run_until_parked();

    // YouTube answers every chunk with 503 Backend Error: the upload keeps going.
    emulators.inject_fault(json!({"method": "PUT", "path": "/upload/youtube/v3/videos", "status": 503, "times": 1000}));
    let service = workspace.service.clone();
    let upload = service.update(cx, |service, cx| service.start_upload(video_id, cx)).unwrap();
    wait_until(cx, "the upload session", |cx| service.read_with(cx, |s, _| s.job(upload)?.session_url.clone()));
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        // As on the Publish tab, the file being sent stays the upload file.
        window.click("run-use-for-upload", cx);
    });
    cx.run_until_parked();
    assert_eq!(workspace.video(cx, video_id).publish.file, Some(take.clone()));
    // The pointer rests on the button: its tooltip says why.
    window(cx, handle, |window, cx| window.hover("run-use-for-upload", cx));
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("state-tooltip").is_some_and(|tooltip| tooltip.visible()));
    });

    service.update(cx, |service, cx| service.cancel_upload(upload, cx));
    wait_until(cx, "the cancellation", |cx| {
        service.read_with(cx, |s, _| s.job(upload).filter(|job| job.status == UploadStatus::Cancelled).map(|_| ()))
    });
    emulators.clear_faults();
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        // The upload stopped under the pointer: the tooltip went with it.
        assert!(window.try_find("state-tooltip").is_none(), "no tooltip is left on an enabled button");
        window.click("run-use-for-upload", cx);
    });
    cx.run_until_parked();
    assert_eq!(workspace.video(cx, video_id).publish.file, Some(cut), "it works again once the upload stopped");
}

#[gpui_kit::test]
fn a_stopped_upload_offers_resume_only_for_its_own_file(cx: &mut TestAppContext) {
    let Some(emulators) = Emulators::start() else { return };
    let workspace = Workspace::open(cx, emulators.backend());
    workspace.sign_in(cx, "creator@example.com");
    let take = publish_support::video_file(workspace.dir.path(), "big-take.mp4");
    let cut = workspace.dir.path().join("real-final.mp4");
    std::fs::write(&cut, vec![7u8; 200_000]).unwrap();
    let video_id = workspace.create_video(cx, "Why Every Developer Should Learn SQLite", |video| {
        video.publish.file = Some(take.clone());
    });
    let (handle, _panel) = open(cx, |window, cx| cx.new(|cx| PublishPanel::new(video_id, window, cx)));
    cx.run_until_parked();
    emulators.inject_fault(json!({"method": "PUT", "path": "/upload/youtube/v3/videos", "status": 503, "times": 1000}));
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("upload-to-youtube", cx);
    });
    let service = workspace.service.clone();
    let job = wait_until(cx, "the upload session", |cx| {
        service.read_with(cx, |s, _| s.latest_job(video_id).filter(|job| job.session_url.is_some()).cloned())
    });
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        // The card keeps showing the file being sent: Change opens nothing.
        window.click("choose-video-file", cx);
        assert!(window.try_find("popup-menu").is_none());
        drop_files(window, "video-file-drop", vec![cut.clone()], cx);
        assert_eq!(window.notifications(cx).len(), 1, "a dropped file is refused with a note");
        window.clear_notifications(cx);
        window.click("cancel-upload", cx);
    });
    wait_until(cx, "the cancellation", |cx| {
        service.read_with(cx, |s, _| s.job(job.id).filter(|job| job.status == UploadStatus::Cancelled).map(|_| ()))
    });
    emulators.clear_faults();
    assert_eq!(workspace.video(cx, video_id).publish.file, Some(take.clone()));
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.find("resume-upload").visible());
        window.click("choose-video-file", cx);
        assert!(window.try_find("popup-menu").is_some(), "Change works again once the upload stopped");
        window.press("escape", cx);
    });

    // AI edit's "Use for upload" switches to the newer cut.
    workspace.state.update(cx, |state, cx| {
        let mut video = state.video(video_id).unwrap().clone();
        video.publish.file = Some(cut.clone());
        state.update_video(video, cx);
    });
    cx.run_until_parked();
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        // Resume would send big-take.mp4; a fresh upload sends the chosen cut.
        assert!(window.try_find("resume-upload").is_none());
        assert!(window.find("upload-cancelled-note").visible());
        window.click("upload-to-youtube", cx);
    });
    let youtube_id = wait_until(cx, "the upload", |cx| {
        match service.read_with(cx, |s, _| s.latest_job(video_id).map(|job| job.status.clone())) {
            Some(UploadStatus::Processing { video_id } | UploadStatus::Done { video_id }) => Some(video_id),
            Some(UploadStatus::Failed { error }) => panic!("upload failed: {error}"),
            _ => None,
        }
    });
    let received = emulators.uploads();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0]["videoId"], youtube_id.as_str());
    assert_eq!(received[0]["sha256"], publish_support::sha256(&cut));
}

#[gpui_kit::test]
fn connecting_to_stopped_emulators_fails_without_opening_the_browser(cx: &mut TestAppContext) {
    // Nothing listens on the discard port.
    let stopped =
        YoutubeBackend::Emulator { google_url: "http://127.0.0.1:9".into(), youtube_url: "http://127.0.0.1:9".into() };
    let workspace = Workspace::open(cx, stopped);
    let (handle, _settings) = open(cx, |window, cx| cx.new(|cx| YoutubeSettings::new(window, cx)));
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.within("youtube-connection").click("connect", cx);
    });
    let service = workspace.service.clone();
    let message =
        wait_until(cx, "the failed sign-in", |cx| match service.read_with(cx, |s, _| s.connection().clone()) {
            Connection::Failed(message) => Some(message.to_string()),
            Connection::Connecting { authorize_url: Some(url) } => panic!("the browser opened {url}"),
            _ => None,
        });
    assert_eq!(message, "Neither emulator is answering. Start them with scripts/emulators.sh.");
    assert_eq!(cx.opened_url(), None);
}

#[gpui_kit::test]
fn a_file_found_missing_at_upload_shows_on_the_file_card(cx: &mut TestAppContext) {
    let Some(emulators) = Emulators::start() else { return };
    let workspace = Workspace::open(cx, emulators.backend());
    workspace.sign_in(cx, "creator@example.com");
    let file = publish_support::video_file(workspace.dir.path(), "huge-take.mp4");
    let video_id = workspace.create_video(cx, "Keychron Q1 Max review", |video| {
        video.publish.file = Some(file.clone());
    });
    let (handle, panel) = open(cx, |window, cx| cx.new(|cx| PublishPanel::new(video_id, window, cx)));
    cx.run_until_parked();
    assert_eq!(panel.read_with(cx, |panel, cx| panel.blocking_reasons(cx)), []);
    // Renamed in the file manager after the tab read it.
    std::fs::rename(&file, workspace.dir.path().join("renamed.mp4")).unwrap();
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("upload-to-youtube", cx);
        assert_eq!(window.notifications(cx).len(), 1);
        window.clear_notifications(cx);
    });
    cx.run_until_parked();
    // The card and the reasons under the button catch up instead of offering the upload again.
    assert_eq!(panel.read_with(cx, |panel, cx| panel.blocking_reasons(cx)), [BlockingReason::FileMissing]);
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.find("upload-blockers").visible());
    });
    assert!(workspace.service.read_with(cx, |s, _| s.jobs().is_empty()));
}

#[gpui_kit::test]
fn an_expired_sign_in_names_the_account_and_offers_reconnect(cx: &mut TestAppContext) {
    let Some(emulators) = Emulators::start() else { return };
    let workspace = Workspace::open(cx, emulators.backend());
    workspace.sign_in(cx, "creator@example.com");
    // Google ends the grant (every 7 days for apps in Testing): the refresh token is gone
    // and the access token has expired by the next start.
    let client = workspace.service.read_with(cx, |s, _| s.client()).unwrap();
    client.revoke().unwrap();
    let store = CredentialStore::new(workspace.dir.path().join("credentials.json"));
    let mut login = store.load().unwrap().unwrap();
    login.credentials.expires_at = Utc::now() - chrono::TimeDelta::hours(1);
    store.save(&login).unwrap();

    let service = workspace.restart(cx);
    let account =
        wait_until(cx, "the rejected sign-in", |cx| match service.read_with(cx, |s, _| s.connection().clone()) {
            Connection::NeedsSignIn(account) => Some(account),
            _ => None,
        });
    assert_eq!(account.channel_handle.as_deref(), Some("@creator"));
    assert!(!service.read_with(cx, |s, _| s.is_connected()));

    let (handle, _badge) = open(cx, |window, cx| cx.new(|cx| AccountBadge::new(window, cx)));
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("account-badge").label(), Some("Demo Creator, Sign in again. Open YouTube settings"));
    });
    let (handle, _settings) = open(cx, |window, cx| cx.new(|cx| YoutubeSettings::new(window, cx)));
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.within("youtube-connection").click("connect", cx);
    });
    let url = wait_until(cx, "the consent page", |cx| match service.read_with(cx, |s, _| s.connection().clone()) {
        Connection::Connecting { authorize_url: Some(url) } => Some(url.to_string()),
        _ => None,
    });
    publish_support::consent_in_browser(&url, "creator@example.com");
    wait_until(cx, "the connection", |cx| service.read_with(cx, |s, _| s.is_connected().then_some(())));
}

#[gpui_kit::test]
fn an_expired_sign_in_blocks_the_upload_with_reconnect(cx: &mut TestAppContext) {
    let Some(emulators) = Emulators::start() else { return };
    let workspace = Workspace::open(cx, emulators.backend());
    workspace.sign_in(cx, "creator@example.com");
    let file = publish_support::video_file(workspace.dir.path(), "final.mp4");
    let video_id = workspace.create_video(cx, "Rust Ownership Explained", |video| {
        video.publish.file = Some(file.clone());
    });
    // The grant ended and the access token expired by the next start.
    let client = workspace.service.read_with(cx, |s, _| s.client()).unwrap();
    client.revoke().unwrap();
    let store = CredentialStore::new(workspace.dir.path().join("credentials.json"));
    let mut login = store.load().unwrap().unwrap();
    login.credentials.expires_at = Utc::now() - chrono::TimeDelta::hours(1);
    store.save(&login).unwrap();
    let service = workspace.restart(cx);
    wait_until(cx, "the rejected sign-in", |cx| {
        service.read_with(cx, |s, _| matches!(s.connection(), Connection::NeedsSignIn(_)).then_some(()))
    });

    let (handle, panel) = open(cx, |window, cx| cx.new(|cx| PublishPanel::new(video_id, window, cx)));
    cx.run_until_parked();
    // The same instruction as the account card, not "Connect a YouTube account."
    assert_eq!(panel.read_with(cx, |panel, cx| panel.blocking_reasons(cx)), [BlockingReason::NeedsSignIn]);
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.find(("upload-blocker", 0usize)).visible());
        window.within("upload-blockers").click("upload-reconnect", cx);
    });
    let consent_page = |cx: &mut TestAppContext| {
        wait_until(cx, "the consent page", |cx| match service.read_with(cx, |s, _| s.connection().clone()) {
            Connection::Connecting { authorize_url: Some(url) } => Some(url.to_string()),
            _ => None,
        })
    };
    consent_page(cx);
    // While the browser is open, the card says to finish there, and nothing else.
    assert_eq!(panel.read_with(cx, |panel, cx| panel.blocking_reasons(cx)), [BlockingReason::SigningIn]);
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.find(("upload-blocker", 0usize)).visible());
        assert!(window.try_find(("upload-blocker", 1usize)).is_none());
        // Cancelling goes back to where the account stood: it still needs a sign-in.
        window.within("publish-account").click("cancel", cx);
    });
    cx.run_until_parked();
    assert!(matches!(service.read_with(cx, |s, _| s.connection().clone()), Connection::NeedsSignIn(_)));
    assert_eq!(panel.read_with(cx, |panel, cx| panel.blocking_reasons(cx)), [BlockingReason::NeedsSignIn]);

    // So does Cancel on the browser's consent page, with a note.
    let notices = Rc::new(RefCell::new(Vec::new()));
    let sink = notices.clone();
    cx.update(|cx| {
        cx.subscribe(&workspace.state, move |_, event: &AppEvent, _| {
            if let AppEvent::Problem(problem) = event {
                sink.borrow_mut().push((problem.title().to_string(), problem.severity()));
            }
        })
        .detach()
    });
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.within("upload-blockers").click("upload-reconnect", cx);
    });
    let url = consent_page(cx);
    publish_support::cancel_in_browser(&url);
    let account =
        wait_until(cx, "the cancelled sign-in", |cx| match service.read_with(cx, |s, _| s.connection().clone()) {
            Connection::NeedsSignIn(account) => Some(account),
            _ => None,
        });
    assert_eq!(account.channel_handle.as_deref(), Some("@creator"), "the account is remembered");
    assert_eq!(panel.read_with(cx, |panel, cx| panel.blocking_reasons(cx)), [BlockingReason::NeedsSignIn]);
    assert_eq!(*notices.borrow(), [("Sign-in cancelled".to_owned(), Severity::Info)]);

    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.within("upload-blockers").click("upload-reconnect", cx);
    });
    let url = consent_page(cx);
    publish_support::consent_in_browser(&url, "creator@example.com");
    wait_until(cx, "the connection", |cx| service.read_with(cx, |s, _| s.is_connected().then_some(())));
    assert_eq!(panel.read_with(cx, |panel, cx| panel.blocking_reasons(cx)), []);
}

#[gpui_kit::test]
fn the_done_card_shows_what_youtube_has_and_what_did_not_work(cx: &mut TestAppContext) {
    let Some(emulators) = Emulators::start() else { return };
    let workspace = Workspace::open(cx, emulators.backend());
    workspace.sign_in(cx, "creator@example.com");
    let service = workspace.service.clone();
    let file = publish_support::video_file(workspace.dir.path(), "final.mp4");
    let thumbnail = publish_support::png_file(workspace.dir.path(), "thumbnail.png");
    let video_id = workspace.create_video(cx, "Stop Using print() to Debug Python", |video| {
        video.publish.file = Some(file.clone());
        video.publish.thumbnail = Some(thumbnail);
        video.publish.privacy = Privacy::Private;
        video.publish.publish_at = Some(Utc::now() + chrono::TimeDelta::days(2));
    });
    // What YouTube answers for channels that aren't phone-verified.
    emulators.inject_fault(
        json!({"method": "POST", "path": "/upload/youtube/v3/thumbnails/set", "status": 403, "reason": "forbidden"}),
    );
    let upload = service.update(cx, |service, cx| service.start_upload(video_id, cx)).unwrap();
    let youtube_id = wait_until(cx, "the processed upload", |cx| {
        cx.executor().advance_clock(Duration::from_secs(5));
        match service.read_with(cx, |s, _| s.job(upload).map(|job| job.status.clone())) {
            Some(UploadStatus::Done { video_id }) => Some(video_id),
            Some(UploadStatus::Failed { error }) => panic!("upload failed: {error}"),
            _ => None,
        }
    });
    let problem = wait_until(cx, "the thumbnail failure", |cx| {
        service.read_with(cx, |s, _| s.thumbnail_problem(upload).map(str::to_owned))
    });
    assert!(problem.contains("verified channels"), "{problem}");

    let (handle, _panel) = open(cx, |window, cx| cx.new(|cx| PublishPanel::new(video_id, window, cx)));
    let info = wait_until(cx, "YouTube’s view of the video", |cx| {
        service.read_with(cx, |s, _| s.youtube_video(&youtube_id).cloned())
    });
    assert_eq!(info.privacy, videocrm_youtube::Privacy::Private);
    assert!(info.publish_at.is_some(), "scheduled on YouTube");
    window(cx, handle, |window, cx| {
        window.clear_notifications(cx);
        window.render_frame(cx);
        // "Private · goes public …", the lasting thumbnail note, and where edits go.
        assert!(window.find("youtube-visibility").visible());
        assert!(window.find("thumbnail-not-set").visible());
        assert!(window.find("publish-next-upload-note").visible());
    });
}

#[gpui_kit::test]
fn a_video_deleted_on_youtube_offers_a_plain_upload(cx: &mut TestAppContext) {
    let Some(emulators) = Emulators::start() else { return };
    let workspace = Workspace::open(cx, emulators.backend());
    workspace.sign_in(cx, "creator@example.com");
    let service = workspace.service.clone();
    let file = publish_support::video_file(workspace.dir.path(), "final-1.mp4");
    let video_id = workspace.create_video(cx, "Rust Ownership Explained", |video| {
        video.publish.file = Some(file.clone());
        video.publish.privacy = Privacy::Private;
    });
    let upload = service.update(cx, |service, cx| service.start_upload(video_id, cx)).unwrap();
    let youtube_id = wait_until(cx, "the processed upload", |cx| {
        cx.executor().advance_clock(Duration::from_secs(5));
        match service.read_with(cx, |s, _| s.job(upload).map(|job| job.status.clone())) {
            Some(UploadStatus::Done { video_id }) => Some(video_id),
            Some(UploadStatus::Failed { error }) => panic!("upload failed: {error}"),
            _ => None,
        }
    });
    assert!(workspace.video(cx, video_id).youtube.is_some());

    // Deleted in YouTube Studio.
    let token = service.read_with(cx, |s, _| s.client()).unwrap().credentials().access_token;
    emulators.delete_video(&token, &youtube_id);
    let (handle, _panel) = open(cx, |window, cx| cx.new(|cx| PublishPanel::new(video_id, window, cx)));
    // Opening the tab asks YouTube; the answer "no such video" drops the dead link.
    wait_until(cx, "the dropped link", |cx| workspace.video(cx, video_id).youtube.is_none().then_some(()));
    let job = service.read_with(cx, |s, _| s.job(upload).cloned()).unwrap();
    assert!(service.read_with(cx, |s, cx| s.is_off_youtube(&job, cx)));
    window(cx, handle, |window, cx| {
        window.clear_notifications(cx);
        window.render_frame(cx);
        assert!(window.find("youtube-video-gone").visible());
        assert!(window.try_find("open-on-youtube").is_none());
        assert!(window.try_find("publish-next-upload-note").is_none());
        // Not "Upload again…": there is no first copy left to warn about.
        window.click("upload-to-youtube", cx);
        assert!(!window.has_active_dialog(cx));
    });
    wait_until(cx, "the new upload", |cx| {
        service.read_with(cx, |s, _| s.latest_job(video_id).filter(|job| job.id != upload).map(|_| ()))
    });

    // The Uploads page no longer links to the deleted video.
    let (handle, _page) = open(cx, |window, cx| cx.new(|cx| UploadsPage::new(window, cx)));
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.find(format!("upload-{upload}")).visible());
        assert!(window.try_find(format!("upload-{upload}-open")).is_none());
    });
}

/// Drops `paths` from the file manager onto the element `id`.
fn drop_files(window: &mut Window, id: &'static str, paths: Vec<std::path::PathBuf>, cx: &mut App) {
    let position = window.find(id).bounds().center();
    let paths = ExternalPaths(paths.into());
    window.dispatch_event(PlatformInput::FileDrop(FileDropEvent::Entered { position, paths }), cx);
    window.dispatch_event(PlatformInput::FileDrop(FileDropEvent::Submit { position }), cx);
}

#[gpui_kit::test]
fn the_publish_tab_asks_youtube_again_when_it_is_shown_again(cx: &mut TestAppContext) {
    let Some(emulators) = Emulators::start() else { return };
    let workspace = Workspace::open(cx, emulators.backend());
    workspace.sign_in(cx, "creator@example.com");
    let service = workspace.service.clone();
    let file = publish_support::video_file(workspace.dir.path(), "final-1.mp4");
    let video_id = workspace.create_video(cx, "Why Every Developer Should Learn SQLite", |video| {
        video.publish.file = Some(file.clone());
        video.publish.privacy = Privacy::Private;
    });
    let upload = |cx: &mut TestAppContext| {
        let upload = service.update(cx, |service, cx| service.start_upload(video_id, cx)).unwrap();
        wait_until(cx, "the processed upload", |cx| {
            cx.executor().advance_clock(Duration::from_secs(5));
            match service.read_with(cx, |s, _| s.job(upload).map(|job| job.status.clone())) {
                Some(UploadStatus::Done { video_id }) => Some(video_id),
                Some(UploadStatus::Failed { error }) => panic!("upload failed: {error}"),
                _ => None,
            }
        })
    };
    let token = |cx: &mut TestAppContext| service.read_with(cx, |s, _| s.client()).unwrap().credentials().access_token;
    let linked = |cx: &mut TestAppContext| workspace.video(cx, video_id).youtube.is_some();
    let first = upload(cx);

    // The page shows the Publish tab; it asked YouTube when it opened.
    workspace.state.update(cx, |state, cx| state.open_video(video_id, VideoTab::Publish, cx));
    let (handle, _panel) = open(cx, |window, cx| cx.new(|cx| PublishPanel::new(video_id, window, cx)));
    window(cx, handle, |window, _| window.activate_window());
    wait_until(cx, "YouTube's answer", |cx| service.read_with(cx, |s, _| s.youtube_video(&first).map(drop)));

    // Deleted in YouTube Studio. Switching tabs right away keeps the answer
    // that just came: each question costs quota.
    emulators.delete_video(&token(cx), &first);
    let show_again = |cx: &mut TestAppContext| {
        workspace.state.update(cx, |state, cx| {
            state.open_video(video_id, VideoTab::Script, cx);
            state.open_video(video_id, VideoTab::Publish, cx);
        })
    };
    show_again(cx);
    cx.run_until_parked();
    std::thread::sleep(Duration::from_millis(300));
    cx.run_until_parked();
    assert!(linked(cx), "asked again too soon");
    // A little later, showing the tab again asks YouTube, which has no such video.
    cx.executor().advance_clock(Duration::from_secs(30));
    show_again(cx);
    wait_until(cx, "the dropped link", |cx| (!linked(cx)).then_some(()));

    // Uploaded again and deleted again, while the creator is in the browser:
    // coming back to VideoCRM asks YouTube.
    let second = upload(cx);
    assert!(linked(cx));
    emulators.delete_video(&token(cx), &second);
    let (browser, _) = open(cx, |_, cx| cx.new(|_| gpui_kit::EmptyView));
    window(cx, browser, |window, _| window.activate_window());
    cx.run_until_parked();
    window(cx, handle, |window, _| window.activate_window());
    wait_until(cx, "the dropped link", |cx| (!linked(cx)).then_some(()));
}

#[gpui_kit::test]
fn files_dropped_on_the_cards_become_the_video_and_thumbnail(cx: &mut TestAppContext) {
    let workspace = Workspace::open(cx, YoutubeBackend::default());
    let video_id = workspace.create_video(cx, "Desk setup 2026", |_| {});
    let take = publish_support::video_file(workspace.dir.path(), "take1.mov");
    let image = publish_support::png_file(workspace.dir.path(), "thumbnail.png");
    let notes = workspace.dir.path().join("notes.txt");
    std::fs::write(&notes, "not an image").unwrap();
    let (handle, _panel) = open(cx, |window, cx| cx.new(|cx| PublishPanel::new(video_id, window, cx)));
    // Without a working file chooser (no desktop portal), dropping is the way in.
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        drop_files(window, "video-file-drop", vec![take.clone()], cx);
        window.render_frame(cx);
        drop_files(window, "thumbnail-drop", vec![notes.clone()], cx);
        // Not an image: refused with a note, nothing is stored.
        assert_eq!(window.notifications(cx).len(), 1);
        window.clear_notifications(cx);
        window.render_frame(cx);
        drop_files(window, "thumbnail-drop", vec![image.clone()], cx);
    });
    settle(cx);
    let publish = workspace.video(cx, video_id).publish;
    assert_eq!(publish.file, Some(take));
    assert_eq!(publish.thumbnail, Some(image));
}
