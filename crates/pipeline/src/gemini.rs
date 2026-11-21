//! The Gemini CLI adapter for editing and writing YouTube details.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::time::Duration;

use serde::{Deserialize, Serialize};
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
/// Recoverable backup of `<cwd>/.gemini/settings.json` kept while a Rushes-managed
/// Gemini run is active, so a crash or forced kill never loses the creator's file.
const BACKUP_FILE: &str = ".rushes-settings-backup.json";
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

/// Restores `<cwd>/.gemini/settings.json` from an on-disk backup left behind by an
/// earlier crashed or killed Gemini CLI run. Does nothing while a run in this process
/// still holds `cwd`, or when no backup exists.
///
/// Returns `true` when no backup remains to be restored (or an active run in this
/// process now owns it), and `false` when the workspace could not be accessed or
/// restored so the caller should retry it later.
///
/// Performs filesystem I/O in `cwd`, so callers should run it off the UI thread.
pub fn restore_leftover_gemini_workspace(cwd: &Path) -> bool {
    let Ok(key) = fs::canonicalize(cwd) else {
        return false;
    };
    let slot = workspace_slot(&key);
    let active = slot.lock().unwrap_or_else(|poison| poison.into_inner());
    if active.is_some() {
        return true;
    }
    let gemini_dir = cwd.join(".gemini");
    let backup_path = gemini_dir.join(BACKUP_FILE);
    let bytes = match fs::read(&backup_path) {
        Ok(bytes) => bytes,
        Err(error) if matches!(error.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory) => {
            return true;
        }
        Err(_) => return false,
    };
    let Ok(backup) = serde_json::from_slice::<WorkspaceBackup>(&bytes) else {
        return true;
    };
    restore_workspace_files(&gemini_dir, &gemini_dir.join("settings.json"), &backup_path, &backup)
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
        // folder. Running in a private temporary folder disables built-in, discovered,
        // MCP, skill and hook execution without touching the user's directory or history.
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
        let allowed_env_vars = forwarded_env_vars(&env);
        let servers = gemini_mcp_servers(invocation, &allowed_env_vars);
        if servers.is_empty() {
            args.push(format!("--allowed-mcp-server-names={NO_MCP_SERVERS}").into());
        } else {
            let allowed_names: Vec<String> = servers.keys().cloned().collect();
            args.push(format!("--allowed-mcp-server-names={}", allowed_names.join(",")).into());
        }
        // Install temporary workspace settings whenever we need to register Rushes' MCP
        // server or allowlist forwarded environment variables (such as ELEVENLABS_API_KEY
        // for video-use shell transcription when Gemini's env redaction is enabled).
        _workspace_guard = if servers.is_empty() && allowed_env_vars.is_empty() {
            None
        } else {
            Some(WorkspaceSettingsGuard::install(&invocation.cwd, servers, allowed_env_vars)?)
        };
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

/// Environment variable names that must survive Gemini CLI's environment redaction
/// for both MCP servers and video-use shell commands (e.g. `ELEVENLABS_API_KEY`).
fn forwarded_env_vars(env: &[(String, String)]) -> Vec<String> {
    let mut names: Vec<String> = env
        .iter()
        .map(|(name, _)| name.clone())
        .chain(std::iter::once("ELEVENLABS_API_KEY".to_owned()))
        .filter(|name| name != "PATH" && name != "GEMINI_CLI_TRUST_WORKSPACE" && name != "NO_BROWSER")
        .filter(|name| effective_env_is_set(name, env))
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Converts `invocation.mcp_config`'s `mcpServers` into Gemini CLI's workspace
/// settings format, forwarding `env_names` as `${VAR}` placeholders (never writing
/// secret values to disk).
fn gemini_mcp_servers(invocation: &Invocation, env_names: &[String]) -> Map<String, Value> {
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
        return Map::new();
    };

    let mut forwarded = Map::new();
    for name in env_names {
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
    servers
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
    // Explicitly override scalar/array tool, discovery, MCP, skill and hook settings
    // from user global config so no tool can run during a YouTube-details request.
    let settings = serde_json::json!({
        "tools": {
            "core": [],
            "allowed": [],
            "discoveryCommand": "",
            "callCommand": "",
        },
        "mcp": {
            "allowed": [NO_MCP_SERVERS],
            "serverCommand": "",
        },
        "skills": { "enabled": false },
        "hooksConfig": { "enabled": false },
    });
    write_private_atomic(&gemini_dir.join("settings.json"), &serde_json::to_vec_pretty(&settings)?)
}

/// On-disk backup written before replacing `<cwd>/.gemini/settings.json`.
#[derive(Clone, Serialize, Deserialize)]
struct WorkspaceBackup {
    created_dir: bool,
    /// Exact original bytes of `<cwd>/.gemini/settings.json` (`None` if absent).
    original: Option<Vec<u8>>,
}

/// Reference-counted backup of `<cwd>/.gemini/settings.json` while one or more
/// Rushes-managed Gemini runs are active in `cwd`.
struct ActiveWorkspace {
    count: usize,
    backup: WorkspaceBackup,
}

type WorkspaceState = Mutex<Option<ActiveWorkspace>>;
type WorkspaceSlot = Arc<WorkspaceState>;
type WorkspaceMap = HashMap<PathBuf, Weak<WorkspaceState>>;

static ACTIVE_WORKSPACES: LazyLock<Mutex<WorkspaceMap>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Returns the per-workspace lock for `key` without holding the global map lock
/// across filesystem I/O, so a stalled mount in one folder never blocks another.
fn workspace_slot(key: &Path) -> WorkspaceSlot {
    let mut map = ACTIVE_WORKSPACES.lock().unwrap_or_else(|poison| poison.into_inner());
    map.retain(|_, slot| slot.strong_count() > 0);
    if let Some(slot) = map.get(key).and_then(Weak::upgrade) {
        return slot;
    }
    let slot = Arc::new(Mutex::new(None));
    map.insert(key.to_path_buf(), Arc::downgrade(&slot));
    slot
}

/// Temporarily merges Rushes' MCP server and env allowlist into
/// `<cwd>/.gemini/settings.json`, keeping a recoverable on-disk backup until the
/// last active run restores the original file (or removes `.gemini/`).
struct WorkspaceSettingsGuard {
    slot: WorkspaceSlot,
    gemini_dir: PathBuf,
    settings_path: PathBuf,
    backup_path: PathBuf,
}

impl WorkspaceSettingsGuard {
    fn install(cwd: &Path, servers: Map<String, Value>, allowed_env_vars: Vec<String>) -> std::io::Result<Self> {
        let key = fs::canonicalize(cwd).or_else(|_| std::path::absolute(cwd))?;
        let gemini_dir = cwd.join(".gemini");
        let settings_path = gemini_dir.join("settings.json");
        let backup_path = gemini_dir.join(BACKUP_FILE);

        let slot = workspace_slot(&key);
        {
            let mut active_slot = slot.lock().unwrap_or_else(|poison| poison.into_inner());
            let (base_bytes, inserted_new, created_backup_now) = if let Some(active) = active_slot.as_mut() {
                active.count += 1;
                // Layer onto the currently active settings file so concurrent runs do not
                // drop each other's additions; `active.backup` still holds the untouched original.
                let current = fs::read(&settings_path).ok().or_else(|| active.backup.original.clone());
                (current, false, false)
            } else {
                let (backup, created_backup_now) = match fs::read(&backup_path) {
                    Ok(bytes) => {
                        // A previous Rushes process died before Drop ran: reuse its backup
                        // instead of treating the temporary replacement as the original.
                        let backup: WorkspaceBackup = serde_json::from_slice(&bytes).map_err(|error| {
                            std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                format!("unreadable Gemini settings backup {}: {error}", backup_path.display()),
                            )
                        })?;
                        (backup, false)
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        let created_dir = !gemini_dir.exists();
                        let original = match fs::read(&settings_path) {
                            Ok(bytes) => Some(bytes),
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                            Err(error) => return Err(error),
                        };
                        let backup = WorkspaceBackup { created_dir, original };
                        fs::create_dir_all(&gemini_dir)?;
                        let encoded = serde_json::to_vec(&backup).map_err(std::io::Error::other)?;
                        write_private_atomic(&backup_path, &encoded)?;
                        (backup, true)
                    }
                    Err(error) => return Err(error),
                };
                let base = backup.original.clone();
                *active_slot = Some(ActiveWorkspace { count: 1, backup });
                (base, true, created_backup_now)
            };

            let merged = merge_workspace_settings(base_bytes.as_deref(), servers, &allowed_env_vars);
            let write_result = fs::create_dir_all(&gemini_dir)
                .and_then(|()| serde_json::to_vec_pretty(&merged).map_err(std::io::Error::other))
                .and_then(|bytes| write_private_atomic(&settings_path, &bytes));

            if let Err(error) = write_result {
                if let Some(active) = active_slot.as_mut() {
                    active.count -= 1;
                    if active.count == 0 {
                        let active = active_slot.take().expect("active entry");
                        if inserted_new && created_backup_now {
                            _ = fs::remove_file(&backup_path);
                            if active.backup.created_dir {
                                _ = fs::remove_dir(&gemini_dir);
                            }
                        }
                    }
                }
                return Err(error);
            }
        }

        Ok(Self { slot, gemini_dir, settings_path, backup_path })
    }
}

impl Drop for WorkspaceSettingsGuard {
    fn drop(&mut self) {
        let mut active_slot = self.slot.lock().unwrap_or_else(|poison| poison.into_inner());
        let Some(active) = active_slot.as_mut() else { return };
        active.count = active.count.saturating_sub(1);
        if active.count > 0 {
            return;
        }
        let Some(active) = active_slot.take() else { return };
        _ = restore_workspace_files(&self.gemini_dir, &self.settings_path, &self.backup_path, &active.backup);
    }
}

fn restore_workspace_files(
    gemini_dir: &Path,
    settings_path: &Path,
    backup_path: &Path,
    backup: &WorkspaceBackup,
) -> bool {
    let restored = match &backup.original {
        Some(bytes) => write_private_atomic(settings_path, bytes).is_ok(),
        None => match fs::remove_file(settings_path) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
            Err(_) => false,
        },
    };
    if !restored {
        return false;
    }
    let backup_removed = match fs::remove_file(backup_path) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => false,
    };
    if backup_removed && backup.created_dir {
        _ = fs::remove_dir(gemini_dir);
    }
    backup_removed
}

