//! Running Claude Code or Codex and streaming their events.

use std::ffi::OsString;
use std::fmt;
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
use crate::process::AgentProcess;

pub const DEFAULT_COMMAND: &str = "claude";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AgentKind {
    #[default]
    Claude,
    Codex,
}
/// Accepts file edits inside the working directories; every other tool must be in
/// [`AgentConfig::allowed_tools`] because nobody can answer a permission prompt.
pub const DEFAULT_PERMISSION_MODE: &str = "acceptEdits";
/// What video-use needs: shell (ffmpeg, python helpers), file tools, the skill
/// loader, sub-agents for animations and a todo list.
pub const DEFAULT_ALLOWED_TOOLS: [&str; 9] =
    ["Bash", "Read", "Write", "Edit", "Glob", "Grep", "Skill", "Agent", "TodoWrite"];

const STDERR_TAIL_LINES: usize = 20;
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// How long to keep reading after the agent exits, in case a leftover child
/// still holds the output pipes open.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Linux: the agent's process group is led by this `sh` script, started with the
/// agent's path as `$0` and its arguments. It gets `SIGTERM` when the thread that
/// started it ends, also when the app is killed (see [`die_with_parent`]), and passes
/// it on to the whole group: the agent and every ffmpeg or python process it started.
/// It waits for the agent itself, so the group keeps the leader that identifies it
/// ([`AgentProcess`]) until the agent is gone, and exits with the agent's status.
/// A background command's stdin would be `/dev/null`; the agent gets the script's
/// own (the prompt, when it comes that way) through descriptor 3.
#[cfg(target_os = "linux")]
const GROUP_LEADER: &str = r#"trap 'trap "" TERM; kill -TERM 0' TERM
exec 3<&0
"$0" "$@" <&3 3<&- &
agent=$!
exec 3<&-
while :; do
  wait "$agent"
  status=$?
  kill -0 "$agent" 2>/dev/null || exit "$status"
done
"#;

/// How to launch the agent CLI.
#[derive(Clone, PartialEq)]
pub struct AgentConfig {
    pub kind: AgentKind,
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
            kind: AgentKind::default(),
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

// `env` often carries API keys, so only the variable names are printed.
impl fmt::Debug for AgentConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let env: Vec<&str> = self.env.iter().map(|(name, _)| name.as_str()).collect();
        f.debug_struct("AgentConfig")
            .field("kind", &self.kind)
            .field("command", &self.command)
            .field("model", &self.model)
            .field("permission_mode", &self.permission_mode)
            .field("allowed_tools", &self.allowed_tools)
            .field("append_system_prompt", &self.append_system_prompt)
            .field("max_turns", &self.max_turns)
            .field("extra_args", &self.extra_args)
            .field("env", &env)
            .field("kill_grace", &self.kill_grace)
            .finish()
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
        // `--add-dir`, `--tools`) use the `--flag=value` form, so no option can swallow
        // an argument. `--resume` takes an optional value, so its id is attached the
        // same way: a separate argument starting with `-` would be parsed as an option
        // instead. A prompt sent on stdin leaves `-p` on its own.
        let mut args: Vec<OsString> = vec!["-p".into()];
        if !invocation.prompt_on_stdin {
            args.push(invocation.prompt.clone().into());
        }
        args.extend(["--output-format".into(), "stream-json".into(), "--verbose".into()]);
        if let Some(session_id) = &invocation.resume {
            args.push(format!("--resume={session_id}").into());
        }
        if let Some(model) = &self.model {
            args.extend(["--model".into(), model.into()]);
        }
        args.extend(["--permission-mode".into(), self.permission_mode.clone().into()]);
        let allowed = invocation.allowed_tools.as_ref().unwrap_or(&self.allowed_tools);
        if !allowed.is_empty() {
            args.push(format!("--allowedTools={}", allowed.join(",")).into());
        }
        if let Some(tools) = &invocation.tools {
            args.push(format!("--tools={}", tools.join(",")).into());
        }
        if let Some(config) = &invocation.mcp_config {
            args.push(format!("--mcp-config={config}").into());
            args.push("--strict-mcp-config".into());
        }
        if let Some(schema) = &invocation.json_schema {
            args.push(format!("--json-schema={schema}").into());
        }
        if invocation.one_off {
            args.push("--no-session-persistence".into());
            if invocation.mcp_config.is_none() {
                args.push("--strict-mcp-config".into());
            }
        }
        for dir in &invocation.add_dirs {
            let mut arg = OsString::from("--add-dir=");
            arg.push(dir);
            args.push(arg);
        }
        let system_prompt: Vec<&str> =
            [invocation.system_prompt.as_deref(), self.append_system_prompt.as_deref()].into_iter().flatten().collect();
        if !system_prompt.is_empty() {
            args.push(format!("--append-system-prompt={}", system_prompt.join("\n\n")).into());
        }
        if let Some(turns) = self.max_turns {
            args.extend(["--max-turns".into(), turns.to_string().into()]);
        }
        args.extend(self.extra_args.iter().map(OsString::from));
        args
    }
}

