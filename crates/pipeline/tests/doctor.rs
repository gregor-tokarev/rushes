//! `doctor()` against a fake `PATH` of stub tools.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use videocrm_pipeline::{AgentConfig, doctor};

const SECRET: &str = "sk_test_do_not_leak_123";

struct Machine {
    _root: TempDir,
    base: PathBuf,
    bin: PathBuf,
}

impl Machine {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let base = fs::canonicalize(root.path()).expect("canonical tempdir");
        let bin = base.join("bin");
        fs::create_dir(&bin).unwrap();
        Self { _root: root, base, bin }
    }

    /// An executable shell script in the fake `bin/`.
    fn stub(&self, name: &str, script: &str) -> PathBuf {
        let path = self.bin.join(name);
        fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// The full toolchain: agent, ffmpeg, ffprobe.
    fn with_tools(self) -> Self {
        self.stub("claude", "echo '9.9.9 (Claude Code)'");
        self.stub("ffmpeg", "echo 'ffmpeg version 9.9-fake Copyright'");
        self.stub("ffprobe", "echo 'ffprobe version 9.9-fake Copyright'");
        self
    }

    /// A skill checkout with `SKILL.md`, a virtualenv and `.env`, reached through a symlink.
    fn skill(&self, dotenv: &str) -> (PathBuf, PathBuf) {
        let real = self.base.join("video-use-checkout");
        fs::create_dir_all(real.join(".venv/bin")).unwrap();
        fs::write(real.join("SKILL.md"), "# video-use\n").unwrap();
        fs::write(real.join(".venv/bin/python"), "").unwrap();
        fs::write(real.join(".env"), dotenv).unwrap();
        let link = self.base.join("video-use");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        (link, real)
    }

    /// The agent sees only the fake `bin/` and no ElevenLabs key in its environment.
    fn config(&self) -> AgentConfig {
        AgentConfig {
            env: vec![
                ("PATH".to_owned(), self.bin.display().to_string()),
                ("ELEVENLABS_API_KEY".to_owned(), String::new()),
            ],
            ..AgentConfig::default()
        }
    }
}

#[test]
fn everything_installed_is_ready() {
    let machine = Machine::new().with_tools();
    let (link, real) = machine.skill(&format!("# keys\nELEVENLABS_API_KEY={SECRET}\n"));
    let report = doctor(&machine.config(), Some(&link));

    assert!(report.ready(), "{report:#?}");
    assert_eq!(report.agent.detail, format!("Claude Code 9.9.9 · {}", machine.bin.join("claude").display()));
    assert_eq!(report.skill.detail, real.display().to_string());
    assert_eq!(report.skill_dir.as_deref(), Some(real.as_path()));
    assert_eq!(report.ffmpeg.detail, format!("ffmpeg 9.9 · {}", machine.bin.join("ffmpeg").display()));
    assert_eq!(report.ffprobe.detail, format!("ffprobe 9.9 · {}", machine.bin.join("ffprobe").display()));
    assert_eq!(report.elevenlabs.detail, format!("ELEVENLABS_API_KEY is set in {}", real.join(".env").display()));
    assert!(report.python.detail.starts_with("video-use virtualenv"));
    assert!(!format!("{report:?}").contains(SECRET), "the key must never leave the .env file");
}

#[test]
fn nothing_installed_is_not_ready() {
    let machine = Machine::new();
    let report = doctor(&machine.config(), Some(&machine.base.join("missing-skill")));

    assert!(!report.ready());
    for (name, check) in [
        ("agent", &report.agent),
        ("skill", &report.skill),
        ("ffmpeg", &report.ffmpeg),
        ("ffprobe", &report.ffprobe),
        ("elevenlabs", &report.elevenlabs),
        ("python", &report.python),
    ] {
        assert!(!check.ok, "{name} should fail: {check:?}");
    }
    assert_eq!(report.agent.detail, "claude not found");
    assert!(report.skill.detail.contains("missing-skill/SKILL.md not found"));
    assert_eq!(report.ffmpeg.detail, "ffmpeg not found on PATH");
    for check in [&report.agent, &report.skill, &report.ffmpeg, &report.elevenlabs, &report.python] {
        assert!(!check.detail.contains('`'), "details are plain text: {}", check.detail);
    }
    assert_eq!(report.skill_dir, None);
}