fn merge_workspace_settings(
    original: Option<&[u8]>,
    servers: Map<String, Value>,
    allowed_env_vars: &[String],
) -> Value {
    let mut root = original.and_then(parse_jsonc_object).unwrap_or_default();

    if !servers.is_empty() {
        let mcp_servers = root.entry("mcpServers".to_owned()).or_insert_with(|| Value::Object(Map::new()));
        if !mcp_servers.is_object() {
            *mcp_servers = Value::Object(Map::new());
        }
        if let Some(existing) = mcp_servers.as_object_mut() {
            for (name, server) in servers {
                existing.insert(name, server);
            }
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

/// Parses a Gemini CLI settings file (JSON with `//` and `/* ... */` comments and
/// optional trailing commas) into a JSON object map.
fn parse_jsonc_object(bytes: &[u8]) -> Option<Map<String, Value>> {
    let text = std::str::from_utf8(bytes).ok()?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let uncommented = strip_json_comments(text);
    let cleaned = strip_trailing_commas(&uncommented);
    match serde_json::from_str::<Value>(&cleaned).ok()? {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

/// Removes `//` and `/* ... */` comments outside JSON string literals.
fn strip_json_comments(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    let mut in_string = false;
    while i < bytes.len() {
        let ch = input[i..].chars().next().expect("valid utf-8 boundary");
        if in_string {
            out.push(ch);
            i += ch.len_utf8();
            if ch == '\\' && i < bytes.len() {
                let escaped = input[i..].chars().next().expect("valid utf-8 boundary");
                out.push(escaped);
                i += escaped.len_utf8();
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if ch == '"' {
            in_string = true;
            out.push(ch);
            i += 1;
            continue;
        }
        if bytes[i] == b'/' && i + 1 < bytes.len() {
            if bytes[i + 1] == b'/' {
                i += 2;
                while i < bytes.len() && bytes[i] != b'\n' && bytes[i] != b'\r' {
                    i += 1;
                }
                continue;
            }
            if bytes[i + 1] == b'*' {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
                continue;
            }
        }
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Removes trailing commas before `}` or `]` outside JSON string literals.
fn strip_trailing_commas(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    let mut in_string = false;
    while i < bytes.len() {
        let ch = input[i..].chars().next().expect("valid utf-8 boundary");
        if in_string {
            out.push(ch);
            i += ch.len_utf8();
            if ch == '\\' && i < bytes.len() {
                let escaped = input[i..].chars().next().expect("valid utf-8 boundary");
                out.push(escaped);
                i += escaped.len_utf8();
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if ch == '"' {
            in_string = true;
            out.push(ch);
            i += 1;
            continue;
        }
        if ch == ',' {
            let mut lookahead = i + 1;
            while lookahead < bytes.len() && bytes[lookahead].is_ascii_whitespace() {
                lookahead += 1;
            }
            if lookahead < bytes.len() && matches!(bytes[lookahead], b'}' | b']') {
                i += 1;
                continue;
            }
        }
        out.push(ch);
        i += ch.len_utf8();
    }
    out
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