/// One agent process: a prompt run in a working directory.
#[derive(Default)]
pub(crate) struct Invocation {
    pub cwd: PathBuf,
    pub prompt: String,
    /// Sends the prompt on stdin rather than as an argument, which the system
    /// limits in length (128 KiB on Linux): for prompts that carry transcripts.
    pub prompt_on_stdin: bool,
    pub resume: Option<String>,
    pub add_dirs: Vec<PathBuf>,
    /// `--mcp-config` (JSON); only these MCP servers are loaded.
    pub mcp_config: Option<String>,
    /// Replaces [`AgentConfig::allowed_tools`].
    pub allowed_tools: Option<Vec<String>>,
    /// Goes before [`AgentConfig::append_system_prompt`].
    pub system_prompt: Option<String>,
    /// Environment for the agent, under [`AgentConfig::env`] (which wins).
    pub env: Vec<(String, String)>,
    /// `--tools`: the only built-in tools the agent has; `None` keeps them all.
    pub tools: Option<Vec<String>>,
    /// `--json-schema`: the final answer is JSON matching this schema.
    pub json_schema: Option<String>,
    /// A one-off answer: no session to resume later (`--no-session-persistence`)
    /// and none of the creator's MCP servers (`--strict-mcp-config`).
    pub one_off: bool,
}

/// What the agent reported while it ran.
#[derive(Debug, Default)]
pub(crate) struct AgentRun {
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

/// Runs the agent to completion, reporting every event as it arrives, and returns
/// its exit status with what it reported. Setting `cancel` terminates the agent and
/// everything it started; processes it leaves behind when it exits are terminated too.
pub(crate) fn run_agent(
    config: &AgentConfig,
    program: &Path,
    invocation: &Invocation,
    on_event: &mut dyn FnMut(&AgentEvent),
    cancel: &AtomicBool,
) -> Result<(ExitStatus, AgentRun), PipelineError> {
    if config.kind == AgentKind::Codex {
        return crate::codex::run(config, program, invocation, on_event, cancel);
    }
    // The config's environment comes last, so it wins over the invocation's.
    let env: Vec<(String, String)> = invocation.env.iter().chain(&config.env).cloned().collect();
    let spec = ProcessSpec {
        program,
        args: config.args(invocation),
        cwd: &invocation.cwd,
        env: &env,
        stdin: invocation.prompt_on_stdin.then(|| invocation.prompt.clone()),
        kill_grace: config.kill_grace,
        parse: parse_line,
    };
    run_process(&spec, on_event, cancel)
}

/// How to start an agent CLI and read what it prints.
pub(crate) struct ProcessSpec<'a> {
    pub program: &'a Path,
    pub args: Vec<OsString>,
    pub cwd: &'a Path,
    pub env: &'a [(String, String)],
    /// Written to the agent's stdin, which is then closed; `None` closes it at once.
    pub stdin: Option<String>,
    pub kill_grace: Duration,
    /// Turns one stdout line into events.
    pub parse: fn(&str) -> Vec<AgentEvent>,
}

