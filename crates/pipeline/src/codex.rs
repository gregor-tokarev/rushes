//! The Codex adapter for editing and writing YouTube details.

use std::ffi::OsString;
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::agent::{AgentConfig, AgentRun, Invocation, ProcessSpec, run_process};
use crate::{AgentEvent, PipelineError, parse_codex_line};

pub(crate) fn run(
    config: &AgentConfig,
    program: &Path,
    invocation: &Invocation,
    on_event: &mut dyn FnMut(&AgentEvent),
    cancel: &AtomicBool,
) -> Result<(ExitStatus, AgentRun), PipelineError> {
    let env: Vec<_> = invocation.env.iter().chain(&config.env).cloned().collect();
    let mut args: Vec<OsString> = [
        "exec",
        "--json",
        "--skip-git-repo-check",
        if invocation.one_off { "--sandbox=read-only" } else { "--sandbox=workspace-write" },
        "--config=approval_policy=\"never\"",
    ]
    .map(OsString::from)
    .to_vec();
    let mut cd = OsString::from("--cd=");
    cd.push(&invocation.cwd);
    args.push(cd);
    if let Some(model) = &config.model {
        args.push(format!("--model={model}").into());
    }
    // Codex merges server tables, even an empty override. Explicitly disable
    // configured servers before adding ours. Dummy transports avoid copying any
    // user credentials into process arguments. Read configuration locally through
    // app-server: `mcp list` also probes remote OAuth discovery endpoints.
    let configured_servers = read_config(program, invocation, &env, cancel)?;
    let mut servers = serde_json::Map::new();
    for name in configured_servers.keys() {
        servers.insert(name.clone(), serde_json::json!({"command": "rushes-unused-mcp-server", "enabled": false}));
    }
    let configured = invocation
        .mcp_config
        .as_deref()
        .and_then(|json| serde_json::from_str::<Value>(json).ok())
        .and_then(|mut json| json.get_mut("mcpServers").map(Value::take))
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some(configured) = configured.as_object() {
        for (name, value) in configured {
            let Some(mut server) = value.as_object().cloned() else { continue };
            server.remove("type");
            server.insert("tool_timeout_sec".to_owned(), 3600.into());
            server.insert("enabled".to_owned(), true.into());
            server.insert("required".to_owned(), true.into());
            server.insert("default_tools_approval_mode".to_owned(), "approve".into());
            // Codex filters the environment of stdio servers. Forward names,
            // never secret values in command-line configuration.
            let mut names: Vec<_> = config.env.iter().chain(&invocation.env).map(|(name, _)| name.clone()).collect();
            names.push("ELEVENLABS_API_KEY".to_owned());
            names.sort();
            names.dedup();
            server.insert("env_vars".to_owned(), serde_json::json!(names));
            servers.insert(name.clone(), Value::Object(server));
        }
    }
    args.push(format!("--config=mcp_servers={}", toml_literal(&Value::Object(servers))).into());
    // Plugin MCP servers are not in config/read's server table. This agent gets
    // Rushes' tools only, just as Claude does with --strict-mcp-config.
    args.push("--config=features.plugins=false".into());
    args.push("--config=features.apps=false".into());
    args.push("--config=web_search=\"disabled\"".into());
    if invocation.one_off {
        args.push("--ephemeral".into());
        args.push("--config=features.shell_tool=false".into());
    } else {
        // video-use transcribes through its shell; it needs the same network
        // access as Rushes' MCP server when it calls ElevenLabs.
        args.push("--config=sandbox_workspace_write.network_access=true".into());
        for dir in &invocation.add_dirs {
            let mut arg = OsString::from("--add-dir=");
            arg.push(dir);
            args.push(arg);
        }
    }
    // Keep the schema alive until the child exits, including on cancellation.
    let mut schema_file = invocation.json_schema.as_ref().map(|_| tempfile::NamedTempFile::new()).transpose()?;
    if let (Some(schema), Some(file)) = (&invocation.json_schema, &mut schema_file) {
        file.write_all(schema.as_bytes())?;
        let mut arg = OsString::from("--output-schema=");
        arg.push(file.path());
        args.push(arg);
    }
    args.extend(config.extra_args.iter().map(OsString::from));
    if let Some(session_id) = &invocation.resume {
        args.extend(["resume", "--", session_id, "-"].map(OsString::from));
    } else {
        args.push("-".into());
    }
    let prompt =
        [invocation.system_prompt.as_deref(), config.append_system_prompt.as_deref(), Some(invocation.prompt.as_str())]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("\n\n");
    run_process(
        &ProcessSpec {
            program,
            args,
            cwd: &invocation.cwd,
            env: &env,
            stdin: Some(prompt),
            kill_grace: config.kill_grace,
            parse: parse_codex_line,
        },
        on_event,
        cancel,
    )
}

/// Reads effective configuration without starting a session or contacting MCP
/// servers. Never forwards configuration (which may contain secrets) to the UI.
fn read_config(
    program: &Path,
    invocation: &Invocation,
    env: &[(String, String)],
    cancel: &AtomicBool,
) -> Result<serde_json::Map<String, Value>, PipelineError> {
    let child = Command::new(program)
        .arg("app-server")
        .arg("--config=features.plugins=false")
        .arg("--config=features.apps=false")
        .current_dir(&invocation.cwd)
        .envs(env.iter().map(|(key, value)| (key, value)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut reader = ConfigReader(child);
    let mut input = reader.0.stdin.take().expect("piped stdin");
    let output = reader.0.stdout.take().expect("piped stdout");
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new().name("codex-config".to_owned()).spawn(move || {
        for line in BufReader::new(output).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    })?;
    writeln!(
        input,
        "{}",
        serde_json::json!({"id": 1, "method": "initialize", "params": {
            "clientInfo": {"name": "rushes", "version": env!("CARGO_PKG_VERSION")}
        }})
    )?;
    input.flush()?;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if cancel.load(Ordering::Relaxed) {
            return Err(PipelineError::Cancelled);
        }
        let line = match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(line) => line,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        };
        let Ok(value) = serde_json::from_str::<Value>(&line) else { continue };
        if value.get("error").is_some() {
            break;
        }
        match value.get("id").and_then(Value::as_u64) {
            Some(1) => {
                writeln!(input, "{}", serde_json::json!({"method": "initialized"}))?;
                writeln!(
                    input,
                    "{}",
                    serde_json::json!({"id": 2, "method": "config/read", "params": {
                        "includeLayers": false, "cwd": invocation.cwd
                    }})
                )?;
                input.flush()?;
            }
            Some(2) => {
                if let Some(config) = value.pointer("/result/config").and_then(Value::as_object) {
                    return Ok(config.get("mcp_servers").and_then(Value::as_object).cloned().unwrap_or_default());
                }
                break;
            }
            _ => {}
        }
    }
    Err(std::io::Error::other("Couldn’t read Codex’s configuration. Update Codex and try again.").into())
}

struct ConfigReader(Child);

impl Drop for ConfigReader {
    fn drop(&mut self) {
        _ = self.0.kill();
        _ = self.0.wait();
    }
}

/// JSON's strings, numbers and arrays are TOML literals too; objects need `=`.
fn toml_literal(value: &Value) -> String {
    match value {
        Value::Object(fields) => format!(
            "{{{}}}",
            fields
                .iter()
                .map(|(key, value)| format!("{}={}", serde_json::json!(key), toml_literal(value)))
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Array(values) => format!("[{}]", values.iter().map(toml_literal).collect::<Vec<_>>().join(",")),
        value => value.to_string(),
    }
}
