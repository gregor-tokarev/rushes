//! UI integration tests for the Publish tab's AI help: the agent writing the title,
//! description and tags (`scripts/fake-agent`), and Codex drawing thumbnails to
//! choose from in a chat that remembers (`scripts/fake-codex`), driven by clicks and
//! typing in headless windows.

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
use rushes::publish::{ChatMessage, ChatRole, DetailsJob, PublishAssistant, PublishPanel, ThumbnailChat, ThumbnailJob};
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

fn chat(cx: &mut TestAppContext, assistant: &Entity<PublishAssistant>, video_id: VideoId) -> ThumbnailChat {
    assistant.read_with(cx, |assistant, _| assistant.thumbnail_chat(video_id).cloned().unwrap_or_default())
}

fn argv(file: &Path) -> Vec<String> {
    serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap()
}

/// Types `message` into the focused composer and sends it with Enter, then waits for Codex's answer.
fn ask(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    assistant: &Entity<PublishAssistant>,
    video_id: VideoId,
    message: &str,
) -> ChatMessage {
    let before = chat(cx, assistant, video_id).messages.len();
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.input(message, cx);
        window.press("enter", cx);
    });
    wait_until(cx, "Codex's answer", |cx| {
        let chat = chat(cx, assistant, video_id);
        let working = matches!(thumbnail_job(cx, assistant, video_id), Some(ThumbnailJob::Working { .. }));
        (chat.messages.len() == before + 2 && !working).then(|| chat.messages.last().cloned().unwrap())
    })
}

#[gpui_kit::test]
fn codex_draws_thumbnails_in_a_chat_that_remembers(cx: &mut TestAppContext) {
    if !tools_available() {
        return;
    }
    let (argv_file, home) = (tempfile::NamedTempFile::new().unwrap(), tempfile::tempdir().unwrap());
    let (workspace, assistant) = workspace(
        cx,
        &[
            ("FAKE_CODEX_ARGV_FILE", argv_file.path().to_str().unwrap()),
            ("FAKE_CODEX_HOME", home.path().to_str().unwrap()),
            ("FAKE_CODEX_DELAY_MS", "100"),
        ],
    );
    let video_id = workspace.create_video(cx, "Sourdough in 24 hours", |_| {});
    let folder = workspace.state.update(cx, |state, cx| state.ensure_video_folder(video_id, cx)).unwrap();
    make_clip(&folder.join("footage/take1.mp4"));
    let (handle, _panel) = open_panel(cx, video_id);
    cx.run_until_parked();

    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("thumbnail-chat", cx);
    });
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.has_active_sheet(cx));
        assert!(window.find("thumbnail-chat-ideas").visible(), "an empty chat offers a start");
        assert_eq!(window.find("thumbnail-chat-frames").checked(), Some(true));
        // Nothing to send yet.
        window.click("thumbnail-chat-send", cx);
    });
    assert_eq!(chat(cx, &assistant, video_id), ThumbnailChat::default());
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        // The message goes in the focused composer; Enter sends it.
        window.input("Me holding the loaf.", cx);
        window.press("enter", cx);
    });
    let job = thumbnail_job(cx, &assistant, video_id);
    assert!(matches!(job, Some(ThumbnailJob::Working { .. })), "{job:?}");
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        // The next message can be typed, not sent: one at a time.
        window.input("And another", cx);
        window.press("enter", cx);
        window.click("thumbnail-chat-send", cx);
        assert!(window.find("thumbnail-chat-cancel").visible());
        assert!(window.find("cancel-thumbnails").visible(), "the card says Codex works too");
    });
    assert_eq!(chat(cx, &assistant, video_id).messages.len(), 1);
    let answer = wait_until(cx, "Codex's answer", |cx| {
        let chat = chat(cx, &assistant, video_id);
        (chat.messages.len() == 2 && thumbnail_job(cx, &assistant, video_id).is_none()).then_some(chat)
    });
    let dir = fs::canonicalize(folder.join("thumbnails")).unwrap();
    assert_eq!(answer.messages[0].role, ChatRole::Creator);
    assert_eq!(answer.messages[0].text, "Me holding the loaf.");
    let drawn = &answer.messages[1];
    assert_eq!(drawn.role, ChatRole::Codex);
    let expected: Vec<PathBuf> = (1..=3).map(|n| dir.join(format!("thumbnail-{n}.jpg"))).collect();
    assert_eq!(drawn.images, expected, "three ideas when the creator doesn't say how many");
    assert!(drawn.text.contains("Test pattern number 1."), "{}", drawn.text);
    let session_id = answer.session_id.clone().expect("Codex's session");
    // Codex saw frames of the footage.
    let args = argv(argv_file.path());
    assert_eq!(args.iter().filter(|arg| arg.starts_with("--image=")).count(), 3, "{args:?}");

    // A change goes to the same session and draws one picture.
    let changed = ask(cx, handle, &assistant, video_id, " with a red background");
    assert_eq!(chat(cx, &assistant, video_id).messages[2].text, "And another with a red background");
    assert_eq!(changed.images, [dir.join("thumbnail-4.jpg")]);
    let args = argv(argv_file.path());
    assert_eq!(args[args.len() - 4..], ["resume", "--", session_id.as_str(), "-"]);
    assert!(!args.iter().any(|arg| arg.starts_with("--image=")), "{args:?}");

    // A question gets an answer from what Codex remembers, and draws nothing.
    let answered = ask(cx, handle, &assistant, video_id, "What did I ask for first?");
    assert_eq!(answered.text, "You first asked: Me holding the loaf.");
    assert!(answered.images.is_empty() && answered.problem.is_none());
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("thumbnail-chat-frames").is_none(), "frames go with a chat's first message");
    });

    // Choosing a thumbnail in the chat makes it the video's.
    wait_until(cx, "the thumbnails in the chat", |cx| {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.try_find("chat-thumbnail-2.jpg").map(drop)
        })
        .unwrap()
    });
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("chat-thumbnail-2.jpg").label(), Some("Use thumbnail-2.jpg"));
        window.click("chat-thumbnail-2.jpg", cx);
    });
    cx.executor().advance_clock(std::time::Duration::from_secs(1));
    cx.run_until_parked();
    let chosen = workspace.video(cx, video_id).publish.thumbnail.expect("a thumbnail");
    assert_eq!(fs::canonicalize(chosen).unwrap(), dir.join("thumbnail-2.jpg"));

    // The chat is kept: a new start of Rushes has it.
    let kept = chat(cx, &assistant, video_id);
    assert_eq!(kept.messages.len(), 6);
    cx.run_until_parked();
    let state = workspace.state.clone();
    let reopened = cx.update(|cx| PublishAssistant::init(state, cx));
    assert_eq!(chat(cx, &reopened, video_id), kept);
}