/// [`run_agent`] for any agent CLI that prints one event per stdout line.
pub(crate) fn run_process(
    spec: &ProcessSpec<'_>,
    on_event: &mut dyn FnMut(&AgentEvent),
    cancel: &AtomicBool,
) -> Result<(ExitStatus, AgentRun), PipelineError> {
    let program = spec.program;
    #[cfg(target_os = "linux")]
    let mut command = {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(GROUP_LEADER).arg(program);
        let app = std::process::id();
        // SAFETY: `die_with_parent` only makes async-signal-safe calls.
        unsafe { std::os::unix::process::CommandExt::pre_exec(&mut command, move || die_with_parent(app)) };
        command
    };
    #[cfg(not(target_os = "linux"))]
    let mut command = Command::new(program);
    command
        .args(&spec.args)
        .current_dir(spec.cwd)
        .envs(spec.env.iter().map(|(key, value)| (key, value)))
        .stdin(if spec.stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Own process group, so cancelling reaches ffmpeg/python grandchildren too.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let child = command.spawn().map_err(|source| PipelineError::Spawn { command: program.to_owned(), source })?;
    let mut agent = RunningAgent { child, kill_grace: spec.kill_grace };

    if let (Some(input), Some(stdin)) = (spec.stdin.clone(), agent.child.stdin.take()) {
        spawn_writer(stdin, input)?;
    }
    let (tx, rx) = mpsc::channel();
    let readers = [
        agent.child.stdout.take().map(|out| spawn_reader(out, tx.clone(), Line::Stdout)),
        agent.child.stderr.take().map(|err| spawn_reader(err, tx, Line::Stderr)),
    ];
    if let Some(error) = readers.into_iter().flatten().find_map(Result::err) {
        return Err(error.into());
    }
    on_event(&AgentEvent::Spawned { process: AgentProcess::of(agent.child.id()) });

    let mut run = AgentRun::default();
    let mut handle = |line: Line, run: &mut AgentRun| {
        let events = match line {
            Line::Stdout(line) => (spec.parse)(&line),
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
        if let Some(status) = agent.child.try_wait()? {
            break status;
        }
    };
    // Whatever the agent left running (a backgrounded ffmpeg, a helper script) would
    // keep going and hold the output pipes open.
    drop(agent);

    let deadline = Instant::now() + DRAIN_TIMEOUT;
    while streams_open && let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(line) => handle(line, &mut run),
            Err(_) => streams_open = false,
        }
    }
    Ok((status, run))
}

/// Writes `input` to the agent's stdin on a thread of its own, so a prompt larger
/// than the pipe never blocks reading the agent's output, then closes it. An agent
/// that exits without reading it all ends the write.
fn spawn_writer(mut stdin: impl std::io::Write + Send + 'static, input: String) -> std::io::Result<()> {
    thread::Builder::new()
        .name("agent-input".to_owned())
        .spawn(move || {
            _ = stdin.write_all(input.as_bytes());
        })
        .map(drop)
}

