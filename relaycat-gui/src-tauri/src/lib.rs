//! RelayCat desktop GUI backend (Tauri).
//!
//! The heavy lifting lives in the `relaycat-cli` library: session/command
//! definitions, config, pairing material, and the frontend-agnostic
//! `PtySession`. This crate is a thin bridge that exposes those as Tauri
//! commands and streams PTY output to the webview.

mod manager;
mod relay;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use manager::{RelayContext, RelayEvent, RelayStateBridge, SessionManager};
use relaycat_cli::command::{SessionKind, TargetCommand};
use relaycat_cli::config::{self, Config, CustomTool};
use relaycat_cli::pairing_store;
use relaycat_cli::recent_store::{self, RecentStore};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

const BUILTIN_TOOLS: &[(&str, &str)] = &[
    ("shell", "Shell"),
    ("codex", "Codex"),
    ("claude", "Claude Code"),
    ("opencode", "OpenCode"),
    ("gemini", "Gemini"),
];

static SESSION_SEQ: AtomicU64 = AtomicU64::new(1);

/// Environment variable carrying the GUI's process id to the Windows relay
/// child, so the child's watchdog can self-terminate (taking its inner shell
/// with it) the moment the GUI exits. See `main.rs`.
pub const GUI_PARENT_PID_ENV: &str = "RELAYCAT_GUI_PARENT_PID";

#[derive(Serialize)]
struct ToolDto {
    name: String,
    label: String,
    /// `builtin` | `custom`
    kind: String,
}

#[derive(Serialize, Deserialize)]
struct CustomToolDto {
    name: String,
    #[serde(default)]
    label: Option<String>,
    cmd: String,
    #[serde(default)]
    args: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct ConfigDto {
    #[serde(default)]
    default_relay: Option<String>,
    #[serde(default)]
    default_tool: Option<String>,
    #[serde(default)]
    favorites: Vec<String>,
    #[serde(default)]
    tools: Vec<CustomToolDto>,
    #[serde(default = "default_true")]
    return_to_launcher: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Serialize)]
struct RecentDto {
    id: String,
    kind: String,
    project: String,
    relay: String,
}

#[derive(Deserialize)]
struct CreateOpts {
    /// Frontend-generated session id (the tab already exists under this id, so
    /// no startup output is lost to a race).
    id: String,
    /// Builtin kind (shell/codex/...) or a custom tool name.
    tool: String,
    project: Option<String>,
    relay: Option<String>,
    rows: u16,
    cols: u16,
}

#[derive(Serialize)]
struct SessionInfo {
    id: String,
    title: String,
    /// `local` | `relay`
    mode: String,
    /// For relay sessions, the CLI log path shown in the diagnostics panel.
    log_path: Option<String>,
}

#[derive(Serialize)]
struct DiagnosticsInfo {
    relay_engine: String,
    gui_exe: String,
    cli_version: String,
    config_path: String,
    recent_path: String,
}

fn to_dto(config: &Config) -> ConfigDto {
    ConfigDto {
        default_relay: config.default_relay.clone(),
        default_tool: config.default_tool.clone(),
        favorites: config
            .favorites
            .iter()
            .map(|p| p.display().to_string())
            .collect(),
        tools: config
            .tools
            .iter()
            .map(|t| CustomToolDto {
                name: t.name.clone(),
                label: t.label.clone(),
                cmd: t.cmd.clone(),
                args: t.args.clone(),
            })
            .collect(),
        return_to_launcher: config.return_to_launcher,
    }
}

fn from_dto(dto: ConfigDto) -> Config {
    Config {
        default_relay: dto.default_relay,
        default_tool: dto.default_tool,
        favorites: dto.favorites.into_iter().map(PathBuf::from).collect(),
        tools: dto
            .tools
            .into_iter()
            .map(|t| CustomTool {
                name: t.name,
                label: t.label,
                cmd: t.cmd,
                args: t.args,
            })
            .collect(),
        return_to_launcher: dto.return_to_launcher,
    }
}

fn load_config() -> Result<Config, String> {
    let path = config::config_file_path().map_err(|e| e.to_string())?;
    Config::load(&path).map_err(|e| e.to_string())
}

fn is_builtin(kind: &str) -> bool {
    BUILTIN_TOOLS.iter().any(|(name, _)| *name == kind)
}

fn build_target(opts: &CreateOpts, config: &Config) -> Result<TargetCommand, String> {
    let project = opts.project.clone().map(PathBuf::from);
    let relay = opts
        .relay
        .as_ref()
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty());

    if is_builtin(&opts.tool) {
        return TargetCommand::for_kind(&opts.tool, project, relay).map_err(|e| e.to_string());
    }

    let tool = config
        .tools
        .iter()
        .find(|t| t.name == opts.tool)
        .ok_or_else(|| format!("unknown tool `{}`", opts.tool))?;
    TargetCommand::for_custom(
        &tool.name,
        tool.cmd.clone(),
        tool.args.clone(),
        project,
        relay,
    )
    .map_err(|e| e.to_string())
}