#[gpui_kit::test]
fn a_new_chat_starts_over_and_keeps_the_thumbnails(cx: &mut TestAppContext) {
    if !tools_available() {
        return;
    }
    let (argv_file, home) = (tempfile::NamedTempFile::new().unwrap(), tempfile::tempdir().unwrap());
    let (workspace, assistant) = workspace(
        cx,
        &[
            ("FAKE_CODEX_ARGV_FILE", argv_file.path().to_str().unwrap()),
            ("FAKE_CODEX_HOME", home.path().to_str().unwrap()),
        ],
    );
    let video_id = workspace.create_video(cx, "Desk setup", |_| {});
    let (handle, _panel) = open_panel(cx, video_id);
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("thumbnail-chat", cx);
    });
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        // Nothing to forget yet.
        window.click("new-thumbnail-chat", cx);
        assert!(!window.has_active_dialog(cx));
        window.click("thumbnail-chat-ideas", cx);
    });
    wait_until(cx, "the ideas", |cx| (chat(cx, &assistant, video_id).messages.len() == 2).then_some(()));
    assert_eq!(chat(cx, &assistant, video_id).messages[1].images.len(), 3);

    // Codex lost the session: the chat says so, and the next message starts over.
    fs::remove_dir_all(home.path()).unwrap();
    let lost = ask(cx, handle, &assistant, video_id, "One more with a red background.");
    assert_eq!(lost.problem.as_deref(), Some(rushes::publish::CHAT_FORGOTTEN));
    assert_eq!(chat(cx, &assistant, video_id).session_id, None);
    let again = ask(cx, handle, &assistant, video_id, "One more with a red background.");
    assert_eq!(again.images.len(), 3, "a new chat's first message");
    assert!(!argv(argv_file.path()).contains(&"resume".to_owned()));

    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("new-thumbnail-chat", cx);
    });
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.has_active_dialog(cx), "asks first");
        window.click("ok", cx);
    });
    assert_eq!(assistant.read_with(cx, |assistant, _| assistant.thumbnail_chat(video_id).cloned()), None);
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.find("thumbnail-chat-ideas").visible());
        assert!(window.try_find("thumbnail-chat-frames").is_none(), "there's no video to show Codex frames of");
        // Escape from the composer closes the chat.
        window.press("escape", cx);
        assert!(!window.has_active_sheet(cx));
    });
    let dir = workspace.video(cx, video_id).folder.unwrap().join("thumbnails");
    assert_eq!(rushes_pipeline::list_thumbnails(&dir).len(), 6);
}