#[test]
fn empty_key_and_broken_agent_are_reported() {
    let machine = Machine::new().with_tools();
    machine.stub("claude", "exit 1");
    let (link, _) = machine.skill("ELEVENLABS_API_KEY=\n");
    let report = doctor(&machine.config(), Some(&link));

    assert!(!report.ready());
    assert!(!report.agent.ok);
    assert!(report.agent.detail.contains("does not run"));
    assert!(!report.elevenlabs.ok);
    assert!(report.elevenlabs.detail.contains("is empty in"));
    assert!(report.skill.ok && report.ffmpeg.ok && report.python.ok);
}

/// video-use's helpers read the skill's `.env` before the environment, so an empty
/// line there (`cp .env.example .env`) hides a key exported in the environment.
#[test]
fn empty_dotenv_line_shadows_the_environment() {
    let machine = Machine::new().with_tools();
    let (link, real) = machine.skill("ELEVENLABS_API_KEY=\n");
    let mut config = machine.config();
    config.env.push(("ELEVENLABS_API_KEY".to_owned(), SECRET.to_owned()));
    let report = doctor(&config, Some(&link));

    assert!(!report.ready());
    assert_eq!(
        report.elevenlabs.detail,
        format!(
            "ELEVENLABS_API_KEY is empty in {}: the skill reads that file before the \
             environment, so fill in the key there or remove the line",
            real.join(".env").display()
        )
    );
    assert!(!format!("{report:?}").contains(SECRET));
}

/// The skill does not understand `export KEY=...` lines, so they do not count.
#[test]
fn exported_dotenv_line_is_not_a_key() {
    let machine = Machine::new().with_tools();
    let (link, _) = machine.skill(&format!("export ELEVENLABS_API_KEY={SECRET}\n"));
    let report = doctor(&machine.config(), Some(&link));

    assert!(!report.elevenlabs.ok);
    assert!(report.elevenlabs.detail.contains("is missing"), "{report:?}");
    assert!(!format!("{report:?}").contains(SECRET));
}

#[test]
fn key_from_the_environment_and_python_fallbacks() {
    let machine = Machine::new().with_tools();
    let skill = machine.base.join("bare-skill");
    fs::create_dir(&skill).unwrap();
    fs::write(skill.join("SKILL.md"), "# video-use\n").unwrap();
    let mut config = machine.config();
    config.env.push(("ELEVENLABS_API_KEY".to_owned(), SECRET.to_owned()));

    let without_python = doctor(&config, Some(&skill));
    assert_eq!(without_python.elevenlabs.detail, "ELEVENLABS_API_KEY is set in the environment");
    assert!(!without_python.python.ok);

    machine.stub("python3", "exit 0");
    let python3 = doctor(&config, Some(&skill));
    assert!(python3.python.ok);
    assert!(python3.python.detail.contains("dependencies are not verified"));

    machine.stub("uv", "exit 0");
    let uv = doctor(&config, Some(&skill));
    assert!(uv.ready(), "{uv:#?}");
    assert!(uv.python.detail.starts_with("uv ("));
    assert!(!format!("{uv:?}").contains(SECRET));
}

#[test]
fn agent_command_can_be_a_path() {
    let machine = Machine::new().with_tools();
    let custom = machine.stub("my-claude", "echo '2.1.0 (Claude Code)'");
    let config = AgentConfig { command: custom.clone(), ..machine.config() };
    let report = doctor(&config, Some(Path::new("/nonexistent")));
    assert!(report.agent.ok);
    assert_eq!(report.agent.detail, format!("Claude Code 2.1.0 · {}", custom.display()));
}

/// Any program answers `--version`; only Claude Code can run an edit.
#[test]
fn an_agent_that_is_not_claude_code_is_refused() {
    let machine = Machine::new().with_tools();
    let other = machine.stub("true", "echo 'true (GNU coreutils) 9.7'");
    let config = AgentConfig { command: other.clone(), ..machine.config() };
    let report = doctor(&config, Some(Path::new("/nonexistent")));
    assert!(!report.agent.ok);
    assert_eq!(
        report.agent.detail,
        format!("{} is not Claude Code: --version printed \"true (GNU coreutils) 9.7\"", other.display())
    );
}
