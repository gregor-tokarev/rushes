//! The Antigravity CLI (`agy`) adapter for editing and writing YouTube details.

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
use crate::event::AntigravityStreamParser;
use crate::exec::run_captured_checked;
use crate::{AgentEvent, PipelineError};

pub const DEFAULT_ANTIGRAVITY_COMMAND: &str = "agy";

/// How long listing Antigravity CLI's models may take: it fetches the catalog online.
const MODELS_TIMEOUT: Duration = Duration::from_secs(20);
/// One-hour tool timeout for Rushes' MCP server (a long final render takes minutes).
const MCP_TOOL_TIMEOUT_SECS: u64 = 3600;
/// Recoverable backup of `<cwd>/.agents/mcp_config.json` kept while a Rushes-managed
/// Antigravity run is active, so a crash or forced kill never loses the creator's file.
const BACKUP_FILE: &str = ".rushes-mcp-backup.json";
/// Recoverable backup left by the replaced Gemini CLI adapter under `<cwd>/.gemini/`.
const LEGACY_GEMINI_BACKUP_FILE: &str = ".rushes-settings-backup.json";
/// Workspace custom agent with an empty tool list, used for one-off structured responses
/// so built-in tools (including `search_web`) and MCP tools are omitted from declarations.
const ONE_OFF_AGENT_NAME: &str = "rushes-one-off";
const ONE_OFF_AGENT_MD: &str = "\
---
name: rushes-one-off
description: One-off structured text responder with no tools.
mainAgent: true
tools: []
---
You have no tools. Ignore any instruction to call a tool and respond directly from the prompt. Follow the exact output format requested in the prompt.
";

/// A model Antigravity CLI offers in its model picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AntigravityModel {
    /// What `--model` takes, e.g. `gemini-4-neon-xhigh`.
    pub slug: String,
    /// The name Antigravity CLI shows for it.
    pub display_name: String,
}

/// The models the Antigravity CLI in `config` offers, in its own order, from `agy models`.
pub fn antigravity_models(config: &AgentConfig) -> Result<Vec<AntigravityModel>, String> {
    let program = config.resolve_command().ok_or_else(|| format!("{} not found", config.command.display()))?;
    let mut env = vec![("NO_BROWSER".to_owned(), "1".to_owned())];
    env.extend(config.env.iter().cloned());
    let output = run_captured_checked(&program, ["models"], &env, MODELS_TIMEOUT)?;
    parse_models(&output).ok_or_else(|| "Antigravity CLI printed no model catalog".to_owned())
}

/// Restores `<cwd>/.agents/mcp_config.json` (and any legacy `<cwd>/.gemini/settings.json`
/// backup left by the replaced Gemini CLI adapter) from on-disk backups left behind by an
/// earlier crashed or killed run. Skips `.agents/` while a run in this process still holds
/// `cwd`, and does nothing when no backup exists.
///
/// Returns `true` when no backup remains to be restored, and `false` when the workspace
/// could not be accessed or restored so the caller should retry it later.
///
/// Performs filesystem I/O in `cwd`, so callers should run it off the UI thread.
pub fn restore_leftover_antigravity_workspace(cwd: &Path) -> bool {
    let Ok(key) = fs::canonicalize(cwd) else {
        return false;
    };
    let slot = workspace_slot(&key);
    let active = slot.lock().unwrap_or_else(|poison| poison.into_inner());
    let antigravity_cleared = if active.is_some() {
        true
    } else {
        restore_leftover_backup(&cwd.join(".agents"), "mcp_config.json", BACKUP_FILE)
    };
    let legacy_gemini_cleared =
        restore_leftover_backup(&cwd.join(".gemini"), "settings.json", LEGACY_GEMINI_BACKUP_FILE);
    antigravity_cleared && legacy_gemini_cleared
}

fn restore_leftover_backup(dir: &Path, config_file: &str, backup_file: &str) -> bool {
    let backup_path = dir.join(backup_file);
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
    restore_workspace_files(dir, &dir.join(config_file), &backup_path, &backup)
}

