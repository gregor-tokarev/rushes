//! The Gemini CLI adapter for editing and writing YouTube details.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::atomic::AtomicBool;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use serde_json::{Map, Value};

use crate::agent::{AgentConfig, AgentRun, Invocation, ProcessSpec, run_process};
use crate::event::GeminiStreamParser;
use crate::exec::run_captured_checked;
use crate::{AgentEvent, PipelineError};

pub const DEFAULT_GEMINI_COMMAND: &str = "gemini";

/// How long listing Gemini CLI's models may take: it may refresh auth or catalog metadata.
const MODELS_TIMEOUT: Duration = Duration::from_secs(20);
/// One-hour tool timeout for Rushes' MCP server (a long final render takes minutes).
const MCP_TOOL_TIMEOUT_MS: u64 = 3_600_000;
/// Non-empty MCP server allowlist sentinel that matches no real server, blocking all
/// user-configured MCP servers when a run needs none.
const NO_MCP_SERVERS: &str = "__rushes_none__";
/// Built-in Gemini CLI model aliases, used when an older CLI lacks `--list-models`.
const FALLBACK_MODELS: [(&str, &str); 4] =
    [("auto", "Auto"), ("pro", "Pro"), ("flash", "Flash"), ("flash-lite", "Flash Lite")];

/// A model Gemini CLI offers in its model picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeminiModel {
    /// What `--model` takes, e.g. `gemini-2.5-pro` or `auto`.
    pub slug: String,
    /// The name Gemini CLI shows for it.
    pub display_name: String,
}

/// The models the Gemini CLI in `config` offers, in its own order, from
/// `gemini --list-models --output-format json`. Falls back to the built-in aliases
/// when an older CLI does not recognize `--list-models`.
pub fn gemini_models(config: &AgentConfig) -> Result<Vec<GeminiModel>, String> {
    let program = config.resolve_command().ok_or_else(|| format!("{} not found", config.command.display()))?;
    let mut env = vec![("NO_BROWSER".to_owned(), "1".to_owned())];
    env.extend(config.env.iter().cloned());
    match run_captured_checked(&program, ["--list-models", "--output-format", "json"], &env, MODELS_TIMEOUT) {
        Ok(output) => parse_models(&output).ok_or_else(|| "Gemini CLI printed no model catalog".to_owned()),
        Err(reason) if unsupported_list_models(&reason) => Ok(fallback_models()),
        Err(reason) => Err(reason),
    }
}

fn unsupported_list_models(reason: &str) -> bool {
    let lower = reason.to_ascii_lowercase();
    lower.contains("unknown argument") && lower.contains("list-models")
}

fn fallback_models() -> Vec<GeminiModel> {
    FALLBACK_MODELS
        .iter()
        .map(|&(slug, display_name)| GeminiModel { slug: slug.to_owned(), display_name: display_name.to_owned() })
        .collect()
}