/// Resolve `program` to the concrete file PATH lookup would launch. A bare
/// command name is looked up across PATH entries (honouring PATHEXT on
/// Windows); a path with a separator is checked directly.
fn resolve_on_path(program: &str) -> Option<PathBuf> {
    let program = program.trim();
    if program.is_empty() {
        return None;
    }
    let direct = Path::new(program);
    if direct.is_absolute() || program.contains('/') || program.contains('\\') {
        return direct.is_file().then(|| direct.to_path_buf());
    }
    let paths = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".to_string())
            .split(';')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&paths) {
        for ext in &exts {
            let candidate = dir.join(format!("{program}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Whether `program` resolves to an executable on the current PATH.
fn is_on_path(program: &str) -> bool {
    resolve_on_path(program).is_some()
}

#[derive(Serialize)]
struct ToolStatusDto {
    name: String,
    label: String,
    /// `builtin` | `custom`
    kind: String,
    /// The underlying program/binary RelayCat would launch.
    program: String,
    /// The program resolves to an executable on PATH right now.
    installed: bool,
    /// Best-effort version string from `<program> --version`, if available.
    version: Option<String>,
}

/// Persisted detection result for a single tool. Stored in `tool_status.json`
/// next to the launcher config so the new-session picker can decide tool
/// availability without re-probing the filesystem on every open.
#[derive(Serialize, Deserialize, Clone)]
struct ToolDetectionEntry {
    name: String,
    installed: bool,
    #[serde(default)]
    version: Option<String>,
}

/// On-disk cache of the last tool-detection sweep.
#[derive(Serialize, Deserialize, Default)]
struct ToolStatusCache {
    /// Unix seconds when the sweep ran (best-effort, may be absent).
    #[serde(default)]
    detected_at: Option<i64>,
    tools: Vec<ToolDetectionEntry>,
}

/// `<config dir>/tool_status.json` — sibling of the launcher `config.json`.
fn tool_status_file_path() -> Result<PathBuf, String> {
    let cfg = config::config_file_path().map_err(|e| e.to_string())?;
    Ok(cfg.with_file_name("tool_status.json"))
}

fn load_tool_status() -> ToolStatusCache {
    let Ok(path) = tool_status_file_path() else {
        return ToolStatusCache::default();
    };
    let Ok(data) = std::fs::read_to_string(&path) else {
        return ToolStatusCache::default();
    };
    serde_json::from_str(&data).unwrap_or_default()
}

fn save_tool_status(cache: &ToolStatusCache) -> Result<(), String> {
    let path = tool_status_file_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(cache).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())
}

fn now_unix() -> Option<i64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)
}

/// Enumerate every configured tool (builtins + custom) with `installed`/`version`
/// left unset. Shared by live detection and the cached-status reader so both
/// agree on the tool set, labels, and launch programs.
fn enumerate_tool_targets() -> Result<Vec<ToolStatusDto>, String> {
    let config = load_config()?;
    let mut out: Vec<ToolStatusDto> = Vec::new();
    for (name, label) in BUILTIN_TOOLS {
        let program = TargetCommand::for_kind(name, None, None)
            .map(|c| c.program)
            .unwrap_or_else(|_| (*name).to_string());
        out.push(ToolStatusDto {
            name: name.to_string(),
            label: label.to_string(),
            kind: "builtin".to_string(),
            program,
            installed: false,
            version: None,
        });
    }
    for tool in &config.tools {
        out.push(ToolStatusDto {
            name: tool.name.clone(),
            label: tool.display_label().to_string(),
            kind: "custom".to_string(),
            program: tool.cmd.clone(),
            installed: false,
            version: None,
        });
    }
    Ok(out)
}

/// Best-effort version probe: runs `<program> --version` with a short timeout
/// and returns the first non-empty output line. Returns `None` if the program
/// can't be launched, times out, or prints nothing useful.
fn tool_version(program: &str) -> Option<String> {
    let program = program.trim();
    if program.is_empty() {
        return None;
    }
    let mut child = version_probe_command(program)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let timeout = Duration::from_secs(3);
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(40));
            }
            Err(_) => return None,
        }
    }
    let output = child.wait_with_output().ok()?;
    let raw = if output.stdout.iter().any(|b| !b.is_ascii_whitespace()) {
        String::from_utf8_lossy(&output.stdout)
    } else {
        String::from_utf8_lossy(&output.stderr)
    };
    let line = raw.lines().map(str::trim).find(|l| !l.is_empty())?;
    let trimmed: String = line.chars().take(80).collect();
    Some(trimmed)
}

