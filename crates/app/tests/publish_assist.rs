//! UI integration tests for the Publish tab's AI help: the agent writing the title,
//! description and tags (`scripts/fake-agent`), and Codex drawing thumbnails to
//! choose from (`scripts/fake-codex`), driven by clicks and typing in headless windows.

mod publish_support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, Entity, TestAppContext, Window, WindowBounds, WindowOptions, px,
    size,
};
use publish_support::{Workspace, wait_until};
use rushes::publish::{DetailsJob, PublishAssistant, PublishPanel, ThumbnailJob};
use rushes_core::settings::YoutubeBackend;
use rushes_core::{Block, BlockKind, DocumentKind, VideoId};

/// The fakes need python3; the fake Codex draws and Rushes converts with ffmpeg.
fn tools_available() -> bool {
    let ok = ["python3", "ffmpeg", "ffprobe"].iter().all(|tool| Command::new(tool).arg("-version").output().is_ok());
    if !ok {
        eprintln!("skipping: python3, ffmpeg and ffprobe are required");
    }
    ok
}

fn script(name: &str) -> String {
    fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts").join(name))
        .expect("fake script")
        .display()
        .to_string()
}

/// A workspace whose agent and Codex are the fakes, and the assistant that runs them.
fn workspace(cx: &mut TestAppContext, env: &[(&str, &str)]) -> (Workspace, Entity<PublishAssistant>) {
    let workspace = Workspace::open(cx, YoutubeBackend::default());
    workspace.state.update(cx, |state, cx| {
        state.update_settings(cx, |settings| {
            settings.pipeline.agent_command = script("fake-agent");
            settings.pipeline.codex_command = script("fake-codex");
        })
    });
    let env: Vec<(String, String)> = env.iter().map(|(key, value)| ((*key).to_owned(), (*value).to_owned())).collect();
    let state = workspace.state.clone();
    let assistant = cx.update(|cx| {
        let assistant = PublishAssistant::init(state, cx);
        assistant.update(cx, |assistant, _| assistant.set_agent_env(env));
        assistant
    });
    (workspace, assistant)
}

fn set_script(workspace: &Workspace, cx: &mut TestAppContext, video_id: VideoId, blocks: Vec<Block>) {
    workspace.state.update(cx, |state, cx| {
        let mut script = state.documents(video_id).into_iter().find(|doc| doc.kind == DocumentKind::Script).unwrap();
        script.blocks = blocks;
        state.save_document(script, cx);
    });
}

fn open_panel(cx: &mut TestAppContext, video_id: VideoId) -> (AnyWindowHandle, Entity<PublishPanel>) {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(Default::default(), size(px(1280.), px(2200.))))),
        ..WindowOptions::default()
    };
    cx.update(|cx| {
        gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| PublishPanel::new(video_id, window, cx)))
            .expect("open the test window")
    })
}

fn window(cx: &mut TestAppContext, handle: AnyWindowHandle, f: impl FnOnce(&mut Window, &mut App)) {
    cx.update_window(handle, |_, window, cx| f(window, cx)).unwrap();
    cx.run_until_parked();
}

fn details_job(cx: &mut TestAppContext, assistant: &Entity<PublishAssistant>, video_id: VideoId) -> Option<DetailsJob> {
    assistant.read_with(cx, |assistant, _| assistant.details(video_id).cloned())
}

fn thumbnail_job(
    cx: &mut TestAppContext,
    assistant: &Entity<PublishAssistant>,
    video_id: VideoId,
) -> Option<ThumbnailJob> {
    assistant.read_with(cx, |assistant, _| assistant.thumbnails(video_id).cloned())
}

