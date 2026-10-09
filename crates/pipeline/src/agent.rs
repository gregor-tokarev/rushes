//! Running the coding agent (Claude Code in print mode) and streaming its events.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use crate::error::PipelineError;
use crate::event::{AgentEvent, parse_line};
use crate::exec::find_executable;

pub const DEFAULT_COMMAND: &str = "claude";
/// Accepts file edits inside the working directories; every other tool must be in
/// [`AgentConfig::allowed_tools`] because nobody can answer a permission prompt.
pub const DEFAULT_PERMISSION_MODE: &str = "acceptEdits";
/// What video-use needs: shell (ffmpeg, python helpers), file tools, the skill
/// loader, sub-agents for animations and a todo list.
pub const DEFAULT_ALLOWED_TOOLS: [&str; 9] = [
    "Bash",
    "Read",
    "Write",
    "Edit",
    "Glob",
    "Grep",
    "Skill",
    "Agent",
    "TodoWrite",
];

const STDERR_TAIL_LINES: usize = 20;
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// How long to keep reading after the agent exits, in case a leftover child
/// still holds the output pipes open.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// How to launch the agent CLI.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentConfig {
    /// Executable: a bare name looked up on `PATH`, or a path.
    pub command: PathBuf,
    /// `--model` alias or full name; `None` keeps the CLI default.
    pub model: Option<String>,
    /// `--permission-mode`.
    pub permission_mode: String,
    /// `--allowedTools`: tools the agent may use without asking.
    pub allowed_tools: Vec<String>,
    /// `--append-system-prompt`, e.g. the creator's standing instructions.
    pub append_system_prompt: Option<String>,
    /// `--max-turns` safety limit per phase.
    pub max_turns: Option<u32>,
    /// Appended verbatim after the generated arguments.
    pub extra_args: Vec<String>,
    /// Extra environment for the agent and every tool it runs. A `PATH` entry
    /// here is also used to locate the agent itself.
    pub env: Vec<(String, String)>,
    /// How long cancellation waits after SIGTERM before SIGKILL.
    pub kill_grace: Duration,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            command: PathBuf::from(DEFAULT_COMMAND),
            model: None,
            permission_mode: DEFAULT_PERMISSION_MODE.to_owned(),
            allowed_tools: DEFAULT_ALLOWED_TOOLS.map(str::to_owned).to_vec(),
            append_system_prompt: None,
            max_turns: None,
            extra_args: Vec::new(),
            env: Vec::new(),
            kill_grace: Duration::from_secs(5),
        }
    }
}

impl AgentConfig {
    /// Absolute path of the agent executable, if it exists.
    pub fn resolve_command(&self) -> Option<PathBuf> {
        find_executable(&self.command, self.env_var("PATH").as_deref())
    }

    /// The agent's view of an environment variable: [`Self::env`] first, then ours.
    pub(crate) fn env_var(&self, key: &str) -> Option<OsString> {
        self.env
            .iter()
            .rev()
            .find(|(name, _)| name == key)
            .map(|(_, value)| OsString::from(value))
            .or_else(|| std::env::var_os(key))
    }

    fn args(&self, invocation: &Invocation) -> Vec<OsString> {
        // The prompt directly follows `-p`, and the variadic options (`--allowedTools`,
        // `--add-dir`) use the `--flag=value` form, so no option can swallow an argument.
        let mut args: Vec<OsString> = vec![
            "-p".into(),
            invocation.prompt.clone().into(),
            "--output-format".into(),
            "stream-json".into(),
            "--verbose".into(),
        ];
        if let Some(session_id) = &invocation.resume {
            args.extend(["--resume".into(), session_id.into()]);
        }
        if let Some(model) = &self.model {
            args.extend(["--model".into(), model.into()]);
        }
        args.extend([
            "--permission-mode".into(),
            self.permission_mode.clone().into(),
        ]);
        if !self.allowed_tools.is_empty() {
            args.push(format!("--allowedTools={}", self.allowed_tools.join(",")).into());
        }
        for dir in &invocation.add_dirs {
            let mut arg = OsString::from("--add-dir=");
            arg.push(dir);
            args.push(arg);
        }
        if let Some(prompt) = &self.append_system_prompt {
            args.push(format!("--append-system-prompt={prompt}").into());
        }
        if let Some(turns) = self.max_turns {
            args.extend(["--max-turns".into(), turns.to_string().into()]);
        }
        args.extend(self.extra_args.iter().map(OsString::from));
        args
    }
}