/// Build the `<program> --version` probe invocation. npm installs tools on
/// Windows as `.cmd`/`.bat` batch shims, which `CreateProcessW` cannot execute
/// directly (the same reason relay sessions route them through `cmd.exe`), so
/// probe those via `cmd.exe /d /c` — with `CREATE_NO_WINDOW` so the probe never
/// flashes a console window. Real executables launch directly on all platforms.
fn version_probe_command(program: &str) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let batch_shim = resolve_on_path(program).filter(|resolved| {
            resolved
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"))
        });
        let mut cmd = match batch_shim {
            Some(resolved) => {
                let mut cmd = Command::new("cmd.exe");
                cmd.arg("/d").arg("/c").arg(resolved).arg("--version");
                cmd
            }
            None => {
                let mut cmd = Command::new(program);
                cmd.arg("--version");
                cmd
            }
        };
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd
    }
    #[cfg(not(windows))]
    {
        let mut cmd = Command::new(program);
        cmd.arg("--version");
        cmd
    }
}

/// Probe each configured tool (builtins + custom) and report whether its
/// launch program is currently installed. Powers the onboarding tool-check
/// page.
#[tauri::command]
async fn detect_tools() -> Result<Vec<ToolStatusDto>, String> {
    // Version probing spawns `<program> --version` subprocesses and blocks while
    // joining them. Run it on the blocking pool so the synchronous work never
    // stalls the main/UI thread (which would freeze the detection progress bar).
    tauri::async_runtime::spawn_blocking(detect_tools_blocking)
        .await
        .map_err(|e| e.to_string())?
}

fn detect_tools_blocking() -> Result<Vec<ToolStatusDto>, String> {
    let mut out = enumerate_tool_targets()?;
    for s in out.iter_mut() {
        // `shell` always launches the user's login shell, which is present by
        // definition; treat it as installed without probing.
        s.installed = s.name == "shell" || is_on_path(&s.program);
    }

    // Probe versions in parallel for installed tools (skip `shell`, whose
    // `--version` would just print the login shell's version, not a tool).
    let probes: Vec<_> = out
        .iter()
        .enumerate()
        .filter(|(_, s)| s.installed && s.name != "shell")
        .map(|(i, s)| {
            let program = s.program.clone();
            (i, std::thread::spawn(move || tool_version(&program)))
        })
        .collect();
    for (i, handle) in probes {
        if let Ok(version) = handle.join() {
            out[i].version = version;
        }
    }

    // Persist the sweep so the new-session picker can read availability without
    // re-probing. Best-effort: a write failure must not fail detection itself.
    let cache = ToolStatusCache {
        detected_at: now_unix(),
        tools: out
            .iter()
            .map(|s| ToolDetectionEntry {
                name: s.name.clone(),
                installed: s.installed,
                version: s.version.clone(),
            })
            .collect(),
    };
    if let Err(e) = save_tool_status(&cache) {
        eprintln!("relaycat: failed to persist tool status: {e}");
    }

    Ok(out)
}

/// Return tool availability from the persisted detection cache *without*
/// probing the filesystem. Tools that have never been detected report
/// `installed = true` so a fresh install never disables everything before the
/// first detection sweep has run.
#[tauri::command]
fn cached_tool_status() -> Result<Vec<ToolStatusDto>, String> {
    let cache = load_tool_status();
    let by_name: std::collections::HashMap<&str, &ToolDetectionEntry> =
        cache.tools.iter().map(|e| (e.name.as_str(), e)).collect();
    let mut out = enumerate_tool_targets()?;
    for s in out.iter_mut() {
        match by_name.get(s.name.as_str()) {
            Some(entry) => {
                s.installed = entry.installed;
                s.version = entry.version.clone();
            }
            None => s.installed = true,
        }
    }
    Ok(out)
}

#[tauri::command]
fn list_tools() -> Result<Vec<ToolDto>, String> {
    let config = load_config()?;
    let mut tools: Vec<ToolDto> = BUILTIN_TOOLS
        .iter()
        .map(|(name, label)| ToolDto {
            name: name.to_string(),
            label: label.to_string(),
            kind: "builtin".to_string(),
        })
        .collect();
    for tool in &config.tools {
        tools.push(ToolDto {
            name: tool.name.clone(),
            label: tool.display_label().to_string(),
            kind: "custom".to_string(),
        });
    }
    Ok(tools)
}

#[tauri::command]
fn get_config() -> Result<ConfigDto, String> {
    Ok(to_dto(&load_config()?))
}

#[tauri::command]
fn save_config(config: ConfigDto) -> Result<(), String> {
    let path = config::config_file_path().map_err(|e| e.to_string())?;
    from_dto(config).save(&path).map_err(|e| e.to_string())
}

#[tauri::command]
fn default_project() -> String {
    std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}

#[tauri::command]
fn list_recents() -> Result<Vec<RecentDto>, String> {
    let path = recent_store::recent_file_path().map_err(|e| e.to_string())?;
    let store = RecentStore::load(&path).map_err(|e| e.to_string())?;
    Ok(store
        .records()
        .iter()
        .map(|r| RecentDto {
            id: r.id.clone(),
            kind: r.kind.as_str().to_string(),
            project: r.project.display().to_string(),
            relay: r.relay.clone(),
        })
        .collect())
}

