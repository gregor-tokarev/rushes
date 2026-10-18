//! YouTube details and thumbnails against `scripts/fake-agent` and `scripts/fake-codex`.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use rushes_pipeline::{
    AgentConfig, AgentEvent, CodexConfig, DetailsRequest, PipelineError, ThumbnailRequest, VideoDetails,
    generate_thumbnails, list_thumbnails, probe_media, write_details,
};

/// The fakes need python3; the fake Codex draws and Rushes converts with ffmpeg.
fn tools_available() -> bool {
    let ok = ["python3", "ffmpeg", "ffprobe"].iter().all(|tool| Command::new(tool).arg("-version").output().is_ok());
    if !ok {
        eprintln!("skipping: python3, ffmpeg and ffprobe are required");
    }
    ok
}

fn script(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts").join(name)
}

fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs.iter().map(|(key, value)| ((*key).to_owned(), (*value).to_owned())).collect()
}

fn fake_agent(pairs: &[(&str, &str)]) -> AgentConfig {
    AgentConfig { command: script("fake-agent"), env: env(pairs), ..AgentConfig::default() }
}

fn fake_codex(pairs: &[(&str, &str)]) -> CodexConfig {
    CodexConfig { command: script("fake-codex"), env: env(pairs), ..CodexConfig::default() }
}

#[test]
fn details_come_back_from_a_prompt_on_stdin() {
    if !tools_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (prompt_file, argv_file) = (dir.path().join("prompt.txt"), dir.path().join("argv.json"));
    let config = fake_agent(&[
        ("FAKE_AGENT_PROMPT_FILE", prompt_file.to_str().unwrap()),
        ("FAKE_AGENT_ARGV_FILE", argv_file.to_str().unwrap()),
    ]);
    // Larger than one argument may be on Linux: it must go on stdin.
    let transcript = "[0.00-1.00] Feed the starter twice a day.\n".repeat(4_000);
    let request = DetailsRequest {
        video_title: "Sourdough in 24 hours".into(),
        script: "## Starter\nFeed it.".into(),
        transcript: Some(transcript.clone()),
        current: VideoDetails {
            description: "My course: https://example.com/course".into(),
            ..VideoDetails::default()
        },
        instructions: "Mention the course.".into(),
        ..DetailsRequest::default()
    };
    let mut events = Vec::new();
    let written =
        write_details(&config, dir.path(), &request, &mut |event| events.push(event.clone()), &AtomicBool::new(false))
            .expect("details written");
    assert_eq!(written.details.title, "Sourdough in 24 hours, explained");
    assert_eq!(written.details.description, "Written from the transcript.\n\nhttps://example.com/course");
    assert_eq!(written.details.tags, ["fake", "rushes", "as instructed"]);
    assert_eq!(written.cost_usd, Some(0.0123));
    assert!(matches!(events.first(), Some(AgentEvent::Spawned { .. })), "{events:?}");

    let prompt = fs::read_to_string(&prompt_file).unwrap();
    assert!(prompt.contains(transcript.trim()), "the whole transcript arrives");
    let argv: Vec<String> = serde_json::from_str(&fs::read_to_string(&argv_file).unwrap()).unwrap();
    assert_eq!(argv[..2], ["-p", "--output-format"], "no prompt argument: {argv:?}");
    assert!(argv.contains(&"--tools=".to_owned()) && argv.iter().any(|arg| arg.starts_with("--json-schema=")));
}

#[test]
fn an_answer_that_is_no_details_is_an_error() {
    if !tools_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let config = fake_agent(&[("FAKE_AGENT_DETAILS", "I can’t help with that.")]);
    let error = write_details(&config, dir.path(), &DetailsRequest::default(), &mut |_| {}, &AtomicBool::new(false))
        .expect_err("not details");
    assert!(
        matches!(&error, PipelineError::AgentFailed { message, no_result: false, .. } if message.contains("wasn’t a title")),
        "{error:?}"
    );

    let failing = fake_agent(&[("FAKE_AGENT_MODE", "fail")]);
    let error = write_details(&failing, dir.path(), &DetailsRequest::default(), &mut |_| {}, &AtomicBool::new(false))
        .expect_err("failed");
    assert!(
        matches!(&error, PipelineError::AgentFailed { message, .. } if message.contains("simulated failure")),
        "{error:?}"
    );

    let missing = AgentConfig { command: "/nonexistent/claude".into(), ..AgentConfig::default() };
    let error = write_details(&missing, dir.path(), &DetailsRequest::default(), &mut |_| {}, &AtomicBool::new(false))
        .expect_err("no agent");
    assert!(matches!(error, PipelineError::AgentNotFound(_)), "{error:?}");
}