/// One agent process: a prompt run in a working directory.
pub(crate) struct Invocation {
    pub cwd: PathBuf,
    pub prompt: String,
    pub resume: Option<String>,
    pub add_dirs: Vec<PathBuf>,
}

/// What a finished agent process reported.
#[derive(Debug, Default)]
pub(crate) struct AgentRun {
    pub status: Option<ExitStatus>,
    pub session_id: Option<String>,
    /// The final [`AgentEvent::Result`], if the agent got that far.
    pub result: Option<AgentEvent>,
    pub last_text: Option<String>,
    pub stderr_tail: Vec<String>,
}

impl AgentRun {
    fn observe(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::Init { session_id, .. } if !session_id.is_empty() => {
                self.session_id = Some(session_id.clone());
            }
            AgentEvent::AssistantText { text } => self.last_text = Some(text.clone()),
            AgentEvent::Result { session_id, .. } => {
                if !session_id.is_empty() {
                    self.session_id = Some(session_id.clone());
                }
                self.result = Some(event.clone());
            }
            AgentEvent::Stderr { line } => {
                if self.stderr_tail.len() == STDERR_TAIL_LINES {
                    self.stderr_tail.remove(0);
                }
                self.stderr_tail.push(line.clone());
            }
            _ => {}
        }
    }
}

enum Line {
    Stdout(String),
    Stderr(String),
}

/// Runs the agent to completion, reporting every event as it arrives. Setting
/// `cancel` terminates the agent and everything it started.
pub(crate) fn run_agent(
    config: &AgentConfig,
    program: &Path,
    invocation: &Invocation,
    on_event: &mut dyn FnMut(&AgentEvent),
    cancel: &AtomicBool,
) -> Result<AgentRun, PipelineError> {
    let mut command = Command::new(program);
    command
        .args(config.args(invocation))
        .current_dir(&invocation.cwd)
        .envs(config.env.iter().map(|(key, value)| (key, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Own process group, so cancelling reaches ffmpeg/python grandchildren too.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = command.spawn().map_err(|source| PipelineError::Spawn {
        command: program.to_owned(),
        source,
    })?;

    let (tx, rx) = mpsc::channel();
    let readers = [
        child
            .stdout
            .take()
            .map(|out| spawn_reader(out, tx.clone(), Line::Stdout)),
        child
            .stderr
            .take()
            .map(|err| spawn_reader(err, tx, Line::Stderr)),
    ];
    if let Some(error) = readers.into_iter().flatten().find_map(Result::err) {
        terminate(&mut child, config.kill_grace);
        return Err(error.into());
    }

    let mut run = AgentRun::default();
    let mut handle = |line: Line, run: &mut AgentRun| {
        let events = match line {
            Line::Stdout(line) => parse_line(&line),
            Line::Stderr(line) => vec![AgentEvent::Stderr { line }],
        };
        for event in &events {
            run.observe(event);
            on_event(event);
        }
    };

    let mut streams_open = true;
    let status = loop {
        if cancel.load(Ordering::Relaxed) {
            terminate(&mut child, config.kill_grace);
            return Err(PipelineError::Cancelled);
        }
        if streams_open {
            match rx.recv_timeout(POLL_INTERVAL) {
                Ok(line) => handle(line, &mut run),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => streams_open = false,
            }
        } else {
            thread::sleep(POLL_INTERVAL);
        }
        if let Some(status) = child.try_wait()? {
            break status;
        }
    };

    let deadline = Instant::now() + DRAIN_TIMEOUT;
    while streams_open && let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(line) => handle(line, &mut run),
            Err(_) => streams_open = false,
        }
    }
    run.status = Some(status);
    Ok(run)
}

fn spawn_reader(
    stream: impl Read + Send + 'static,
    tx: Sender<Line>,
    wrap: fn(String) -> Line,
) -> std::io::Result<()> {
    thread::Builder::new()
        .name("agent-output".to_owned())
        .spawn(move || {
            // Byte lines + lossy decoding: invalid UTF-8 must not end the stream.
            let mut reader = BufReader::new(stream);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let line = String::from_utf8_lossy(&buf);
                        let line = line.trim_end_matches(['\n', '\r']).to_owned();
                        if tx.send(wrap(line)).is_err() {
                            break;
                        }
                    }
                }
            }
        })
        .map(drop)
}

