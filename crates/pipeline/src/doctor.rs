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
    /// What was found (version and path) or what is missing, as plain text for people.
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
        [&self.agent, &self.skill, &self.ffmpeg, &self.ffprobe, &self.elevenlabs, &self.python]
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

/// What `claude --version` prints after the version number: `2.1.284 (Claude Code)`.
const CLAUDE_CODE: &str = "Claude Code";

fn check_agent(config: &AgentConfig) -> Check {
    let Some(program) = config.resolve_command() else {
        return Check::fail(format!("{} not found", config.command.display()));
    };
    let Some(output) = run_captured(&program, ["--version"], &config.env, VERSION_TIMEOUT) else {
        return Check::fail(format!("{} does not run: --version failed", program.display()));
    };
    let version = first_line(&output);
    // Any program runs; only Claude Code can run the edit.
    if !version.contains(CLAUDE_CODE) {
        return Check::fail(format!("{} is not Claude Code: --version printed “{version}”", program.display()));
    }
    let number = version.split_whitespace().next().filter(|word| word.starts_with(|c: char| c.is_ascii_digit()));
    match number {
        Some(number) => Check::pass(format!("{CLAUDE_CODE} {number} · {}", program.display())),
        None => Check::pass(format!("{CLAUDE_CODE} · {}", program.display())),
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
        return Check::fail(format!("{name} not found on PATH"));
    };
    match run_captured(&program, ["-version"], &[], VERSION_TIMEOUT) {
        Some(output) => match ffmpeg_version(first_line(&output)) {
            Some(version) => Check::pass(format!("{name} {version} · {}", program.display())),
            None => Check::pass(program.display().to_string()),
        },
        None => Check::fail(format!("{} does not run: -version failed", program.display())),
    }
}

/// The version in ffmpeg's banner, `ffmpeg version 8.0.1-3ubuntu2 Copyright (c) ...`:
/// `8.0.1`, without a distribution's package suffix. Builds from git keep their name
/// (`N-118000-g1234abcd`).
fn ffmpeg_version(banner: &str) -> Option<&str> {
    let mut words = banner.split_whitespace();
    words.find(|word| *word == "version")?;
    let version = words.next()?;
    let release = version.split_once('-').map_or(version, |(release, _)| release);
    let numeric = !release.is_empty() && release.chars().all(|c| c.is_ascii_digit() || c == '.');
    Some(if numeric { release } else { version })
}

/// Mirrors how video-use's `helpers/transcribe.py` finds the key: a `KEY=` line in the
/// skill's `.env` wins, even when empty, and only then the environment counts.
fn check_elevenlabs(config: &AgentConfig, skill_dir: Option<&Path>) -> Check {
    let env_file = skill_dir.map(|dir| dir.join(".env"));
    let in_file = env_file.as_deref().and_then(|path| {
        let text = fs::read_to_string(path).ok()?;
        dotenv_has_value(&text, ELEVENLABS_KEY).map(|filled| (path, filled))
    });
    let in_environment = config.env_var(ELEVENLABS_KEY).is_some_and(|key| !key.to_string_lossy().trim().is_empty());
    match (in_file, env_file.as_deref()) {
        (Some((path, true)), _) => Check::pass(format!("{ELEVENLABS_KEY} is set in {}", path.display())),
        (Some((path, false)), _) => Check::fail(format!(
            "{ELEVENLABS_KEY} is empty in {}: the skill reads that file before the \
             environment, so fill in the key there or remove the line",
            path.display()
        )),
        (None, _) if in_environment => Check::pass(format!("{ELEVENLABS_KEY} is set in the environment")),
        (None, Some(path)) => {
            Check::fail(format!("{ELEVENLABS_KEY} is missing: add it to {} or the environment", path.display()))
        }
        (None, None) => Check::fail(format!("{ELEVENLABS_KEY} is not set in the environment")),
    }
}

/// Whether the first `key=` line of a `.env` file has a non-empty value, parsed like
/// the skill does (`#` comments, surrounding quotes stripped, no `export` prefix);
/// `None` when no line assigns `key`. The value is only tested, never copied out.
fn dotenv_has_value(text: &str, key: &str) -> Option<bool> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .find(|(name, _)| name.trim() == key)
        .map(|(_, value)| !value.trim().trim_matches('"').trim_matches('\'').is_empty())
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
        Check::pass(format!("uv ({}) runs the skill’s helpers", uv.display()))
    } else if let Some(python3) = python3 {
        Check::pass(format!("{} (no virtualenv: the skill’s dependencies are not verified)", python3.display()))
    } else {
        Check::fail("no Python found")
    }
}

fn first_line(text: &str) -> &str {
    text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffmpeg_versions_lose_the_banner_and_package_suffix() {
        let banner = "ffmpeg version 8.0.1-3ubuntu2 Copyright (c) 2000-2025 the FFmpeg developers";
        assert_eq!(ffmpeg_version(banner), Some("8.0.1"));
        assert_eq!(ffmpeg_version("ffprobe version 7.1 Copyright (c) 2007-2024"), Some("7.1"));
        assert_eq!(ffmpeg_version("ffmpeg version N-118000-g1234abcd Copyright"), Some("N-118000-g1234abcd"));
        assert_eq!(ffmpeg_version("something else"), None);
    }

    #[test]
    fn dotenv_detects_assignments_without_reading_values_out() {
        let has = |text: &str| dotenv_has_value(text, ELEVENLABS_KEY);
        assert_eq!(has("ELEVENLABS_API_KEY=sk_123\n"), Some(true));
        assert_eq!(has("# c\n ELEVENLABS_API_KEY = \"x\""), Some(true));
        assert_eq!(has("ELEVENLABS_API_KEY=\n"), Some(false));
        assert_eq!(has("ELEVENLABS_API_KEY=\"\""), Some(false));
        // The first assignment wins, as in the skill.
        assert_eq!(has("ELEVENLABS_API_KEY=\nELEVENLABS_API_KEY=x"), Some(false));
        assert_eq!(has("# ELEVENLABS_API_KEY=x"), None);
        assert_eq!(has("OTHER_ELEVENLABS_API_KEY=x"), None);
        // The skill does not understand `export`, so neither may we.
        assert_eq!(has("export ELEVENLABS_API_KEY=x"), None);
    }
}
