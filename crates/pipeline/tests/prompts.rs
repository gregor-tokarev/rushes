//! Prompt builders: what each phase asks the agent to do.

use std::path::Path;

use videocrm_pipeline::{FootageFile, MediaInfo, Phase, build_prompt};

const SKILL_MD: &str = "/opt/video-use/SKILL.md";

fn footage() -> Vec<FootageFile> {
    vec![
        FootageFile {
            name: "take1.mp4".to_owned(),
            media: Some(MediaInfo {
                duration_secs: 62.4,
                width: Some(1920),
                height: Some(1080),
                has_audio: true,
            }),
        },
        FootageFile {
            name: "broll.mov".to_owned(),
            media: Some(MediaInfo {
                duration_secs: 4.0,
                width: Some(3840),
                height: Some(2160),
                has_audio: false,
            }),
        },
        FootageFile {
            name: "voice.wav".to_owned(),
            media: Some(MediaInfo {
                duration_secs: 5.0,
                width: None,
                height: None,
                has_audio: true,
            }),
        },
        FootageFile {
            name: "unreadable.mkv".to_owned(),
            media: None,
        },
    ]
}

fn prompt(phase: Phase) -> String {
    build_prompt(&phase, Path::new(SKILL_MD), &footage())
}

const BRIEF: &str = "Tutorial: home studio on a budget.\nHook: \"You don't need a $2000 camera.\"\nStyle: fast cuts, no music.";

#[test]
fn plan_prompt_asks_for_a_strategy_and_a_stop() {
    let prompt = prompt(Phase::Plan {
        brief: BRIEF.to_owned(),
    });
    assert!(prompt.starts_with("VideoCRM edit request (planning phase)."));
    for expected in [
        "video-use skill",
        SKILL_MD,
        "raw footage in the current directory",
        "- take1.mp4 (1:02, 1920x1080)\n",
        "- broll.mov (0:04, 3840x2160, no audio)\n",
        "- voice.wav (0:05, audio only)\n",
        "- unreadable.mkv\n",
        "Write every output to ./edit/",
        "Never modify the source files",
        BRIEF,
        "transcribe",
        "plain English",
        "./edit/project.md",
        "STOP. Do not cut or render anything",
        "headless",
        "End with the strategy only",
    ] {
        assert!(
            prompt.contains(expected),
            "missing {expected:?} in:\n{prompt}"
        );
    }
    assert!(!prompt.contains("final.mp4"), "{prompt}");
}

#[test]
fn plan_prompt_without_brief_or_footage() {
    let prompt = build_prompt(
        &Phase::Plan {
            brief: "  \n".to_owned(),
        },
        Path::new(SKILL_MD),
        &[],
    );
    assert!(prompt.contains("The creator gave no brief"));
    assert!(prompt.contains("No media files were found"));
}

#[test]
fn approve_prompt_renders_and_summarises() {
    let prompt = prompt(Phase::Approve { notes: None });
    assert!(prompt.starts_with("VideoCRM edit: strategy approved (render phase)."));
    for expected in [
        "approved the edit strategy",
        SKILL_MD,
        "./edit/final.mp4",
        "./edit/preview.mp4",
        "Self-evaluate",
        "at most 3 passes",
        "./edit/project.md",
        "short summary",
    ] {
        assert!(
            prompt.contains(expected),
            "missing {expected:?} in:\n{prompt}"
        );
    }
    assert!(!prompt.contains("notes"), "{prompt}");
    assert!(
        !prompt.contains("take1.mp4"),
        "the session already knows the footage"
    );

    let blank_notes = build_prompt(
        &Phase::Approve {
            notes: Some(" ".to_owned()),
        },
        Path::new(SKILL_MD),
        &[],
    );
    assert!(!blank_notes.contains("notes"));

    let with_notes = prompt_with_notes("Keep the joke at 0:42.");
    assert!(with_notes.contains("Their notes, to take into account:\n\nKeep the joke at 0:42."));
}

fn prompt_with_notes(notes: &str) -> String {
    prompt(Phase::Approve {
        notes: Some(notes.to_owned()),
    })
}

#[test]
fn feedback_prompt_applies_changes_and_re_renders() {
    let prompt = prompt(Phase::Feedback {
        message: "  Cut the intro to 5 seconds.\n".to_owned(),
    });
    assert!(prompt.starts_with("VideoCRM edit: feedback from the creator (revision phase)."));
    for expected in [
        "asks for these changes:\n\nCut the intro to 5 seconds.\n\n",
        SKILL_MD,
        "Re-render ./edit/preview.mp4 and ./edit/final.mp4",
        "cached transcripts",
        "If nothing has been rendered yet",
        "short summary of what changed",
    ] {
        assert!(
            prompt.contains(expected),
            "missing {expected:?} in:\n{prompt}"
        );
    }
}

#[test]
fn auto_prompt_plans_and_renders_without_waiting() {
    let prompt = prompt(Phase::Auto {
        brief: BRIEF.to_owned(),
    });
    assert!(prompt.starts_with("VideoCRM edit request (single pass, pre-approved)."));
    for expected in [
        SKILL_MD,
        "- take1.mp4 (1:02, 1920x1080)",
        BRIEF,
        "pre-approved this edit, so do not stop to confirm the strategy",
        "./edit/project.md",
        "./edit/final.mp4",
        "Self-evaluate",
        "strategy you chose",
    ] {
        assert!(
            prompt.contains(expected),
            "missing {expected:?} in:\n{prompt}"
        );
    }
    assert!(!prompt.contains("STOP"));
}

#[test]
fn prompts_are_never_mistaken_for_cli_flags() {
    for phase in [
        Phase::Plan {
            brief: "-x".to_owned(),
        },
        Phase::Approve {
            notes: Some("--resume".to_owned()),
        },
        Phase::Feedback {
            message: "--help".to_owned(),
        },
        Phase::Auto {
            brief: "-p".to_owned(),
        },
    ] {
        assert!(!prompt(phase).starts_with('-'));
    }
}