/// A 2 s test clip to take reference frames from.
fn make_clip(path: &Path) {
    let status = Command::new("ffmpeg")
        .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc=duration=2:size=640x360:rate=25"])
        .args(["-pix_fmt", "yuv420p"])
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn codex_drafts_become_numbered_youtube_thumbnails() {
    if !tools_available() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("thumbnails");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("thumbnail-2.jpg"), b"an earlier one").unwrap();
    let clip = root.path().join("final.mp4");
    make_clip(&clip);
    let (prompt_file, argv_file) = (root.path().join("prompt.txt"), root.path().join("argv.json"));
    let config = fake_codex(&[
        ("FAKE_CODEX_HOME", root.path().join("codex").to_str().unwrap()),
        ("FAKE_CODEX_PROMPT_FILE", prompt_file.to_str().unwrap()),
        ("FAKE_CODEX_ARGV_FILE", argv_file.to_str().unwrap()),
    ]);
    let request = ThumbnailRequest {
        dir: dir.clone(),
        video_title: "Sourdough in 24 hours".into(),
        script: "## Starter\nFeed it.".into(),
        message: "Me holding the loaf.".into(),
        frames_from: Some(clip.clone()),
        ..ThumbnailRequest::default()
    };
    let mut events = Vec::new();
    let outcome =
        generate_thumbnails(&config, &request, &mut |event| events.push(event.clone()), &AtomicBool::new(false))
            .expect("thumbnails drawn");

    let dir = fs::canonicalize(&dir).unwrap();
    let expected: Vec<PathBuf> = (3..=5).map(|n| dir.join(format!("thumbnail-{n}.jpg"))).collect();
    assert_eq!(outcome.made, expected, "three ideas when the creator doesn't say how many");
    assert_eq!(outcome.problem, None);
    assert!(outcome.reply.contains("Test pattern number 3."), "{}", outcome.reply);
    assert!(outcome.session_id.is_some(), "the chat's session, for the next turn");
    for path in &outcome.made {
        let media = probe_media(path).expect("an image ffprobe reads");
        assert_eq!((media.width, media.height), (Some(1280), Some(720)), "{}", path.display());
        assert!(fs::metadata(path).unwrap().len() < 2 * 1024 * 1024);
    }
    assert!(!dir.join(".drafts").exists(), "drafts and frames are cleaned up");
    assert_eq!(list_thumbnails(&dir)[..2], [dir.join("thumbnail-5.jpg"), dir.join("thumbnail-4.jpg")]);
    assert!(events.iter().any(|event| matches!(event, AgentEvent::ToolUse { name, .. } if name == "Shell")));

    let argv: Vec<String> = serde_json::from_str(&fs::read_to_string(&argv_file).unwrap()).unwrap();
    let images: Vec<&String> = argv.iter().filter(|arg| arg.starts_with("--image=")).collect();
    assert_eq!(images.len(), 3, "three frames of the video: {argv:?}");
    assert!(!argv.contains(&"resume".to_owned()), "{argv:?}");
    let prompt = fs::read_to_string(&prompt_file).unwrap();
    assert!(prompt.contains("Me holding the loaf.") && prompt.contains("frames from the video"), "{prompt}");
}