#[gpui_kit::test]
fn the_agent_writes_the_details_and_undo_puts_them_back(cx: &mut TestAppContext) {
    if !tools_available() {
        return;
    }
    let (workspace, assistant) = workspace(cx, &[]);
    let old_description = "Old words.\n\nChapters:\n0:00 Intro\n1:10 Starter\n\nMy course: https://example.com/course";
    let video_id = workspace.create_video(cx, "Sourdough in 24 hours", |video| {
        video.publish.description = old_description.into();
        video.publish.tags = vec!["bread".into()];
    });
    set_script(
        &workspace,
        cx,
        video_id,
        vec![Block::new(BlockKind::Heading2, "Starter"), Block::paragraph("Feed it.")],
    );
    let (handle, _panel) = open_panel(cx, video_id);

    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("write-details", cx);
    });
    // The dialog opens with the instructions field focused.
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.input("Mention the course.", cx);
        window.click("write-details-commit", cx);
    });
    wait_until(cx, "the details", |cx| {
        matches!(details_job(cx, &assistant, video_id), Some(DetailsJob::Written(_))).then_some(())
    });

    let publish = workspace.video(cx, video_id).publish;
    assert_eq!(publish.title, "Sourdough in 24 hours, explained");
    // The agent's words, then the chapter list the description had.
    assert_eq!(
        publish.description,
        "Written from the script.\n\nhttps://example.com/course\n\nChapters:\n0:00 Intro\n1:10 Starter"
    );
    assert_eq!(publish.tags, ["fake", "rushes", "as instructed"]);
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("publish-title").value(), Some("Sourdough in 24 hours, explained"));
        assert!(window.find("details-job").visible(), "the line offers Undo");
        window.click("undo-write-details", cx);
    });
    cx.run_until_parked();
    let publish = workspace.video(cx, video_id).publish;
    assert_eq!((publish.title.as_str(), publish.description.as_str()), ("", old_description));
    assert_eq!(publish.tags, ["bread"]);
    assert_eq!(details_job(cx, &assistant, video_id), None);
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("publish-title").value(), Some(""));
        assert!(window.try_find("details-job").is_none());
    });
}

#[gpui_kit::test]
fn typing_over_written_details_drops_the_undo(cx: &mut TestAppContext) {
    if !tools_available() {
        return;
    }
    let (workspace, assistant) = workspace(cx, &[]);
    let video_id = workspace.create_video(cx, "Desk setup", |_| {});
    set_script(&workspace, cx, video_id, vec![Block::paragraph("My desk.")]);
    let (handle, _panel) = open_panel(cx, video_id);
    cx.update(|cx| assistant.update(cx, |assistant, cx| assistant.write_details(video_id, String::new(), cx)))
        .expect("started");
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        // The agent replaces the fields, so they take no typing meanwhile.
        window.click("publish-title", cx);
        window.input("lost", cx);
        assert_eq!(window.find("publish-title").value(), Some(""));
    });
    wait_until(cx, "the details", |cx| {
        matches!(details_job(cx, &assistant, video_id), Some(DetailsJob::Written(_))).then_some(())
    });
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("publish-title", cx);
        window.input("!", cx);
        assert_eq!(window.find("publish-title").value(), Some("Desk setup, explained!"));
    });
    assert_eq!(details_job(cx, &assistant, video_id), None, "Undo would throw the edit away");
}

#[gpui_kit::test]
fn a_failed_write_says_so_and_changes_nothing(cx: &mut TestAppContext) {
    if !tools_available() {
        return;
    }
    let (workspace, assistant) = workspace(cx, &[("FAKE_AGENT_MODE", "fail")]);
    let video_id = workspace.create_video(cx, "Desk setup", |video| video.publish.title = "Mine".into());
    set_script(&workspace, cx, video_id, vec![Block::paragraph("My desk.")]);
    let (handle, _panel) = open_panel(cx, video_id);
    cx.update(|cx| assistant.update(cx, |assistant, cx| assistant.write_details(video_id, "Short.".into(), cx)))
        .expect("started");
    let error = wait_until(cx, "the failure", |cx| match details_job(cx, &assistant, video_id) {
        Some(DetailsJob::Failed(error)) => Some(error),
        _ => None,
    });
    assert!(error.contains("simulated failure"), "{error}");
    assert_eq!(workspace.video(cx, video_id).publish.title, "Mine");
    // Trying again opens the dialog with the instructions given last.
    assert_eq!(assistant.read_with(cx, |assistant, _| assistant.last_instructions(video_id).to_owned()), "Short.");
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("dismiss-write-details", cx);
    });
    assert_eq!(details_job(cx, &assistant, video_id), None);
}