#[tauri::command]
fn forget_recent(id: String) -> Result<(), String> {
    let path = recent_store::recent_file_path().map_err(|e| e.to_string())?;
    let mut store = RecentStore::load(&path).map_err(|e| e.to_string())?;
    store.forget(&id).map_err(|e| e.to_string())?;
    store.save(&path).map_err(|e| e.to_string())
}

#[derive(Serialize)]
struct PairedDeviceDto {
    /// Recent-session id (stable handle for the project + tool pairing).
    id: String,
    kind: String,
    project: String,
    relay: String,
    /// A reusable pairing credential is stored on disk for this project + tool.
    paired: bool,
    /// Unix seconds the stored pairing was created (0 when not paired).
    created_at: u64,
    /// The stored pairing is past its TTL and would require a re-scan anyway.
    expired: bool,
    last_used_at: u64,
    use_count: u64,
}

/// List the projects/tools for which a reusable pairing credential is stored on
/// this machine — i.e. the "paired devices" the user can review or revoke.
#[tauri::command]
fn list_paired_devices() -> Result<Vec<PairedDeviceDto>, String> {
    let path = recent_store::recent_file_path().map_err(|e| e.to_string())?;
    let store = RecentStore::load(&path).map_err(|e| e.to_string())?;
    let now = pairing_store::current_unix_timestamp();
    let mut out = Vec::new();
    for r in store.records() {
        let session_path = pairing_store::session_file_path(&r.project, &r.kind);
        let stored = pairing_store::load_session(&session_path)
            .ok()
            .flatten();
        if let Some(s) = stored {
            out.push(PairedDeviceDto {
                id: r.id.clone(),
                kind: r.kind.as_str().to_string(),
                project: r.project.display().to_string(),
                relay: r.relay.clone(),
                paired: true,
                created_at: s.created_at_unix,
                expired: s.is_expired_at(now),
                last_used_at: r.last_used_at,
                use_count: r.use_count,
            });
        }
    }
    Ok(out)
}

/// Revoke the stored pairing credential for one project + tool: delete the
/// session file and its pairing QR images so the next session must re-scan.
#[tauri::command]
fn revoke_pairing(project: String, kind: String) -> Result<(), String> {
    let kind = SessionKind::new(kind).map_err(|e| e.to_string())?;
    remove_pairing_for(&PathBuf::from(project), &kind)
}

fn remove_if_exists(path: &Path) -> Result<bool, String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.to_string()),
    }
}