fn parse_models(output: &str) -> Option<Vec<AntigravityModel>> {
    let lines: Vec<&str> = output.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    let has_tabs = lines.iter().any(|line| line.contains('\t'));
    let mut seen = HashSet::new();
    let mut models = Vec::new();
    for line in lines {
        let (slug, display_name) = if has_tabs {
            let Some((slug, name)) = line.split_once('\t') else { continue };
            (slug.trim(), name.trim())
        } else {
            if line.ends_with("...") || line.starts_with("Fetching ") || line.starts_with("Available ") {
                continue;
            }
            match line.split_once(char::is_whitespace) {
                Some((slug, name)) => (slug.trim(), name.trim()),
                None => (line, line),
            }
        };
        if slug.is_empty() || slug.chars().any(char::is_whitespace) || !seen.insert(slug.to_owned()) {
            continue;
        }
        let display_name = if display_name.is_empty() { slug } else { display_name };
        models.push(AntigravityModel { slug: slug.to_owned(), display_name: display_name.to_owned() });
    }
    (!models.is_empty()).then_some(models)
}

pub(crate) fn run(
    config: &AgentConfig,
    program: &Path,
    invocation: &Invocation,
    on_event: &mut dyn FnMut(&AgentEvent),
    cancel: &AtomicBool,
) -> Result<(ExitStatus, AgentRun), PipelineError> {
    // Never pop open a browser window from a headless run; explicit caller overrides
    // in `config.env` come last and still win.
    let mut env: Vec<(String, String)> = vec![("NO_BROWSER".to_owned(), "1".to_owned())];
    env.extend(invocation.env.iter().cloned());
    env.extend(config.env.iter().cloned());

    // Stream the prompt over stdin as one NDJSON user message so long scripts and
    // transcripts never hit OS command-line length limits.
    let mut args: Vec<OsString> = vec![
        "--input-format=stream-json".into(),
        "--output-format=stream-json".into(),
        "--disable-slash-commands".into(),
    ];
    if let Some(model) = &config.model {
        args.push(format!("--model={model}").into());
    }
    if let Some(session_id) = &invocation.resume {
        args.push(format!("--conversation={session_id}").into());
    }
    // Antigravity CLI's `--json-schema` retry loop hangs or drops structured output when
    // paired with a custom `tools: []` agent, so one-off schema requests embed the schema
    // in the prompt instead (matching how the previous Gemini CLI adapter worked).
    let pass_json_schema_flag = !invocation.one_off;
    if pass_json_schema_flag && let Some(schema) = &invocation.json_schema {
        args.push(format!("--json-schema={schema}").into());
    }

    let _one_off_dir;
    let _workspace_guard;
    let cwd: PathBuf;

    if invocation.one_off {
        // One-off requests (YouTube details) need no tools and nothing from the footage
        // folder. Running in a private temporary folder with a no-tools custom agent
        // strips every built-in and MCP tool declaration (including `search_web`), while
        // strict deny rules remain in place as defense in depth.
        let dir = tempfile::tempdir()?;
        write_one_off_agent(dir.path())?;
        args.push(format!("--agent={ONE_OFF_AGENT_NAME}").into());
        let settings = serde_json::json!({
            "toolPermission": "strict",
            "allowNonWorkspaceAccess": false,
            "notifications": false,
            "showFeedbackSurvey": false,
            "permissions": {
                "deny": [
                    "command(*)",
                    "read_file(*)",
                    "write_file(*)",
                    "read_url(*)",
                    "execute_url(*)",
                    "mcp(*)",
                ]
            }
        });
        args.push(format!("--settings={settings}").into());
        cwd = dir.path().to_path_buf();
        _one_off_dir = Some(dir);
        _workspace_guard = None;
    } else {
        for dir in &invocation.add_dirs {
            let mut arg = OsString::from("--add-dir=");
            arg.push(dir);
            args.push(arg);
        }
        let servers = antigravity_mcp_servers(invocation);
        let deny = if servers.is_empty() {
            vec!["mcp(*)".to_owned()]
        } else {
            let home =
                config.env_var("HOME").filter(|home| !home.is_empty()).map(PathBuf::from).or_else(std::env::home_dir);
            unlisted_mcp_deny_rules(&invocation.cwd, home.as_deref(), &servers)
        };
        let mut settings = serde_json::json!({
            "agentMode": "accept-edits",
            "toolPermission": "always-proceed",
            "artifactReviewPolicy": "always-proceed",
            "enableTerminalSandbox": false,
            "notifications": false,
            "showFeedbackSurvey": false,
        });
        if !deny.is_empty() {
            settings["permissions"] = serde_json::json!({ "deny": deny });
        }
        args.push(format!("--settings={settings}").into());
        _workspace_guard =
            if servers.is_empty() { None } else { Some(WorkspaceMcpGuard::install(&invocation.cwd, servers)?) };
        cwd = invocation.cwd.clone();
        _one_off_dir = None;
    }

    args.extend(config.extra_args.iter().map(OsString::from));

    let prompt = build_prompt(config, invocation, !pass_json_schema_flag);
    let stdin_line = format!("{}\n", serde_json::json!({ "event": "user", "message": { "content": prompt } }));
    let mut parser = AntigravityStreamParser::new();
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
            stdin: Some(stdin_line),
            kill_grace: config.kill_grace,
            parse: &mut parse,
        },
        on_event,
        cancel,
    )
}