#[gpui_kit::test]
fn the_agent_needs_a_script_or_a_transcript(cx: &mut TestAppContext) {
    let (workspace, assistant) = workspace(cx, &[]);
    let video_id = workspace.create_video(cx, "Nothing yet", |_| {});
    set_script(&workspace, cx, video_id, Vec::new());
    let (handle, _panel) = open_panel(cx, video_id);
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("write-details", cx);
        assert!(!window.has_active_dialog(cx), "nothing to write from");
    });
    let refused =
        cx.update(|cx| assistant.update(cx, |assistant, cx| assistant.write_details(video_id, String::new(), cx)));
    assert_eq!(refused, Err(rushes::publish::AssistError::NothingToWriteFrom));

    // A transcript from an AI edit is enough.
    let footage = workspace.video(cx, video_id).folder.expect("media folder").join("footage");
    fs::create_dir_all(footage.join("edit")).unwrap();
    fs::write(footage.join("edit/takes_packed.md"), "## take1\n[0.00-1.00] Hello.\n").unwrap();
    workspace.state.update(cx, |state, cx| state.reload(cx));
    cx.run_until_parked();
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("write-details", cx);
        assert!(window.has_active_dialog(cx));
    });
}

/// A 2 s test clip.
fn make_clip(path: &Path) {
    let status = Command::new("ffmpeg")
        .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc=duration=2:size=640x360:rate=25"])
        .args(["-pix_fmt", "yuv420p"])
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
}

#[gpui_kit::test]
fn codex_draws_thumbnails_to_choose_from(cx: &mut TestAppContext) {
    if !tools_available() {
        return;
    }
    let argv = tempfile::NamedTempFile::new().unwrap();
    let (workspace, assistant) = workspace(cx, &[("FAKE_CODEX_ARGV_FILE", argv.path().to_str().unwrap())]);
    let video_id = workspace.create_video(cx, "Sourdough in 24 hours", |_| {});
    let folder = workspace.state.update(cx, |state, cx| state.ensure_video_folder(video_id, cx)).unwrap();
    make_clip(&folder.join("footage/take1.mp4"));
    let (handle, _panel) = open_panel(cx, video_id);
    cx.run_until_parked();

    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("generate-thumbnails", cx);
    });
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        // The ideas typed in the focused field, and two of them.
        window.input("Me holding the loaf.", cx);
        window.within("generate-thumbnails-count").click(1usize, cx);
    });
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(window.within("generate-thumbnails-count").find(1usize).checked(), Some(true));
        window.click("generate-thumbnails-commit", cx);
    });
    let job = thumbnail_job(cx, &assistant, video_id);
    assert!(matches!(job, Some(ThumbnailJob::Drawing { count: 2, .. })), "{job:?}");
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        // One request at a time.
        window.click("generate-thumbnails", cx);
        assert!(!window.has_active_dialog(cx));
    });
    wait_until(cx, "the thumbnails", |cx| {
        matches!(thumbnail_job(cx, &assistant, video_id), Some(ThumbnailJob::Drawn { .. })).then_some(())
    });
    assert_eq!(thumbnail_job(cx, &assistant, video_id), Some(ThumbnailJob::Drawn { made: 2, asked: 2, problem: None }));
    // Codex saw frames of the footage.
    let args: Vec<String> = serde_json::from_str(&fs::read_to_string(argv.path()).unwrap()).unwrap();
    assert_eq!(args.iter().filter(|arg| arg.starts_with("--image=")).count(), 3, "{args:?}");

    let dir = fs::canonicalize(folder.join("thumbnails")).unwrap();
    let second: PathBuf = dir.join("thumbnail-2.jpg");
    assert!(second.is_file() && dir.join("thumbnail-1.jpg").is_file());
    wait_until(cx, "the grid", |cx| {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.try_find("drawn-thumbnail-2.jpg").map(drop)
        })
        .unwrap()
    });
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("drawn-thumbnail-2.jpg").label(), Some("Use thumbnail-2.jpg"));
        window.click("drawn-thumbnail-2.jpg", cx);
    });
    cx.executor().advance_clock(std::time::Duration::from_secs(1));
    cx.run_until_parked();
    let chosen = workspace.video(cx, video_id).publish.thumbnail.expect("a thumbnail");
    assert_eq!(fs::canonicalize(chosen).unwrap(), second);
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("drawn-thumbnail-1.jpg", cx);
    });
    cx.executor().advance_clock(std::time::Duration::from_secs(1));
    cx.run_until_parked();
    let chosen = workspace.video(cx, video_id).publish.thumbnail.expect("a thumbnail");
    assert_eq!(fs::canonicalize(chosen).unwrap(), dir.join("thumbnail-1.jpg"));
}