/// Remove the stored pairing session for `project`/`kind` plus its QR PNGs
/// (`latest-<kind>.png` and `<kind>-<room>.png`).
fn remove_pairing_for(project: &Path, kind: &SessionKind) -> Result<(), String> {
    remove_if_exists(&pairing_store::session_file_path(project, kind))?;
    let qr_dir = pairing_store::pairing_qr_dir(project);
    let latest = format!("latest-{}.png", kind.as_str());
    let prefix = format!("{}-", kind.as_str());
    if let Ok(entries) = std::fs::read_dir(&qr_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.ends_with(".png") && (name == latest || name.starts_with(&prefix)) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct ClearOpts {
    #[serde(default)]
    pairings: bool,
    #[serde(default)]
    logs: bool,
    #[serde(default)]
    recents: bool,
    #[serde(default)]
    crash_log: bool,
}

#[derive(Serialize, Default)]
struct ClearReport {
    pairings_removed: u32,
    logs_removed: u32,
    recents_removed: u32,
    crash_log_removed: bool,
}

/// One-click removal of sensitive on-disk data: stored pairing credentials,
/// per-project CLI logs, the recent-sessions list, and the GUI crash log. Each
/// category is opt-in. Running sessions are unaffected.
#[tauri::command]
fn clear_sensitive_data(opts: ClearOpts) -> Result<ClearReport, String> {
    let path = recent_store::recent_file_path().map_err(|e| e.to_string())?;
    let store = RecentStore::load(&path).map_err(|e| e.to_string())?;
    let mut report = ClearReport::default();

    if opts.pairings {
        for r in store.records() {
            let session_path = pairing_store::session_file_path(&r.project, &r.kind);
            if session_path.exists() {
                report.pairings_removed += 1;
            }
            remove_pairing_for(&r.project, &r.kind)?;
        }
    }

    if opts.logs {
        let mut seen = std::collections::HashSet::new();
        for r in store.records() {
            if !seen.insert(r.project.clone()) {
                continue;
            }
            let log_path = r
                .project
                .join(pairing_store::RELAYCAT_DIR)
                .join("cli.log");
            if remove_if_exists(&log_path)? {
                report.logs_removed += 1;
            }
        }
    }

    if opts.recents {
        report.recents_removed = store.records().len() as u32;
        RecentStore::new(Vec::new())
            .save(&path)
            .map_err(|e| e.to_string())?;
    }

    if opts.crash_log
        && let Some(p) = crash_log_path()
        && remove_if_exists(&p)?
    {
        report.crash_log_removed = true;
    }

    Ok(report)
}

#[tauri::command]
async fn check_relay_compatibility(
    app: AppHandle,
    relay_url: String,
) -> relay::RelayCompatibilityCheck {
    relay::probe_relay_compatibility(
        &relay_url,
        &app.package_info().version.to_string(),
        relaycat_cli::gui_bridge::RELAYCAT_CLI_VERSION,
    )
    .await
}

#[tauri::command]
fn create_session(
    app: AppHandle,
    state: State<SessionManager>,
    opts: CreateOpts,
) -> Result<SessionInfo, String> {
    let config = load_config()?;
    let id = if opts.id.is_empty() {
        format!("s{}", SESSION_SEQ.fetch_add(1, Ordering::Relaxed))
    } else {
        opts.id.clone()
    };
    let rows = opts.rows.max(1);
    let cols = opts.cols.max(1);

    let relay = opts
        .relay
        .as_ref()
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty());

    if let Some(relay_url) = relay {
        return create_relay_session(app, state, &opts, &config, id, rows, cols, relay_url);
    }

    let target = build_target(&opts, &config)?;
    let title = target.session_kind.as_str().to_string();
    state
        .spawn(app, id.clone(), &target, rows, cols)
        .map_err(|e| e.to_string())?;

    Ok(SessionInfo {
        id,
        title,
        mode: "local".to_string(),
        log_path: None,
    })
}

/// Relay sessions re-execute this GUI binary with a hidden subcommand so the
/// relay runs from the linked `relaycat-cli` source directly (inside a PTY the
/// GUI owns) — reusing the CLI's secure pairing + mirror engine verbatim
/// without depending on a separately compiled `relaycat` executable. See
/// [`crate::relay`] and [`relaycat_cli::gui_bridge`].
#[allow(clippy::too_many_arguments)]
fn create_relay_session(
    app: AppHandle,
    state: State<SessionManager>,
    opts: &CreateOpts,
    config: &Config,
    id: String,
    rows: u16,
    cols: u16,
    relay_url: String,
) -> Result<SessionInfo, String> {
    let exe = std::env::current_exe().map_err(|e| format!("无法定位 GUI 可执行文件：{e}"))?;

    let project = opts.project.as_deref();
    let mut args = vec![relaycat_cli::gui_bridge::RELAY_CHILD_ARG.to_string()];
    args.extend(relay::relay_args(
        &opts.tool,
        is_builtin(&opts.tool),
        config,
        project,
        &relay_url,
    )?);
    let project_dir = relay::project_dir(project);
    let session_kind = SessionKind::new(opts.tool.clone()).map_err(|e| e.to_string())?;

    let target = TargetCommand {
        program: exe.to_string_lossy().into_owned(),
        args,
        cwd: Some(project_dir.clone()),
        // PtySession re-runs this binary in relay-child mode; the relay logic
        // itself drives pairing/mirror via the linked CLI source.
        relay: None,
        session_kind,
    };
    let log_path = relay::cli_log_path(&project_dir);
    // Per-tab scope for the pairing, resize, and state side channels so
    // multiple relay tabs never read each other's pairing URL or state.
    let gui_session_id = relaycat_cli::gui_bridge::sanitize_session_id(&id);
    // The relay child writes the pairing URL here as a ConPTY-proof fallback to
    // the private OSC it also prints. Clear any stale file from a prior session
    // in this project so the poller never reports an old URL.
    let pairing_url_path = relaycat_cli::pairing_store::gui_pairing_url_path_for(
        &project_dir,
        &target.session_kind,
        Some(&gui_session_id),
    );
    let _ = std::fs::remove_file(&pairing_url_path);
    let _ = std::fs::remove_file(pairing_url_path.with_extension("txt.tmp"));
    // Windows pipe-bridge resize channel: the GUI rewrites this file on reflow
    // and the relay child polls it (a pipe carries no resize signal). Clear any
    // stale file so the child does not act on a previous session's size.
    let resize_file = project_dir
        .join(relaycat_cli::pairing_store::RELAYCAT_DIR)
        .join(format!(
            "gui-resize-{}-{}.txt",
            target.session_kind.as_str(),
            gui_session_id
        ));
    let _ = std::fs::remove_file(&resize_file);
    let _ = std::fs::remove_file(resize_file.with_extension("tmp"));
    let state_bridge =
        RelayStateBridge::bind(relaycat_cli::gui_bridge::new_gui_relay_state_token())
            .map_err(|e| e.to_string())?;
    let context = RelayContext {
        pairing_url_path,
        resize_file,
        gui_session_id,
        state_bridge,
    };

    state
        .spawn_relay(app, id.clone(), &target, rows, cols, context)
        .map_err(|e| e.to_string())?;

    // Remember the tool/project/relay so the next "new session" dialog can
    // pre-fill the relay (and the recents list offers a one-click re-launch),
    // mirroring how the CLI persists recent relay targets.
    if let Ok(logical) = build_target(opts, config) {
        record_recent(&logical);
    }

    Ok(SessionInfo {
        id,
        title: opts.tool.clone(),
        mode: "relay".to_string(),
        log_path: Some(log_path.display().to_string()),
    })
}

/// Persist a relay target to the recent store (best-effort; failures are
/// non-fatal and simply skip remembering).
fn record_recent(target: &TargetCommand) {
    let Ok(path) = recent_store::recent_file_path() else {
        return;
    };
    let Ok(mut store) = RecentStore::load(&path) else {
        return;
    };
    if store.upsert_target(target, relaycat_cli::pairing_store::current_unix_timestamp()) {
        let _ = store.save(&path);
    }
}

#[tauri::command]
fn write_session(state: State<SessionManager>, id: String, data: String) -> Result<(), String> {
    state.write(&id, data.as_bytes()).map_err(|e| e.to_string())
}

#[tauri::command]
fn resize_session(
    state: State<SessionManager>,
    id: String,
    rows: u16,
    cols: u16,
) -> Result<(), String> {
    state.resize(&id, rows, cols).map_err(|e| e.to_string())
}

#[tauri::command]
fn close_session(state: State<SessionManager>, id: String) -> Result<(), String> {
    state.close(&id).map_err(|e| e.to_string())
}

#[tauri::command]
fn get_relay_state(state: State<SessionManager>, id: String) -> Option<RelayEvent> {
    state
        .relay_state(&id)
        .map(|snapshot| RelayEvent::from_snapshot(id, &snapshot))
}

/// Render a QR code SVG for an arbitrary pairing URL. Relay sessions scrape the
/// real URL the CLI prints (`session://pairing`) and call this to draw it.
#[tauri::command]
fn render_qr(data: String) -> Result<String, String> {
    let data = data.trim();
    if data.is_empty() {
        return Err("empty QR payload".to_string());
    }
    render_qr_svg(data).map_err(|e| e.to_string())
}

#[tauri::command]
fn diagnostics() -> Result<DiagnosticsInfo, String> {
    Ok(DiagnosticsInfo {
        relay_engine: "in-process (linked relaycat-cli)".to_string(),
        gui_exe: std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        cli_version: relaycat_cli::gui_bridge::RELAYCAT_CLI_VERSION.to_string(),
        config_path: config::config_file_path()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        recent_path: recent_store::recent_file_path()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
    })
}

/// Read up to the last `max_bytes` of a file. Returns an empty string if the
/// file does not exist yet.
fn read_file_tail(path: &str, max_bytes: u64) -> Result<String, String> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(e) => return Err(e.to_string()),
    };
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    let cap = max_bytes.clamp(1, 4 * 1024 * 1024);
    let start = len.saturating_sub(cap);
    file.seek(SeekFrom::Start(start))
        .map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Read up to the last `max_bytes` of a (CLI log) file, for the diagnostics
