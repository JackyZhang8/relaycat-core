//! Relay-mode support for the GUI.
//!
//! Local sessions spawn the target tool directly in a PTY. Relay sessions
//! re-execute the GUI binary in relay-child mode (see
//! [`relaycat_cli::gui_bridge`]) inside the PTY: the linked CLI source owns the
//! battle-tested secure-pairing + terminal-mirror engine that the iOS / Android
//! apps depend on, so the GUI reuses it verbatim rather than re-implementing the
//! sensitive relay protocol — and without depending on a separately compiled
//! `relaycat` executable. The GUI is then just a terminal host that renders the
//! relay session locally (xterm.js), forwards keystrokes, and surfaces the
//! pairing QR / status to the native UI by observing its output and log file.
//!
//! This module builds the `relaycat`-style argument vector for that child and
//! resolves the project directory / CLI log path it uses.

use std::path::{Path, PathBuf};

use relaycat_cli::config::Config;
use relaycat_cli::pairing_store::RELAYCAT_DIR;

/// Build the `relaycat <subcommand> --relay <url>` argument vector for a relay
/// session. `tool` is a built-in kind (shell/codex/...) or a custom tool name.
pub fn relay_args(
    tool: &str,
    is_builtin: bool,
    config: &Config,
    project: Option<&str>,
    relay_url: &str,
) -> Result<Vec<String>, String> {
    let mut args: Vec<String> = Vec::new();

    if is_builtin {
        args.push(tool.to_string());
        push_project(&mut args, project);
        args.push("--relay".to_string());
        args.push(relay_url.to_string());
        return Ok(args);
    }

    let custom = config
        .tools
        .iter()
        .find(|t| t.name == tool)
        .ok_or_else(|| format!("unknown tool `{tool}`"))?;
    args.push("tool".to_string());
    args.push(custom.name.clone());
    args.push("--cmd".to_string());
    args.push(custom.cmd.clone());
    push_project(&mut args, project);
    args.push("--relay".to_string());
    args.push(relay_url.to_string());
    if !custom.args.is_empty() {
        args.push("--".to_string());
        args.extend(custom.args.iter().cloned());
    }
    Ok(args)
}

fn push_project(args: &mut Vec<String>, project: Option<&str>) {
    if let Some(project) = project.map(str::trim).filter(|p| !p.is_empty()) {
        args.push("--project".to_string());
        args.push(project.to_string());
    }
}

/// The working directory a relay session runs in, used both as the child cwd
/// and to locate the CLI log file the GUI tails for pairing status.
pub fn project_dir(project: Option<&str>) -> PathBuf {
    project
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

/// Path to the CLI log file (`<project>/.relaycat/cli.log`) that the relay CLI
/// appends pairing lifecycle lines to.
pub fn cli_log_path(project_dir: &Path) -> PathBuf {
    project_dir.join(RELAYCAT_DIR).join("cli.log")
}