fn spawn_reader(stream: impl Read + Send + 'static, tx: Sender<Line>, wrap: fn(String) -> Line) -> std::io::Result<()> {
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

/// Linux, in the forked group leader before it runs: asks for `SIGTERM` once the
/// app's thread that started it ends, so the agent never outlives the app.
#[cfg(target_os = "linux")]
fn die_with_parent(app: u32) -> std::io::Result<()> {
    // SAFETY: prctl and getppid are async-signal-safe and take no pointers.
    if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // The app may have died before the signal was asked for.
    if unsafe { libc::getppid() } as u32 != app {
        return Err(std::io::Error::other("Rushes exited while starting the agent"));
    }
    Ok(())
}

/// The running agent. Every way out of [`run_agent`] (exit, cancellation, an error,
/// a panicking `on_event`) drops it, which terminates what is left of its process group.
struct RunningAgent {
    child: Child,
    kill_grace: Duration,
}

impl Drop for RunningAgent {
    fn drop(&mut self) {
        terminate(&mut self.child, self.kill_grace);
    }
}

/// SIGTERM to the agent's process group, SIGKILL to whatever is left after the
/// grace period, then reap the agent. Returns at once when the group is already gone.
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
    // SAFETY: killpg has no memory-safety preconditions. `group` is the id of the
    // process group we created for the agent. The kernel does not hand that id out
    // again while any member is alive, even after the leader was reaped; once the
    // group is empty, killpg fails with ESRCH.
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
            ..Invocation::default()
        }
    }

    fn strings(args: Vec<OsString>) -> Vec<String> {
        args.into_iter().map(|arg| arg.into_string().expect("utf-8")).collect()
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
                "--resume=abc-123",
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
    fn a_tool_server_replaces_the_tools_and_leads_the_system_prompt() {
        let config = AgentConfig { append_system_prompt: Some("House style.".to_owned()), ..AgentConfig::default() };
        let invocation = Invocation {
            cwd: PathBuf::from("/videos/footage"),
            prompt: "Edit this".to_owned(),
            mcp_config: Some(r#"{"mcpServers":{}}"#.to_owned()),
            tools: Some(vec!["Read".to_owned(), "Write".to_owned()]),
            allowed_tools: Some(vec!["mcp__rushes".to_owned(), "Read".to_owned()]),
            system_prompt: Some("Use the tools.".to_owned()),
            ..Invocation::default()
        };
        let args = strings(config.args(&invocation));
        assert_eq!(
            args[5..],
            [
                "--permission-mode",
                "acceptEdits",
                "--allowedTools=mcp__rushes,Read",
                "--tools=Read,Write",
                r#"--mcp-config={"mcpServers":{}}"#,
                "--strict-mcp-config",
                "--append-system-prompt=Use the tools.\n\nHouse style.",
            ]
        );
    }

    #[test]
    fn a_one_off_answer_reads_its_prompt_from_stdin() {
        let invocation = Invocation {
            prompt_on_stdin: true,
            tools: Some(Vec::new()),
            json_schema: Some(r#"{"type":"object"}"#.to_owned()),
            one_off: true,
            ..invocation(None)
        };
        let config = AgentConfig { allowed_tools: Vec::new(), ..AgentConfig::default() };
        let args = strings(config.args(&invocation));
        assert_eq!(
            args,
            [
                "-p",
                "--output-format",
                "stream-json",
                "--verbose",
                "--permission-mode",
                "acceptEdits",
                "--tools=",
                r#"--json-schema={"type":"object"}"#,
                "--no-session-persistence",
                "--strict-mcp-config",
                "--add-dir=/skills/video-use",
            ]
        );
    }

    #[test]
    fn debug_output_hides_env_values() {
        let config = AgentConfig {
            env: vec![("ANTHROPIC_API_KEY".to_owned(), "sk-ant-secret".to_owned())],
            ..AgentConfig::default()
        };
        let debug = format!("{config:?}");
        assert!(debug.contains("ANTHROPIC_API_KEY"), "{debug}");
        assert!(!debug.contains("sk-ant-secret"), "{debug}");
    }

    #[test]
    fn env_overrides_win_over_the_process_environment() {
        let config = AgentConfig {
            env: vec![
                ("RUSHES_TEST_VAR".to_owned(), "first".to_owned()),
                ("RUSHES_TEST_VAR".to_owned(), "second".to_owned()),
            ],
            ..AgentConfig::default()
        };
        assert_eq!(config.env_var("RUSHES_TEST_VAR"), Some("second".into()));
        assert_eq!(AgentConfig::default().env_var("RUSHES_TEST_VAR"), None);
    }

    #[test]
    fn stderr_tail_keeps_the_last_lines() {
        let mut run = AgentRun::default();
        for n in 0..STDERR_TAIL_LINES + 5 {
            run.observe(&AgentEvent::Stderr { line: n.to_string() });
        }
        assert_eq!(run.stderr_tail.len(), STDERR_TAIL_LINES);
        assert_eq!(run.stderr_tail[0], "5");
    }
}