fn parse_models(output: &str) -> Option<Vec<GeminiModel>> {
    let trimmed = output.trim();
    let catalog: Value = serde_json::from_str(trimmed).ok().or_else(|| {
        let start = trimmed.find('[')?;
        let end = trimmed.rfind(']')?;
        serde_json::from_str(trimmed.get(start..=end)?).ok()
    })?;
    let entries = catalog.as_array().or_else(|| catalog.get("models").and_then(Value::as_array))?;
    let mut seen = HashSet::new();
    let mut models = Vec::new();
    for entry in entries {
        let Some(slug) = entry
            .get("modelId")
            .or_else(|| entry.get("id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|slug| !slug.is_empty())
        else {
            continue;
        };
        if !seen.insert(slug.to_owned()) {
            continue;
        }
        let display_name = entry
            .get("displayName")
            .or_else(|| entry.get("name"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(slug);
        models.push(GeminiModel { slug: slug.to_owned(), display_name: display_name.to_owned() });
    }
    Some(models)
}

pub(crate) fn run(
    config: &AgentConfig,
    program: &Path,
    invocation: &Invocation,
    on_event: &mut dyn FnMut(&AgentEvent),
    cancel: &AtomicBool,
) -> Result<(ExitStatus, AgentRun), PipelineError> {
    // Trust the workspace and never pop open a browser window from a headless run;
    // explicit caller overrides in `config.env` come last and still win.
    let mut env: Vec<(String, String)> =
        vec![("GEMINI_CLI_TRUST_WORKSPACE".to_owned(), "true".to_owned()), ("NO_BROWSER".to_owned(), "1".to_owned())];
    env.extend(invocation.env.iter().cloned());
    env.extend(config.env.iter().cloned());

    let mut args: Vec<OsString> = vec!["--output-format=stream-json".into(), "-p".into(), String::new().into()];
    if let Some(model) = &config.model {
        args.extend(["--model".into(), model.into()]);
    }
    if let Some(session_id) = &invocation.resume {
        args.push(format!("--resume={session_id}").into());
    }
    // Disable user-installed extensions so only the tools Rushes configures are active.
    args.push("--extensions=none".into());

    let _one_off_dir;
    let _workspace_guard;
    let cwd: PathBuf;

    if invocation.one_off {
        // One-off requests (YouTube details) need no tools and nothing from the footage
        // folder. Running in a private temporary folder disables built-in tools via
        // `tools.core = []` without touching the user's directory or session history.
        let dir = tempfile::tempdir()?;
        write_one_off_settings(dir.path())?;
        args.push("--approval-mode=default".into());
        args.push(format!("--allowed-mcp-server-names={NO_MCP_SERVERS}").into());
        cwd = dir.path().to_path_buf();
        _one_off_dir = Some(dir);
        _workspace_guard = None;
    } else {
        args.push("--approval-mode=yolo".into());
        for dir in &invocation.add_dirs {
            let mut arg = OsString::from("--include-directories=");
            arg.push(dir);
            args.push(arg);
        }
        let (servers, allowed_env_vars) = gemini_mcp_servers(invocation, &env);
        if servers.is_empty() {
            args.push(format!("--allowed-mcp-server-names={NO_MCP_SERVERS}").into());
            _workspace_guard = None;
        } else {
            let allowed_names: Vec<String> = servers.keys().cloned().collect();
            args.push(format!("--allowed-mcp-server-names={}", allowed_names.join(",")).into());
            _workspace_guard = Some(WorkspaceSettingsGuard::install(&invocation.cwd, servers, allowed_env_vars)?);
        }
        cwd = invocation.cwd.clone();
        _one_off_dir = None;
    }

    args.extend(config.extra_args.iter().map(OsString::from));

    let prompt = build_prompt(config, invocation);
    let mut parser = GeminiStreamParser::new();
    let mut parse = |line: Option<&str>| match line {
        Some(line) => parser.parse_line(line),
        None => parser.finish(),
    };
    run_process(
        ProcessSpec {
            program,
            args,
            cwd: &cwd,
            env: &env,
            stdin: Some(prompt),
            kill_grace: config.kill_grace,
            parse: &mut parse,
        },
        on_event,
        cancel,
    )
}

fn build_prompt(config: &AgentConfig, invocation: &Invocation) -> String {
    let mut parts: Vec<String> =
        [invocation.system_prompt.as_deref(), config.append_system_prompt.as_deref(), Some(invocation.prompt.as_str())]
            .into_iter()
            .flatten()
            .map(str::to_owned)
            .collect();
    if let Some(schema) = &invocation.json_schema {
        parts.push(format!(
            "Respond with a single JSON object matching this JSON Schema, with no extra text:\n{schema}"
        ));
    }
    parts.join("\n\n")
}

/// Converts `invocation.mcp_config`'s `mcpServers` into Gemini CLI's workspace
/// settings format, forwarding configured environment variable names as `${VAR}`
/// placeholders (never writing secret values to disk).
fn gemini_mcp_servers(invocation: &Invocation, env: &[(String, String)]) -> (Map<String, Value>, Vec<String>) {
    let Some(configured) = invocation
        .mcp_config
        .as_deref()
        .and_then(|json| serde_json::from_str::<Value>(json).ok())
        .and_then(|mut json| json.get_mut("mcpServers").map(Value::take))
        .and_then(|value| match value {
            Value::Object(map) => Some(map),
            _ => None,
        })
    else {
        return (Map::new(), Vec::new());
    };

    let mut env_names: Vec<String> = env
        .iter()
        .map(|(name, _)| name.clone())
        .chain(std::iter::once("ELEVENLABS_API_KEY".to_owned()))
        .filter(|name| name != "PATH" && name != "GEMINI_CLI_TRUST_WORKSPACE" && name != "NO_BROWSER")
        .filter(|name| effective_env_is_set(name, env))
        .collect();
    env_names.sort();
    env_names.dedup();

    let mut forwarded = Map::new();
    for name in &env_names {
        forwarded.insert(name.clone(), Value::String(format!("${{{name}}}")));
    }

    let mut servers = Map::new();
    for (name, value) in configured {
        let Some(mut server) = value.as_object().cloned() else { continue };
        server.remove("type");
        server.insert("timeout".to_owned(), MCP_TOOL_TIMEOUT_MS.into());
        server.insert("trust".to_owned(), Value::Bool(true));
        if !forwarded.is_empty() {
            let mut server_env = server.remove("env").and_then(|v| v.as_object().cloned()).unwrap_or_default();
            for (k, v) in &forwarded {
                server_env.entry(k.clone()).or_insert_with(|| v.clone());
            }
            server.insert("env".to_owned(), Value::Object(server_env));
        }
        servers.insert(name, Value::Object(server));
    }
    (servers, env_names)
}

/// Whether `key` has a non-empty value in the child process's effective environment
/// (`env` overrides win over this process's environment).
fn effective_env_is_set(key: &str, env: &[(String, String)]) -> bool {
    if let Some((_, value)) = env.iter().rev().find(|(name, _)| name == key) {
        return !value.trim().is_empty();
    }
    std::env::var_os(key).is_some_and(|value| !value.to_string_lossy().trim().is_empty())
}

fn write_one_off_settings(dir: &Path) -> std::io::Result<()> {
    let gemini_dir = dir.join(".gemini");
    fs::create_dir_all(&gemini_dir)?;
    let settings = serde_json::json!({
        "tools": { "core": [] },
        "mcp": { "allowed": [NO_MCP_SERVERS] },
    });
    write_private_atomic(&gemini_dir.join("settings.json"), &serde_json::to_vec_pretty(&settings)?)
}

/// Reference-counted backup of `<cwd>/.gemini/settings.json` while one or more
/// Rushes-managed Gemini runs are active in `cwd`.
struct ActiveWorkspace {
    count: usize,
    created_dir: bool,
    original: Option<Vec<u8>>,
}

static ACTIVE_WORKSPACES: LazyLock<Mutex<HashMap<PathBuf, ActiveWorkspace>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Temporarily merges Rushes' MCP server into `<cwd>/.gemini/settings.json` and
/// restores the previous file (or removes `.gemini/`) when the last active run ends.
struct WorkspaceSettingsGuard {
    key: PathBuf,
    gemini_dir: PathBuf,
    settings_path: PathBuf,
}

impl WorkspaceSettingsGuard {
    fn install(cwd: &Path, servers: Map<String, Value>, allowed_env_vars: Vec<String>) -> std::io::Result<Self> {
        let key = fs::canonicalize(cwd).or_else(|_| std::path::absolute(cwd))?;
        let gemini_dir = cwd.join(".gemini");
        let settings_path = gemini_dir.join("settings.json");

        let mut map = ACTIVE_WORKSPACES.lock().unwrap_or_else(|poison| poison.into_inner());
        let (base_bytes, inserted_new) = if let Some(active) = map.get_mut(&key) {
            active.count += 1;
            (active.original.clone(), false)
        } else {
            let created_dir = !gemini_dir.exists();
            let original = match fs::read(&settings_path) {
                Ok(bytes) => Some(bytes),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error),
            };
            map.insert(key.clone(), ActiveWorkspace { count: 1, created_dir, original: original.clone() });
            (original, true)
        };

        let merged = merge_workspace_settings(base_bytes.as_deref(), servers, &allowed_env_vars);
        let write_result = fs::create_dir_all(&gemini_dir)
            .and_then(|()| serde_json::to_vec_pretty(&merged).map_err(std::io::Error::other))
            .and_then(|bytes| write_private_atomic(&settings_path, &bytes));

        if let Err(error) = write_result {
            if let Some(active) = map.get_mut(&key) {
                active.count -= 1;
                if active.count == 0 {
                    let active = map.remove(&key).expect("active entry");
                    if inserted_new && active.created_dir {
                        _ = fs::remove_dir(&gemini_dir);
                    }
                }
            }
            return Err(error);
        }

        Ok(Self { key, gemini_dir, settings_path })
    }
}

impl Drop for WorkspaceSettingsGuard {
    fn drop(&mut self) {
        let mut map = ACTIVE_WORKSPACES.lock().unwrap_or_else(|poison| poison.into_inner());
        let Some(active) = map.get_mut(&self.key) else { return };
        active.count = active.count.saturating_sub(1);
        if active.count > 0 {
            return;
        }
        let Some(active) = map.remove(&self.key) else { return };
        match active.original {
            Some(bytes) => _ = write_private_atomic(&self.settings_path, &bytes),
            None => {
                _ = fs::remove_file(&self.settings_path);
                if active.created_dir {
                    _ = fs::remove_dir(&self.gemini_dir);
                }
            }
        }
    }
}

fn merge_workspace_settings(
    original: Option<&[u8]>,
    servers: Map<String, Value>,
    allowed_env_vars: &[String],
) -> Value {
    let mut root = original
        .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok())
        .and_then(|value| match value {
            Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default();

    let mcp_servers = root.entry("mcpServers".to_owned()).or_insert_with(|| Value::Object(Map::new()));
    if !mcp_servers.is_object() {
        *mcp_servers = Value::Object(Map::new());
    }
    if let Some(existing) = mcp_servers.as_object_mut() {
        for (name, server) in servers {
            existing.insert(name, server);
        }
    }

    if !allowed_env_vars.is_empty() {
        let security = root.entry("security".to_owned()).or_insert_with(|| Value::Object(Map::new()));
        if !security.is_object() {
            *security = Value::Object(Map::new());
        }
        if let Some(security_map) = security.as_object_mut() {
            let redaction = security_map
                .entry("environmentVariableRedaction".to_owned())
                .or_insert_with(|| Value::Object(Map::new()));
            if !redaction.is_object() {
                *redaction = Value::Object(Map::new());
            }
            if let Some(redaction_map) = redaction.as_object_mut() {
                let allowed = redaction_map.entry("allowed".to_owned()).or_insert_with(|| Value::Array(Vec::new()));
                if !allowed.is_array() {
                    *allowed = Value::Array(Vec::new());
                }
                if let Some(list) = allowed.as_array_mut() {
                    for var in allowed_env_vars {
                        if !list.iter().any(|item| item.as_str() == Some(var.as_str())) {
                            list.push(Value::String(var.clone()));
                        }
                    }
                }
            }
        }
    }

    Value::Object(root)
}

fn write_private_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::Builder::new().prefix(".rushes-gemini-").suffix(".tmp").tempfile_in(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tmp.as_file().set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    tmp.write_all(bytes)?;
    tmp.as_file().flush()?;
    tmp.persist(path).map_err(|error| error.error)?;
    Ok(())
}