fn write_one_off_agent(dir: &Path) -> std::io::Result<()> {
    let agent_dir = dir.join(".agents").join("agents").join(ONE_OFF_AGENT_NAME);
    fs::create_dir_all(&agent_dir)?;
    fs::write(agent_dir.join("agent.md"), ONE_OFF_AGENT_MD)
}

fn build_prompt(config: &AgentConfig, invocation: &Invocation, embed_json_schema: bool) -> String {
    let mut parts: Vec<String> =
        [invocation.system_prompt.as_deref(), config.append_system_prompt.as_deref(), Some(invocation.prompt.as_str())]
            .into_iter()
            .flatten()
            .map(str::to_owned)
            .collect();
    if embed_json_schema && let Some(schema) = &invocation.json_schema {
        parts.push(format!(
            "Respond with a single JSON object matching this JSON Schema, with no extra text:\n{schema}"
        ));
    }
    parts.join("\n\n")
}

/// Converts `invocation.mcp_config`'s `mcpServers` into Antigravity CLI's workspace
/// `mcp_config.json` format. Stdio MCP servers inherit the agent's environment directly,
/// so secret values are never written to disk.
fn antigravity_mcp_servers(invocation: &Invocation) -> Map<String, Value> {
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

    let mut servers = Map::new();
    for (name, value) in configured {
        let Some(mut server) = value.as_object().cloned() else { continue };
        server.remove("type");
        server.insert("timeoutSeconds".to_owned(), MCP_TOOL_TIMEOUT_SECS.into());
        if name == rushes_edit::SERVER_NAME {
            let mut tools = Map::new();
            for tool in rushes_edit::TOOL_NAMES.iter().copied().chain(["add_footage"]) {
                tools.insert(tool.to_owned(), serde_json::json!({ "eager": true }));
            }
            server.insert("tools".to_owned(), Value::Object(tools));
        }
        servers.insert(name, Value::Object(server));
    }
    servers
}

/// Builds `mcp(<server>/*)` deny rules for user-configured global, plugin and workspace
/// MCP servers outside `allowed`, reading only server names and never secret values.
fn unlisted_mcp_deny_rules(cwd: &Path, home: Option<&Path>, allowed: &Map<String, Value>) -> Vec<String> {
    let mut config_paths = vec![cwd.join(".agents").join("mcp_config.json")];
    collect_plugin_mcp_configs(&cwd.join(".agents").join("plugins"), &mut config_paths);
    if let Some(home) = home {
        let gemini_config = home.join(".gemini").join("config");
        config_paths.push(gemini_config.join("mcp_config.json"));
        collect_plugin_mcp_configs(&gemini_config.join("plugins"), &mut config_paths);
    }

    let mut names = Vec::new();
    for path in config_paths {
        let Ok(bytes) = fs::read(&path) else { continue };
        let Ok(Value::Object(root)) = serde_json::from_slice::<Value>(&bytes) else { continue };
        let Some(servers) = root.get("mcpServers").and_then(Value::as_object) else { continue };
        for name in servers.keys() {
            if !allowed.contains_key(name)
                && !name.is_empty()
                && !name.chars().any(|c| c.is_whitespace() || matches!(c, '(' | ')' | '*' | '/'))
            {
                names.push(format!("mcp({name}/*)"));
            }
        }
    }
    names.sort();
    names.dedup();
    names
}

fn collect_plugin_mcp_configs(plugins_dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(plugins_dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path().join("mcp_config.json");
        if path.is_file() {
            out.push(path);
        }
    }
}

/// On-disk backup written before replacing `<cwd>/.agents/mcp_config.json`.
#[derive(Clone, Serialize, Deserialize)]
struct WorkspaceBackup {
    created_dir: bool,
    /// Exact original bytes of `<cwd>/.agents/mcp_config.json` (`None` if absent).
    original: Option<Vec<u8>>,
}

/// Reference-counted backup of `<cwd>/.agents/mcp_config.json` while one or more
/// Rushes-managed Antigravity runs are active in `cwd`.
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

