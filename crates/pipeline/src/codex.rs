//! The Codex adapter for editing and writing YouTube details.

use std::ffi::{OsStr, OsString};
use std::io::Write as _;
use std::path::Path;
use std::process::ExitStatus;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use serde_json::Value;

use crate::agent::{AgentConfig, AgentRun, Invocation, ProcessSpec, run_process};
use crate::exec::run_captured;
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
    // user credentials into process arguments. `mcp list` never starts servers.
    let listed = run_captured(
        program,
        [OsStr::new("--cd"), invocation.cwd.as_os_str(), OsStr::new("mcp"), OsStr::new("list"), OsStr::new("--json")],
        &env,
        Duration::from_secs(15),
    )
    .and_then(|json| serde_json::from_str::<Vec<Value>>(&json).ok())
    .ok_or_else(|| {
        std::io::Error::other("Couldn’t read Codex’s MCP configuration. Run `codex mcp list --json` to check it.")
    })?;
    let mut servers = serde_json::Map::new();
    for server in listed {
        if let Some(name) = server.get("name").and_then(Value::as_str) {
            servers
                .insert(name.to_owned(), serde_json::json!({"command": "rushes-unused-mcp-server", "enabled": false}));
        }
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
