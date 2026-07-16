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
use std::time::Duration;

use relaycat_cli::config::Config;
use relaycat_cli::pairing_store::RELAYCAT_DIR;
use semver::Version;
use serde::{Deserialize, Serialize};
use url::Url;

const SUPPORTED_RELAY_PROTOCOLS: &[u16] = &[1];

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct RelayHealthResponse {
    pub status: String,
    pub version: String,
    #[serde(default)]
    pub protocol_version: Option<u16>,
    #[serde(default)]
    pub min_gui_version: Option<String>,
    #[serde(default)]
    pub min_cli_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RelayCompatibilityStatus {
    Compatible,
    Incompatible,
    Unverified,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RelayCompatibilityCheck {
    pub status: RelayCompatibilityStatus,
    pub action: Option<String>,
    pub server_version: Option<String>,
    pub min_gui_version: Option<String>,
    pub min_cli_version: Option<String>,
    pub reason: String,
}

impl RelayCompatibilityCheck {
    fn compatible(
        server_version: &str,
        min_gui_version: Option<String>,
        min_cli_version: Option<String>,
    ) -> Self {
        Self {
            status: RelayCompatibilityStatus::Compatible,
            action: None,
            server_version: Some(server_version.to_string()),
            min_gui_version,
            min_cli_version,
            reason: "compatible".to_string(),
        }
    }

    fn incompatible(
        action: &str,
        server_version: &str,
        min_gui_version: Option<String>,
        min_cli_version: Option<String>,
        reason: String,
    ) -> Self {
        Self {
            status: RelayCompatibilityStatus::Incompatible,
            action: Some(action.to_string()),
            server_version: Some(server_version.to_string()),
            min_gui_version,
            min_cli_version,
            reason,
        }
    }

    fn unverified(reason: String) -> Self {
        Self {
            status: RelayCompatibilityStatus::Unverified,
            action: None,
            server_version: None,
            min_gui_version: None,
            min_cli_version: None,
            reason,
        }
    }
}

pub fn evaluate_relay_health(
    health: &RelayHealthResponse,
    gui_version: &str,
    cli_version: &str,
) -> RelayCompatibilityCheck {
    let protocol = health.protocol_version.unwrap_or(1);
    let min_supported = *SUPPORTED_RELAY_PROTOCOLS
        .iter()
        .min()
        .expect("protocol list is nonempty");
    let max_supported = *SUPPORTED_RELAY_PROTOCOLS
        .iter()
        .max()
        .expect("protocol list is nonempty");

    if !SUPPORTED_RELAY_PROTOCOLS.contains(&protocol) {
        let action = if protocol > max_supported {
            "upgrade_gui"
        } else if protocol < min_supported {
            "upgrade_server"
        } else {
            "upgrade_gui"
        };
        return RelayCompatibilityCheck::incompatible(
            action,
            &health.version,
            health.min_gui_version.clone(),
            health.min_cli_version.clone(),
            format!("unsupported relay protocol {protocol}"),
        );
    }

    if let Some(minimum) = health.min_gui_version.as_deref() {
        let current = Version::parse(gui_version);
        let required = Version::parse(minimum);
        if matches!((current, required), (Ok(current), Ok(required)) if current < required) {
            return RelayCompatibilityCheck::incompatible(
                "upgrade_gui",
                &health.version,
                health.min_gui_version.clone(),
                health.min_cli_version.clone(),
                format!("GUI {gui_version} is below required {minimum}"),
            );
        }
    }

    if let Some(minimum) = health.min_cli_version.as_deref() {
        let current = Version::parse(cli_version);
        let required = Version::parse(minimum);
        if matches!((current, required), (Ok(current), Ok(required)) if current < required) {
            return RelayCompatibilityCheck::incompatible(
                "upgrade_gui",
                &health.version,
                health.min_gui_version.clone(),
                health.min_cli_version.clone(),
                format!("embedded CLI {cli_version} is below required {minimum}"),
            );
        }
    }

    RelayCompatibilityCheck::compatible(
        &health.version,
        health.min_gui_version.clone(),
        health.min_cli_version.clone(),
    )
}

pub fn relay_health_url(relay_url: &str) -> Result<Url, String> {
    let mut url = Url::parse(relay_url).map_err(|error| error.to_string())?;
    let target_scheme = match url.scheme() {
        "wss" => "https",
        "ws" => "http",
        "https" => "https",
        "http" => "http",
        scheme => return Err(format!("unsupported relay URL scheme {scheme}")),
    };
    url.set_scheme(target_scheme)
        .map_err(|_| "failed to map relay URL scheme".to_string())?;

    let path = url.path().trim_end_matches('/');
    let base = path.strip_suffix("/ws").unwrap_or(path);
    let health_path = if base.is_empty() {
        "/".to_string()
    } else {
        format!("{base}/")
    };
    url.set_path(&health_path);
    url.set_fragment(None);
    Ok(url)
}

pub async fn probe_relay_compatibility(
    relay_url: &str,
    gui_version: &str,
    cli_version: &str,
) -> RelayCompatibilityCheck {
    let health_url = match relay_health_url(relay_url) {
        Ok(url) => url,
        Err(error) => return RelayCompatibilityCheck::unverified(error),
    };
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
    {
        Ok(client) => client,
        Err(error) => return RelayCompatibilityCheck::unverified(error.to_string()),
    };
    let response = match client.get(health_url).send().await {
        Ok(response) => response,
        Err(error) => return RelayCompatibilityCheck::unverified(error.to_string()),
    };
    if !response.status().is_success() {
        return RelayCompatibilityCheck::unverified(format!(
            "health endpoint returned {}",
            response.status()
        ));
    }
    match response.json::<RelayHealthResponse>().await {
        Ok(health) => evaluate_relay_health(&health, gui_version, cli_version),
        Err(error) => RelayCompatibilityCheck::unverified(error.to_string()),
    }
}

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

#[cfg(test)]
mod relay_compatibility_tests {
    use super::*;

    fn health(
        protocol_version: Option<u16>,
        min_gui_version: Option<&str>,
        min_cli_version: Option<&str>,
    ) -> RelayHealthResponse {
        RelayHealthResponse {
            status: "running".to_string(),
            version: "v0.1.2".to_string(),
            protocol_version,
            min_gui_version: min_gui_version.map(str::to_string),
            min_cli_version: min_cli_version.map(str::to_string),
        }
    }

    #[test]
    fn accepts_supported_protocol_and_gui_version() {
        assert_eq!(
            evaluate_relay_health(&health(Some(1), Some("0.1.5"), None), "0.1.5", "0.1.2").status,
            RelayCompatibilityStatus::Compatible,
        );
    }

    #[test]
    fn asks_for_gui_upgrade_when_server_protocol_is_newer() {
        let result = evaluate_relay_health(&health(Some(2), None, None), "0.1.5", "0.1.2");
        assert_eq!(result.status, RelayCompatibilityStatus::Incompatible);
        assert_eq!(result.action.as_deref(), Some("upgrade_gui"));
    }

    #[test]
    fn asks_for_server_upgrade_when_server_protocol_is_older() {
        let result = evaluate_relay_health(&health(Some(0), None, None), "0.1.5", "0.1.2");
        assert_eq!(result.status, RelayCompatibilityStatus::Incompatible);
        assert_eq!(result.action.as_deref(), Some("upgrade_server"));
    }

    #[test]
    fn asks_for_gui_upgrade_when_gui_is_below_server_minimum() {
        let result =
            evaluate_relay_health(&health(Some(1), Some("0.1.6"), None), "0.1.5", "0.1.2");
        assert_eq!(result.status, RelayCompatibilityStatus::Incompatible);
        assert_eq!(result.action.as_deref(), Some("upgrade_gui"));
    }

    #[test]
    fn accepts_legacy_health_without_compatibility_fields_as_protocol_one() {
        assert_eq!(
            evaluate_relay_health(&health(None, None, None), "0.1.5", "0.1.2").status,
            RelayCompatibilityStatus::Compatible,
        );
    }

    #[test]
    fn asks_for_gui_upgrade_when_embedded_cli_is_below_server_minimum() {
        let result = evaluate_relay_health(
            &health(Some(1), None, Some("0.1.3")),
            "0.1.5",
            "0.1.2",
        );
        assert_eq!(result.status, RelayCompatibilityStatus::Incompatible);
        assert_eq!(result.action.as_deref(), Some("upgrade_gui"));
    }

    #[test]
    fn builds_health_url_from_websocket_relay_url() {
        assert_eq!(
            relay_health_url("wss://relay.example.com/ws?token=abc")
                .unwrap()
                .as_str(),
            "https://relay.example.com/?token=abc",
        );
        assert_eq!(
            relay_health_url("ws://127.0.0.1:8787").unwrap().as_str(),
            "http://127.0.0.1:8787/",
        );
    }

    #[test]
    fn marks_an_invalid_relay_url_as_unverified() {
        let result = tauri::async_runtime::block_on(probe_relay_compatibility(
            "not a URL",
            "0.1.5",
            "0.1.2",
        ));
        assert_eq!(result.status, RelayCompatibilityStatus::Unverified);
    }
}