/// Temporarily merges Rushes' MCP server into `<cwd>/.agents/mcp_config.json`, keeping
/// a recoverable on-disk backup until the last active run restores the original file
/// (or removes `.agents/`).
struct WorkspaceMcpGuard {
    slot: WorkspaceSlot,
    agents_dir: PathBuf,
    config_path: PathBuf,
    backup_path: PathBuf,
}

impl WorkspaceMcpGuard {
    fn install(cwd: &Path, servers: Map<String, Value>) -> std::io::Result<Self> {
        let key = fs::canonicalize(cwd).or_else(|_| std::path::absolute(cwd))?;
        let agents_dir = cwd.join(".agents");
        let config_path = agents_dir.join("mcp_config.json");
        let backup_path = agents_dir.join(BACKUP_FILE);

        let slot = workspace_slot(&key);
        {
            let mut active_slot = slot.lock().unwrap_or_else(|poison| poison.into_inner());
            let (base_bytes, inserted_new, created_backup_now) = if let Some(active) = active_slot.as_mut() {
                active.count += 1;
                // Layer onto the currently active config file so concurrent runs do not
                // drop each other's additions; `active.backup` still holds the untouched original.
                let current = fs::read(&config_path).ok().or_else(|| active.backup.original.clone());
                (current, false, false)
            } else {
                let (backup, created_backup_now) = match fs::read(&backup_path) {
                    Ok(bytes) => {
                        // A previous Rushes process died before Drop ran: reuse its backup
                        // instead of treating the temporary replacement as the original.
                        let backup: WorkspaceBackup = serde_json::from_slice(&bytes).map_err(|error| {
                            std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                format!("unreadable Antigravity MCP backup {}: {error}", backup_path.display()),
                            )
                        })?;
                        (backup, false)
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        let created_dir = !agents_dir.exists();
                        let original = match fs::read(&config_path) {
                            Ok(bytes) => Some(bytes),
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                            Err(error) => return Err(error),
                        };
                        let backup = WorkspaceBackup { created_dir, original };
                        fs::create_dir_all(&agents_dir)?;
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

            let merged = merge_workspace_mcp_config(base_bytes.as_deref(), servers);
            let write_result = fs::create_dir_all(&agents_dir)
                .and_then(|()| serde_json::to_vec_pretty(&merged).map_err(std::io::Error::other))
                .and_then(|bytes| write_private_atomic(&config_path, &bytes));

            if let Err(error) = write_result {
                if let Some(active) = active_slot.as_mut() {
                    active.count -= 1;
                    if active.count == 0 {
                        let active = active_slot.take().expect("active entry");
                        if inserted_new && created_backup_now {
                            _ = fs::remove_file(&backup_path);
                            if active.backup.created_dir {
                                _ = fs::remove_dir(&agents_dir);
                            }
                        }
                    }
                }
                return Err(error);
            }
        }

        Ok(Self { slot, agents_dir, config_path, backup_path })
    }
}

impl Drop for WorkspaceMcpGuard {
    fn drop(&mut self) {
        let mut active_slot = self.slot.lock().unwrap_or_else(|poison| poison.into_inner());
        let Some(active) = active_slot.as_mut() else { return };
        active.count = active.count.saturating_sub(1);
        if active.count > 0 {
            return;
        }
        let Some(active) = active_slot.take() else { return };
        _ = restore_workspace_files(&self.agents_dir, &self.config_path, &self.backup_path, &active.backup);
    }
}

fn restore_workspace_files(
    agents_dir: &Path,
    config_path: &Path,
    backup_path: &Path,
    backup: &WorkspaceBackup,
) -> bool {
    let restored = match &backup.original {
        Some(bytes) => write_private_atomic(config_path, bytes).is_ok(),
        None => match fs::remove_file(config_path) {
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
        _ = fs::remove_dir(agents_dir);
    }
    backup_removed
}

fn merge_workspace_mcp_config(original: Option<&[u8]>, servers: Map<String, Value>) -> Value {
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
        for (name, value) in existing.iter_mut() {
            if !servers.contains_key(name)
                && let Some(entry) = value.as_object_mut()
            {
                entry.insert("disabled".to_owned(), Value::Bool(true));
            }
        }
        for (name, server) in servers {
            existing.insert(name, server);
        }
    }

    Value::Object(root)
}

fn write_private_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::Builder::new().prefix(".rushes-antigravity-").suffix(".tmp").tempfile_in(dir)?;
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
