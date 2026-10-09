//! Readiness checks for everything a pipeline needs on this machine.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::agent::AgentConfig;
use crate::exec::{find_executable, run_captured};

const VERSION_TIMEOUT: Duration = Duration::from_secs(15);
const ELEVENLABS_KEY: &str = "ELEVENLABS_API_KEY";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    pub ok: bool,
    /// What was found (path, version) or what is missing and how to fix it.
    pub detail: String,
}

impl Check {
    fn pass(detail: impl Into<String>) -> Self {
        Self { ok: true, detail: detail.into() }
    }

    fn fail(detail: impl Into<String>) -> Self {
        Self { ok: false, detail: detail.into() }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Doctor {
    /// The agent CLI.
    pub agent: Check,
    /// The video-use skill (`SKILL.md`).
    pub skill: Check,
    pub ffmpeg: Check,
    pub ffprobe: Check,
    /// The ElevenLabs API key video-use transcribes with. The key itself is never read out.
    pub elevenlabs: Check,
    /// A Python able to run the skill's helpers.
    pub python: Check,
    /// The resolved (symlink-free) skill folder, when `SKILL.md` was found.
    pub skill_dir: Option<PathBuf>,
}

impl Doctor {
    pub fn ready(&self) -> bool {
        [
            &self.agent,
            &self.skill,
            &self.ffmpeg,
            &self.ffprobe,
            &self.elevenlabs,
            &self.python,
        ]
        .iter()
        .all(|check| check.ok)
    }
}

/// Where Claude Code looks for the skill: `~/.claude/skills/video-use`.
pub fn default_skill_dir() -> Option<PathBuf> {
    std::env::home_dir().map(|home| home.join(".claude").join("skills").join("video-use"))
}

/// Checks the agent CLI, the video-use skill (`skill_dir`, or [`default_skill_dir`]),
/// ffmpeg/ffprobe, the ElevenLabs key and Python. Tools are looked up on the agent's
/// `PATH` ([`AgentConfig::env`] first), since that is where the agent will run them.
pub fn doctor(config: &AgentConfig, skill_dir: Option<&Path>) -> Doctor {
    let path_var = config.env_var("PATH");
    let find = |name: &str| find_executable(Path::new(name), path_var.as_deref());

    let (skill, resolved_skill_dir) = check_skill(skill_dir);
    Doctor {
        agent: check_agent(config),
        ffmpeg: check_ffmpeg_tool(find("ffmpeg"), "ffmpeg"),
        ffprobe: check_ffmpeg_tool(find("ffprobe"), "ffprobe"),
        elevenlabs: check_elevenlabs(config, resolved_skill_dir.as_deref()),
        python: check_python(resolved_skill_dir.as_deref(), find("uv"), find("python3")),
        skill,
        skill_dir: resolved_skill_dir,
    }
}

fn check_agent(config: &AgentConfig) -> Check {
    let Some(program) = config.resolve_command() else {
        return Check::fail(format!(
            "`{}` not found: install Claude Code or set the agent command",
            config.command.display()
        ));
    };
    match run_captured(&program, ["--version"], &config.env, VERSION_TIMEOUT) {
        Some(version) => Check::pass(format!("{} ({})", program.display(), first_line(&version))),
        None => Check::fail(format!("{} does not run (`--version` failed)", program.display())),
    }
}

fn check_skill(skill_dir: Option<&Path>) -> (Check, Option<PathBuf>) {
    let Some(dir) = skill_dir.map(Path::to_owned).or_else(default_skill_dir) else {
        return (Check::fail("no skill folder configured and no home directory"), None);
    };
    let skill_md = dir.join("SKILL.md");
    match fs::canonicalize(&dir) {
        Ok(real) if skill_md.is_file() => (Check::pass(real.display().to_string()), Some(real)),
        _ => (
            Check::fail(format!(
                "{} not found: install video-use (github.com/browser-use/video-use)",
                skill_md.display()
            )),
            None,
        ),
    }
}

fn check_ffmpeg_tool(program: Option<PathBuf>, name: &str) -> Check {
    let Some(program) = program else {
        return Check::fail(format!("`{name}` not found on PATH: install ffmpeg"));
    };
    match run_captured(&program, ["-version"], &[], VERSION_TIMEOUT) {
        Some(version) => Check::pass(format!("{} ({})", program.display(), first_line(&version))),
        None => Check::fail(format!("{} does not run (`-version` failed)", program.display())),
    }
}

fn check_elevenlabs(config: &AgentConfig, skill_dir: Option<&Path>) -> Check {
    if config
        .env_var(ELEVENLABS_KEY)
        .is_some_and(|key| !key.to_string_lossy().trim().is_empty())
    {
        return Check::pass(format!("{ELEVENLABS_KEY} is set in the environment"));
    }
    let Some(skill_dir) = skill_dir else {
        return Check::fail(format!("{ELEVENLABS_KEY} is not set in the environment"));
    };
    let env_file = skill_dir.join(".env");
    if fs::read_to_string(&env_file).is_ok_and(|text| dotenv_has_value(&text, ELEVENLABS_KEY)) {
        Check::pass(format!("{ELEVENLABS_KEY} is set in {}", env_file.display()))
    } else {
        Check::fail(format!(
            "{ELEVENLABS_KEY} is missing: add it to {} or the environment",
            env_file.display()
        ))
    }
}

/// Whether a `.env` file assigns a non-empty value to `key`. The value is only
/// tested for emptiness, never copied out.
fn dotenv_has_value(text: &str, key: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim();
        let line = line.strip_prefix("export ").unwrap_or(line);
        line.split_once('=').is_some_and(|(name, value)| {
            let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
            name.trim() == key && !value.is_empty()
        })
    })
}

fn check_python(skill_dir: Option<&Path>, uv: Option<PathBuf>, python3: Option<PathBuf>) -> Check {
    let venv_python = skill_dir.and_then(|dir| {
        [".venv/bin/python", ".venv/Scripts/python.exe"]
            .into_iter()
            .map(|relative| dir.join(relative))
            .find(|path| path.is_file())
    });
    if let Some(python) = venv_python {
        Check::pass(format!("video-use virtualenv ({})", python.display()))
    } else if let Some(uv) = uv {
        Check::pass(format!("uv ({}) runs the skill's helpers", uv.display()))
    } else if let Some(python3) = python3 {
        Check::pass(format!(
            "{} (no virtualenv: the skill's dependencies are not verified)",
            python3.display()
        ))
    } else {
        Check::fail("no Python found: install uv and run `uv sync` in the skill folder")
    }
}

fn first_line(text: &str) -> &str {
    text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotenv_detects_assignments_without_reading_values_out() {
        assert!(dotenv_has_value("ELEVENLABS_API_KEY=sk_123\n", ELEVENLABS_KEY));
        assert!(dotenv_has_value("# c\nexport ELEVENLABS_API_KEY = \"x\"", ELEVENLABS_KEY));
        assert!(!dotenv_has_value("ELEVENLABS_API_KEY=\n", ELEVENLABS_KEY));
        assert!(!dotenv_has_value("ELEVENLABS_API_KEY=\"\"", ELEVENLABS_KEY));
        assert!(!dotenv_has_value("# ELEVENLABS_API_KEY=x", ELEVENLABS_KEY));
        assert!(!dotenv_has_value("OTHER_ELEVENLABS_API_KEY=x", ELEVENLABS_KEY));
    }
}