#[test]
fn later_turns_resume_the_chat_codex_remembers() {
    if !tools_available() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let (prompt_file, argv_file) = (root.path().join("prompt.txt"), root.path().join("argv.json"));
    let config = fake_codex(&[
        ("FAKE_CODEX_HOME", root.path().join("codex").to_str().unwrap()),
        ("FAKE_CODEX_PROMPT_FILE", prompt_file.to_str().unwrap()),
        ("FAKE_CODEX_ARGV_FILE", argv_file.to_str().unwrap()),
    ]);
    let clip = root.path().join("final.mp4");
    make_clip(&clip);
    let first = ThumbnailRequest {
        dir: root.path().join("thumbnails"),
        script: "## Starter".into(),
        message: "Two ideas with the loaf: 2 ideas.".into(),
        frames_from: Some(clip),
        ..ThumbnailRequest::default()
    };
    let first = generate_thumbnails(&config, &first, &mut |_| {}, &AtomicBool::new(false)).expect("drawn");
    assert_eq!(first.made.len(), 2);
    let session_id = first.session_id.clone().expect("a session");

    // A change draws one picture, in the same session, without the frames again.
    let change = ThumbnailRequest {
        dir: root.path().join("thumbnails"),
        session_id: Some(session_id.clone()),
        script: "## Starter".into(),
        message: "Make number 2 brighter.".into(),
        frames_from: first.made.first().cloned(),
        last_drawn: first.made.clone(),
        ..ThumbnailRequest::default()
    };
    let changed = generate_thumbnails(&config, &change, &mut |_| {}, &AtomicBool::new(false)).expect("drawn");
    let dir = fs::canonicalize(root.path().join("thumbnails")).unwrap();
    assert_eq!(changed.made, [dir.join("thumbnail-3.jpg")]);
    assert_eq!(changed.session_id.as_deref(), Some(session_id.as_str()));
    let argv: Vec<String> = serde_json::from_str(&fs::read_to_string(&argv_file).unwrap()).unwrap();
    assert_eq!(argv[argv.len() - 4..], ["resume", "--", session_id.as_str(), "-"]);
    assert!(!argv.iter().any(|arg| arg.starts_with("--image=")), "{argv:?}");
    let prompt = fs::read_to_string(&prompt_file).unwrap();
    assert!(prompt.starts_with("The creator says:\n<message>\nMake number 2 brighter.\n</message>"), "{prompt}");
    assert!(prompt.contains("thumbnail-1.jpg, thumbnail-2.jpg") && !prompt.contains("<script>"), "{prompt}");

    // A question draws nothing: the answer is what Codex remembers.
    let question = ThumbnailRequest {
        session_id: Some(session_id.clone()),
        message: "What did I ask for first?".into(),
        ..change.clone()
    };
    let answered = generate_thumbnails(&config, &question, &mut |_| {}, &AtomicBool::new(false)).expect("answered");
    assert!(answered.made.is_empty());
    assert_eq!(answered.problem, None);
    assert_eq!(answered.reply, "You first asked: Two ideas with the loaf: 2 ideas.");

    // A session Codex no longer has can't go on.
    let gone = ThumbnailRequest { session_id: Some("01a14e32-0000-7b21-a6e7-786b973d3093".into()), ..question };
    let error = generate_thumbnails(&config, &gone, &mut |_| {}, &AtomicBool::new(false)).expect_err("gone");
    assert!(
        matches!(&error, PipelineError::SessionNotFound(id) if id == "01a14e32-0000-7b21-a6e7-786b973d3093"),
        "{error:?}"
    );
    assert_eq!(list_thumbnails(&dir).len(), 3);
}

#[test]
fn what_codex_drew_before_it_stopped_is_kept() {
    if !tools_available() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let request = ThumbnailRequest { dir: root.path().join("thumbnails"), ..ThumbnailRequest::default() };
    let outcome =
        generate_thumbnails(&fake_codex(&[("FAKE_CODEX_DRAW", "1")]), &request, &mut |_| {}, &AtomicBool::new(false))
            .expect("one of three");
    assert_eq!(outcome.made.len(), 1);
    assert!(outcome.session_id.is_some(), "the chat goes on after a usage limit");
    assert_eq!(outcome.problem.as_deref(), Some("You've hit your usage limit."));

    let error = generate_thumbnails(
        &fake_codex(&[("FAKE_CODEX_MODE", "fail")]),
        &request,
        &mut |_| {},
        &AtomicBool::new(false),
    )
    .expect_err("none drawn");
    assert!(
        matches!(&error, PipelineError::AgentFailed { message, .. } if message == "You've hit your usage limit."),
        "{error:?}"
    );
    assert_eq!(list_thumbnails(&request.dir).len(), 1, "a failed run adds nothing");

    let error = generate_thumbnails(
        &fake_codex(&[("FAKE_CODEX_MODE", "silent")]),
        &request,
        &mut |_| {},
        &AtomicBool::new(false),
    )
    .expect_err("neither a word nor a picture");
    assert!(
        matches!(&error, PipelineError::AgentFailed { message, .. } if message == "Codex finished without answering"),
        "{error:?}"
    );
}

#[test]
fn cancelling_stops_codex_and_keeps_nothing() {
    if !tools_available() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let request = ThumbnailRequest { dir: root.path().join("thumbnails"), ..ThumbnailRequest::default() };
    let config =
        CodexConfig { kill_grace: Duration::from_millis(500), ..fake_codex(&[("FAKE_CODEX_DELAY_MS", "400")]) };
    let cancel = AtomicBool::new(false);
    let started = Instant::now();
    let error = generate_thumbnails(
        &config,
        &request,
        &mut |event| {
            if matches!(event, AgentEvent::AssistantText { .. }) {
                cancel.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        },
        &cancel,
    )
    .expect_err("cancelled");
    assert!(matches!(error, PipelineError::Cancelled), "{error:?}");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(list_thumbnails(&request.dir).is_empty());
    assert!(!request.dir.join(".drafts").exists());
}