#[gpui_kit::test]
fn cancelling_codex_leaves_no_thumbnails(cx: &mut TestAppContext) {
    if !tools_available() {
        return;
    }
    let (workspace, assistant) = workspace(cx, &[("FAKE_CODEX_DELAY_MS", "300")]);
    let video_id = workspace.create_video(cx, "Desk setup", |_| {});
    let (handle, _panel) = open_panel(cx, video_id);
    let options = rushes::publish::ThumbnailOptions { count: 2, ideas: String::new(), use_frames: false };
    cx.update(|cx| assistant.update(cx, |assistant, cx| assistant.generate_thumbnails(video_id, options, cx)))
        .expect("started");
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("cancel-thumbnails", cx);
    });
    wait_until(cx, "the cancellation", |cx| thumbnail_job(cx, &assistant, video_id).is_none().then_some(()));
    let dir = workspace.video(cx, video_id).folder.unwrap().join("thumbnails");
    assert!(rushes_pipeline::list_thumbnails(&dir).is_empty());
}

#[gpui_kit::test]
fn a_codex_that_fails_says_why(cx: &mut TestAppContext) {
    if !tools_available() {
        return;
    }
    let (workspace, assistant) = workspace(cx, &[("FAKE_CODEX_MODE", "fail")]);
    let video_id = workspace.create_video(cx, "Desk setup", |_| {});
    let (handle, _panel) = open_panel(cx, video_id);
    cx.update(|cx| {
        assistant.update(cx, |assistant, cx| assistant.generate_thumbnails(video_id, Default::default(), cx))
    })
    .expect("started");
    let error = wait_until(cx, "the failure", |cx| match thumbnail_job(cx, &assistant, video_id) {
        Some(ThumbnailJob::Failed(error)) => Some(error),
        _ => None,
    });
    assert_eq!(error, "You've hit your usage limit.");
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.find("thumbnail-job").visible());
        window.click("dismiss-thumbnails", cx);
    });
    assert_eq!(thumbnail_job(cx, &assistant, video_id), None);

    // Without Codex installed, the creator learns where to set it.
    workspace.state.update(cx, |state, cx| {
        state.update_settings(cx, |settings| settings.pipeline.codex_command = "/nonexistent/codex".into())
    });
    cx.update(|cx| {
        assistant.update(cx, |assistant, cx| assistant.generate_thumbnails(video_id, Default::default(), cx))
    })
    .expect("started");
    let error = wait_until(cx, "the failure", |cx| match thumbnail_job(cx, &assistant, video_id) {
        Some(ThumbnailJob::Failed(error)) => Some(error),
        _ => None,
    });
    assert!(error.starts_with("Couldn’t find Codex at “/nonexistent/codex”."), "{error}");
}