/// panel's live tail.
#[tauri::command]
fn read_log_tail(path: String, max_bytes: u64) -> Result<String, String> {
    read_file_tail(&path, max_bytes)
}

/// Path of the GUI crash log, alongside the config file.
fn crash_log_path() -> Option<PathBuf> {
    config::config_file_path()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("gui-crash.log")))
}

/// Redact sensitive material so a diagnostics bundle can be shared safely:
/// collapse the home directory, strip URL query/fragments (pairing material),
/// blank out long opaque tokens (keys/base64/hex) and IPv4 addresses.
fn redact(input: &str) -> String {
    use regex::Regex;
    use std::sync::OnceLock;
    static URL_TAIL: OnceLock<Regex> = OnceLock::new();
    static TOKEN: OnceLock<Regex> = OnceLock::new();
    static IPV4: OnceLock<Regex> = OnceLock::new();

    let mut out = input.to_string();

    for var in ["HOME", "USERPROFILE"] {
        if let Some(home) = std::env::var_os(var) {
            let home = home.to_string_lossy().to_string();
            if home.len() >= 3 {
                out = out.replace(&home, "~");
            }
        }
    }

    let url_tail = URL_TAIL.get_or_init(|| Regex::new(r"([?#])[^\s)\]]+").unwrap());
    out = url_tail.replace_all(&out, "$1[REDACTED]").into_owned();

    let token = TOKEN.get_or_init(|| Regex::new(r"[A-Za-z0-9_-]{32,}={0,2}").unwrap());
    out = token.replace_all(&out, "[REDACTED]").into_owned();

    let ipv4 = IPV4.get_or_init(|| Regex::new(r"\b\d{1,3}(?:\.\d{1,3}){3}\b").unwrap());
    out = ipv4.replace_all(&out, "[REDACTED-IP]").into_owned();

    out
}