/// SIGTERM to the agent's process group, SIGKILL to whatever is left after the
/// grace period, then reap the agent.
#[cfg(unix)]
fn terminate(child: &mut Child, grace: Duration) {
    let group = child.id() as libc::pid_t;
    signal_group(group, libc::SIGTERM);
    let deadline = Instant::now() + grace;
    loop {
        // Reap the leader so a zombie does not keep the group "alive".
        let _ = child.try_wait();
        if !signal_group(group, 0) {
            break;
        }
        if Instant::now() >= deadline {
            signal_group(group, libc::SIGKILL);
            break;
        }
        thread::sleep(POLL_INTERVAL);
    }
    let _ = child.wait();
}

/// Sends `signal` to every process in `group`; `false` once the group is gone.
#[cfg(unix)]
fn signal_group(group: libc::pid_t, signal: libc::c_int) -> bool {
    // SAFETY: killpg has no memory-safety preconditions. `group` is the id of a
    // process group we created and whose leader we have not reaped before the
    // first signal, so it cannot name an unrelated group.
    unsafe { libc::killpg(group, signal) == 0 }
}

#[cfg(not(unix))]
fn terminate(child: &mut Child, _grace: Duration) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invocation(resume: Option<&str>) -> Invocation {
        Invocation {
            cwd: PathBuf::from("/videos/footage"),
            prompt: "Edit this".to_owned(),
            resume: resume.map(str::to_owned),
            add_dirs: vec![PathBuf::from("/skills/video-use")],
        }
    }

    fn strings(args: Vec<OsString>) -> Vec<String> {
        args.into_iter()
            .map(|arg| arg.into_string().expect("utf-8"))
            .collect()
    }

    #[test]
    fn default_args_are_safe_and_complete() {
        let args = strings(AgentConfig::default().args(&invocation(None)));
        assert_eq!(
            args,
            [
                "-p",
                "Edit this",
                "--output-format",
                "stream-json",
                "--verbose",
                "--permission-mode",
                "acceptEdits",
                "--allowedTools=Bash,Read,Write,Edit,Glob,Grep,Skill,Agent,TodoWrite",
                "--add-dir=/skills/video-use",
            ]
        );
    }

    #[test]
    fn optional_args_follow_the_config() {
        let config = AgentConfig {
            model: Some("sonnet".to_owned()),
            allowed_tools: Vec::new(),
            append_system_prompt: Some("- keep it short".to_owned()),
            max_turns: Some(40),
            extra_args: vec!["--effort".to_owned(), "high".to_owned()],
            ..AgentConfig::default()
        };
        let args = strings(config.args(&invocation(Some("abc-123"))));
        assert_eq!(
            args[5..],
            [
                "--resume",
                "abc-123",
                "--model",
                "sonnet",
                "--permission-mode",
                "acceptEdits",
                "--add-dir=/skills/video-use",
                "--append-system-prompt=- keep it short",
                "--max-turns",
                "40",
                "--effort",
                "high",
            ]
        );
    }

    #[test]
    fn env_overrides_win_over_the_process_environment() {
        let config = AgentConfig {
            env: vec![
                ("VIDEOCRM_TEST_VAR".to_owned(), "first".to_owned()),
                ("VIDEOCRM_TEST_VAR".to_owned(), "second".to_owned()),
            ],
            ..AgentConfig::default()
        };
        assert_eq!(config.env_var("VIDEOCRM_TEST_VAR"), Some("second".into()));
        assert_eq!(AgentConfig::default().env_var("VIDEOCRM_TEST_VAR"), None);
    }

    #[test]
    fn stderr_tail_keeps_the_last_lines() {
        let mut run = AgentRun::default();
        for n in 0..STDERR_TAIL_LINES + 5 {
            run.observe(&AgentEvent::Stderr {
                line: n.to_string(),
            });
        }
        assert_eq!(run.stderr_tail.len(), STDERR_TAIL_LINES);
        assert_eq!(run.stderr_tail[0], "5");
    }
}