#[gpui_kit::test]
fn cancelling_codex_leaves_the_message_unanswered(cx: &mut TestAppContext) {
    if !tools_available() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let (workspace, assistant) =
        workspace(cx, &[("FAKE_CODEX_DELAY_MS", "300"), ("FAKE_CODEX_HOME", home.path().to_str().unwrap())]);
    let video_id = workspace.create_video(cx, "Desk setup", |_| {});
    let (handle, _panel) = open_panel(cx, video_id);
    cx.update(|cx| {
        assistant.update(cx, |assistant, cx| assistant.send_thumbnail_message(video_id, "Two ideas".into(), false, cx))
    })
    .expect("started");
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("cancel-thumbnails", cx);
    });
    wait_until(cx, "the cancellation", |cx| thumbnail_job(cx, &assistant, video_id).is_none().then_some(()));
    let dir = workspace.video(cx, video_id).folder.unwrap().join("thumbnails");
    assert!(rushes_pipeline::list_thumbnails(&dir).is_empty());
    let chat = chat(cx, &assistant, video_id);
    assert!(chat.unanswered(), "{chat:?}");
}

#[gpui_kit::test]
fn a_codex_that_stops_early_says_why_on_the_card(cx: &mut TestAppContext) {
    if !tools_available() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let (workspace, assistant) =
        workspace(cx, &[("FAKE_CODEX_DRAW", "1"), ("FAKE_CODEX_HOME", home.path().to_str().unwrap())]);
    let video_id = workspace.create_video(cx, "Desk setup", |_| {});
    let (handle, _panel) = open_panel(cx, video_id);
    cx.update(|cx| {
        assistant.update(cx, |assistant, cx| assistant.send_thumbnail_message(video_id, "3 ideas".into(), false, cx))
    })
    .expect("started");
    let problem = wait_until(cx, "the partial answer", |cx| match thumbnail_job(cx, &assistant, video_id) {
        Some(ThumbnailJob::StoppedEarly(problem)) => Some(problem),
        _ => None,
    });
    assert_eq!(problem, "You've hit your usage limit.");
    let said = chat(cx, &assistant, video_id).messages.last().cloned().unwrap();
    assert_eq!((said.images.len(), said.problem.as_deref()), (1, Some("You've hit your usage limit.")));
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.find("thumbnail-job").visible(), "the card says Codex stopped early");
        window.click("dismiss-thumbnails", cx);
    });
    assert_eq!(thumbnail_job(cx, &assistant, video_id), None);
}

#[gpui_kit::test]
fn a_codex_that_fails_says_why(cx: &mut TestAppContext) {
    if !tools_available() {
        return;
    }
    let (workspace, assistant) = workspace(cx, &[("FAKE_CODEX_MODE", "fail")]);
    let video_id = workspace.create_video(cx, "Desk setup", |_| {});
    let (handle, _panel) = open_panel(cx, video_id);
    let send = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            assistant.update(cx, |assistant, cx| assistant.send_thumbnail_message(video_id, String::new(), false, cx))
        })
        .expect("started")
    };
    send(cx);
    let error = wait_until(cx, "the failure", |cx| match thumbnail_job(cx, &assistant, video_id) {
        Some(ThumbnailJob::Failed(error)) => Some(error),
        _ => None,
    });
    assert_eq!(error, "You've hit your usage limit.");
    let said = chat(cx, &assistant, video_id).messages.last().cloned().unwrap();
    assert_eq!(said.problem.as_deref(), Some("You've hit your usage limit."), "the chat says so too");
    window(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.find("thumbnail-job").visible());
        window.click("dismiss-thumbnails", cx);
    });
    assert_eq!(thumbnail_job(cx, &assistant, video_id), None);
    assert_eq!(chat(cx, &assistant, video_id).messages.len(), 2, "dismissing keeps the chat");

    // Without Codex installed, the creator learns where to set it.
    workspace.state.update(cx, |state, cx| {
        state.update_settings(cx, |settings| settings.pipeline.codex_command = "/nonexistent/codex".into())
    });
    send(cx);
    let error = wait_until(cx, "the failure", |cx| match thumbnail_job(cx, &assistant, video_id) {
        Some(ThumbnailJob::Failed(error)) => Some(error),
        _ => None,
    });
    assert!(error.starts_with("Couldn’t find Codex at “/nonexistent/codex”."), "{error}");
}