/// Build a redacted, shareable diagnostics report (environment + crash log +
/// the active relay session log). `app_version` / `generated_at` come from the
/// webview so the report matches what the user sees.
#[tauri::command]
fn build_diagnostics_report(
    app_version: String,
    generated_at: String,
    log_path: Option<String>,
) -> Result<String, String> {
    let info = diagnostics()?;
    let mut s = String::new();
    s.push_str("RelayCat GUI Diagnostics Report\n");
    s.push_str(&format!("Generated: {generated_at}\n"));
    s.push_str(&format!("App version: {app_version}\n"));
    s.push_str(&format!("CLI version: {}\n", info.cli_version));
    s.push_str(&format!(
        "OS: {} / {}\n",
        std::env::consts::OS,
        std::env::consts::ARCH
    ));

    s.push_str("\n[Environment]\n");
    s.push_str(&format!("Relay engine: {}\n", info.relay_engine));
    s.push_str(&format!("GUI executable: {}\n", info.gui_exe));
    s.push_str(&format!("Config file: {}\n", info.config_path));
    s.push_str(&format!("Recents file: {}\n", info.recent_path));

    s.push_str("\n[Crash log]\n");
    match crash_log_path() {
        Some(p) => {
            let tail = read_file_tail(&p.to_string_lossy(), 64 * 1024).unwrap_or_default();
            s.push_str(if tail.trim().is_empty() {
                "(none)\n"
            } else {
                &tail
            });
            if !tail.is_empty() && !tail.ends_with('\n') {
                s.push('\n');
            }
        }
        None => s.push_str("(unavailable)\n"),
    }

    s.push_str("\n[Active session log]\n");
    match log_path {
        Some(p) if !p.is_empty() => {
            let tail = read_file_tail(&p, 256 * 1024).unwrap_or_default();
            s.push_str(if tail.trim().is_empty() {
                "(empty)\n"
            } else {
                &tail
            });
            if !tail.is_empty() && !tail.ends_with('\n') {
                s.push('\n');
            }
        }
        _ => s.push_str("(no active relay session)\n"),
    }

    Ok(redact(&s))
}

/// Write text to a user-chosen path (destination picked via the save dialog).
#[tauri::command]
fn write_text_file(path: String, contents: String) -> Result<(), String> {
    std::fs::write(&path, contents).map_err(|e| e.to_string())
}

/// Append GUI panics to the crash log so they surface in diagnostics exports.
fn install_crash_logging() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(path) = crash_log_path() {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                use std::io::Write;
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let loc = info
                    .location()
                    .map(|l| format!("{}:{}", l.file(), l.line()))
                    .unwrap_or_else(|| "unknown".to_string());
                let msg = info
                    .payload()
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| info.payload().downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic".to_string());
                let backtrace = std::backtrace::Backtrace::force_capture();
                let _ = writeln!(file, "[{ts}] panic at {loc}: {msg}\n{backtrace}\n");
            }
        }
        previous(info);
    }));
}

/// Open an external https URL in the user's default browser.
///
/// Restricted to https URLs for official RelayCat pages and the source repo
/// links referenced from the UI.
#[tauri::command]
async fn open_url(url: String) -> Result<(), String> {
    let allowed = url.starts_with("https://")
        && {
            let host = url
                .trim_start_matches("https://")
                .split(['/', '?', '#'])
                .next()
                .unwrap_or("");
            host == "relaycat.cn"
                || host == "www.relaycat.cn"
                || host == "relaycat.app"
                || host == "www.relaycat.app"
                || (host == "github.com" && url == "https://github.com/jackyZhang8/relaycat-core")
        };
    if !allowed {
        return Err("blocked url".into());
    }

    #[cfg(target_os = "windows")]
    let result = {
        use std::os::windows::process::CommandExt;
        // Launch via `cmd /C start`, but with CREATE_NO_WINDOW so the helper
        // never flashes a black console window before the browser appears
        // (which looked alarmingly like malware). The browser itself is a GUI
        // app and is unaffected.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new("cmd")
            .args(["/C", "start", "", &url])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
    };
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(&url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = std::process::Command::new("xdg-open").arg(&url).spawn();

    result.map(|_| ()).map_err(|e| e.to_string())
}

/// Write a PNG image (rasterized in the webview, e.g. the pairing QR) to the
/// system clipboard. The webview's `navigator.clipboard` cannot reliably put
/// images on the clipboard across platforms (notably WebKitGTK), so image
/// copies go through the Tauri clipboard plugin instead.
#[tauri::command]
fn copy_image_png(app: AppHandle, bytes: Vec<u8>) -> Result<(), String> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    let image = tauri::image::Image::from_bytes(&bytes).map_err(|e| e.to_string())?;
    app.clipboard()
        .write_image(&image)
        .map_err(|e| e.to_string())
}

fn render_qr_svg(data: &str) -> anyhow::Result<String> {
    use qrcode::QrCode;
    use qrcode::render::svg;

    let code = QrCode::new(data.as_bytes())?;
    let image = code
        .render::<svg::Color>()
        .min_dimensions(180, 180)
        .quiet_zone(false)
        .dark_color(svg::Color("#0d1117"))
        .light_color(svg::Color("#ffffff"))
        .build();
    Ok(image)
}

/// Build the system tray icon (show / quit), so the window can be closed to
/// tray and reopened later.
fn setup_tray(app: &AppHandle) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    let show = MenuItem::with_id(app, "tray-show", "显示主窗口 · Show", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "tray-quit", "退出 · Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;

    let builder = TrayIconBuilder::with_id("main")
        .tooltip("RelayCat")
        .icon(tauri::include_image!("icons/tray.png"))
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "tray-show" => show_main_window(app),
            "tray-quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });
    let tray = builder.build(app)?;
    configure_macos_tray_button(&tray)?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn configure_macos_tray_button<R: tauri::Runtime>(
    tray: &tauri::tray::TrayIcon<R>,
) -> tauri::Result<()> {
    use objc2_app_kit::{NSCellImagePosition, NSImageScaling};
    use objc2_foundation::{MainThreadMarker, NSSize};

    tray.with_inner_tray_icon(|inner| {
        let Some(status_item) = inner.ns_status_item() else {
            return;
        };
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let Some(button) = status_item.button(mtm) else {
            return;
        };

        if let Some(image) = button.image() {
            image.setSize(NSSize::new(18.0, 18.0));
            button.setImage(Some(&image));
        }
        button.setImagePosition(NSCellImagePosition::ImageOnly);
        button.setImageScaling(NSImageScaling::ScaleProportionallyDown);
    })
}

#[cfg(not(target_os = "macos"))]
fn configure_macos_tray_button<R: tauri::Runtime>(
    _tray: &tauri::tray::TrayIcon<R>,
) -> tauri::Result<()> {
    Ok(())
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// When the app is launched from a desktop environment (double-clicking the
/// bundled app) rather than a terminal, the process only inherits a minimal
/// PATH and none of the variables set in the user's shell startup files
/// (`~/.bashrc`, `~/.zshrc`, `~/.profile`, nvm/volta/cargo init, API keys,
/// proxies, …). That makes tool detection, version probes, and — more
/// importantly — actually launching the coding agents behave differently from
/// running via `dev.sh` in a terminal.
///
/// To make both paths consistent we ask the user's login shell for its full
/// environment once at startup and merge it in: PATH is replaced with the
/// login shell's PATH, and any variable not already present in this process is
/// imported (so we never clobber what the desktop session set, e.g. DISPLAY /
/// XDG_* / DBUS_SESSION_BUS_ADDRESS). Skipped when launched from a terminal
/// (where the environment is already complete) and on Windows (PATH is
/// inherited there).
#[cfg(unix)]
fn hydrate_env_from_login_shell() {
    use std::io::IsTerminal;

    // A terminal launch (e.g. `dev.sh`/`tauri dev`) already has the full env.
    if std::io::stdout().is_terminal() || std::io::stderr().is_terminal() {
        return;
    }

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());

    // `-i -l -c` loads interactive + login startup files so we see the same
    // PATH/vars the user gets in their terminal. Emit env null-separated so
    // values containing newlines survive.
    let mut child = match Command::new(&shell)
        .args(["-ilc", "/usr/bin/env -0 2>/dev/null || env -0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return,
    };

    let timeout = Duration::from_secs(5);
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return;
                }
                std::thread::sleep(Duration::from_millis(40));
            }
            Err(_) => return,
        }
    }

    let Ok(output) = child.wait_with_output() else {
        return;
    };
    if !output.status.success() {
        return;
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    for entry in raw.split('\0') {
        let Some((key, value)) = entry.split_once('=') else {
            continue;
        };
        if key.is_empty() {
            continue;
        }
        // Always adopt the login shell's PATH; for everything else only fill in
        // variables this process doesn't already have, so we don't override the
        // desktop session's own values.
        if key == "PATH" {
            if !value.is_empty() {
                // SAFETY: set during startup, before any threads spawn.
                unsafe { std::env::set_var("PATH", value) };
            }
        } else if std::env::var_os(key).is_none() {
            // SAFETY: set during startup, before any threads spawn.
            unsafe { std::env::set_var(key, value) };
        }
    }
}

#[cfg(not(unix))]
fn hydrate_env_from_login_shell() {}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    hydrate_env_from_login_shell();
    install_crash_logging();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None::<Vec<&str>>,
        ))
        .setup(|app| {
            setup_tray(app.handle())?;
            Ok(())
        })
        .manage(SessionManager::default())
        .invoke_handler(tauri::generate_handler![
            list_tools,
            detect_tools,
            cached_tool_status,
            get_config,
            save_config,
            default_project,
            list_recents,
            forget_recent,
            list_paired_devices,
            revoke_pairing,
            clear_sensitive_data,
            check_relay_compatibility,
            create_session,
            write_session,
            resize_session,
            close_session,
            get_relay_state,
            render_qr,
            open_url,
            diagnostics,
            read_log_tail,
            build_diagnostics_report,
            write_text_file,
            copy_image_png,
        ])
        .run(tauri::generate_context!())
        .expect("error while running RelayCat GUI");
}
