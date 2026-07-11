//! User configuration for the launcher (`~/.config/relaycat/config.json` on
//! Unix-like systems, `%APPDATA%\relaycat\config.json` on Windows).
//!
//! The config is optional: a missing file behaves exactly like
//! [`Config::default`]. It lets the TUI launcher remember a default relay and
//! tool, pin favorite projects, register custom tools (e.g. Gemini, Aider), and
//! choose whether the launcher reappears after a session ends.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const CONFIG_FILE: &str = "config.json";
const RELAYCAT_CONFIG_DIR: &str = "relaycat";

/// A user-defined tool the launcher can start, in addition to the built-in
/// shell/codex/claude/opencode/gemini/aider entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomTool {
    /// Stable id/kind shown in the app and recent list (e.g. `gemini`).
    pub name: String,
    /// Optional display label for the picker. Falls back to `name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Program to execute.
    pub cmd: String,
    /// Arguments passed to the program.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
}

impl CustomTool {
    /// The label to show in the picker.
    pub fn display_label(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.name)
    }
}

/// Launcher configuration. All fields are optional in the on-disk JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// Relay URL pre-filled when a project has no remembered relay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_relay: Option<String>,
    /// Tool selected first in the picker (built-in kind or custom tool name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_tool: Option<String>,
    /// Favorite project directories, pinned to the top of the project picker.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub favorites: Vec<PathBuf>,
    /// User-defined tools shown alongside the built-ins.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<CustomTool>,
    /// When true, the launcher reappears after a session ends instead of the
    /// process exiting.
    #[serde(default = "default_true")]
    pub return_to_launcher: bool,
}

fn default_true() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            default_relay: None,
            default_tool: None,
            favorites: Vec::new(),
            tools: Vec::new(),
            return_to_launcher: true,
        }
    }
}

impl Config {
    /// Load the config from `path`, returning [`Config::default`] when the file
    /// does not exist. Invalid JSON is an error.
    pub fn load(path: &Path) -> Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(err) => {
                return Err(err).with_context(|| format!("failed to read {}", path.display()));
            }
        };
        serde_json::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))
    }

    /// Persist the config to `path`, creating the parent directory if needed.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let text =
            serde_json::to_string_pretty(self).context("failed to encode relaycat config")?;
        fs::write(path, format!("{text}\n"))
            .with_context(|| format!("failed to write {}", path.display()))
    }

    /// Serialize to pretty JSON (used by `relaycat config`).
    pub fn to_pretty(&self) -> Result<String> {
        serde_json::to_string_pretty(self).context("failed to encode relaycat config")
    }

    /// Add `project` to the favorites if it is not already present. Returns
    /// `true` when the favorites changed.
    pub fn add_favorite(&mut self, project: PathBuf) -> bool {
        if self.favorites.iter().any(|path| path == &project) {
            return false;
        }
        self.favorites.push(project);
        true
    }

    /// Remove `project` from the favorites. Returns `true` when it was present.
    pub fn remove_favorite(&mut self, project: &Path) -> bool {
        let before = self.favorites.len();
        self.favorites.retain(|path| path != project);
        self.favorites.len() != before
    }

    /// Toggle `project`'s favorite status, returning the new state (`true` when
    /// it is now a favorite).
    pub fn toggle_favorite(&mut self, project: PathBuf) -> bool {
        if self.remove_favorite(&project) {
            false
        } else {
            self.favorites.push(project);
            true
        }
    }

    /// Whether `project` is currently a favorite.
    pub fn is_favorite(&self, project: &Path) -> bool {
        self.favorites.iter().any(|path| path == project)
    }
}

/// Path to the config file: `$XDG_CONFIG_HOME/relaycat/config.json` on all
/// platforms, falling back to `%APPDATA%\relaycat\config.json` on Windows or
/// `$HOME/.config/relaycat/config.json` on Unix-like systems.
pub fn config_file_path() -> Result<PathBuf> {
    Ok(relaycat_config_dir()?.join(CONFIG_FILE))
}

pub(crate) fn relaycat_config_dir() -> Result<PathBuf> {
    relaycat_config_dir_from_env(&ConfigDirEnv::from_process(), cfg!(windows))
}

#[derive(Debug, Default)]
pub(crate) struct ConfigDirEnv {
    xdg_config_home: Option<PathBuf>,
    appdata: Option<PathBuf>,
    userprofile: Option<PathBuf>,
    home: Option<PathBuf>,
}

impl ConfigDirEnv {
    fn from_process() -> Self {
        Self {
            xdg_config_home: std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
            appdata: std::env::var_os("APPDATA").map(PathBuf::from),
            userprofile: std::env::var_os("USERPROFILE").map(PathBuf::from),
            home: std::env::var_os("HOME").map(PathBuf::from),
        }
    }
}

pub(crate) fn relaycat_config_dir_from_env(
    env: &ConfigDirEnv,
    is_windows: bool,
) -> Result<PathBuf> {
    Ok(config_base_dir_from_env(env, is_windows)?.join(RELAYCAT_CONFIG_DIR))
}

fn config_base_dir_from_env(env: &ConfigDirEnv, is_windows: bool) -> Result<PathBuf> {
    if let Some(path) = &env.xdg_config_home {
        return Ok(path.clone());
    }

    if is_windows {
        if let Some(path) = &env.appdata {
            return Ok(path.clone());
        }
        if let Some(path) = &env.userprofile {
            return Ok(path.join("AppData").join("Roaming"));
        }
        return env
            .home
            .as_ref()
            .map(|path| path.join(".config"))
            .context("APPDATA, USERPROFILE, and HOME are not set");
    }

    env.home
        .as_ref()
        .map(|path| path.join(".config"))
        .context("HOME is not set")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_default() {
        let path = std::env::temp_dir().join("relaycat-config-does-not-exist-xyz.json");
        let _ = fs::remove_file(&path);
        assert_eq!(Config::load(&path).unwrap(), Config::default());
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("relaycat-config-test-{}", std::process::id()));
        let path = dir.join("config.json");
        let config = Config {
            default_relay: Some("ws://127.0.0.1:8787".to_string()),
            default_tool: Some("codex".to_string()),
            favorites: vec![PathBuf::from("/work/alpha")],
            tools: vec![CustomTool {
                name: "gemini".to_string(),
                label: Some("Gemini".to_string()),
                cmd: "gemini".to_string(),
                args: vec!["--model".to_string(), "flash".to_string()],
            }],
            return_to_launcher: false,
        };
        config.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), config);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn partial_json_fills_defaults() {
        let dir =
            std::env::temp_dir().join(format!("relaycat-config-partial-{}", std::process::id()));
        let path = dir.join("config.json");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, r#"{"default_tool":"claude"}"#).unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(config.default_tool.as_deref(), Some("claude"));
        assert_eq!(config.default_relay, None);
        assert!(config.favorites.is_empty());
        // `return_to_launcher` defaults to true even when omitted.
        assert!(config.return_to_launcher);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn favorite_toggle_is_idempotent() {
        let mut config = Config::default();
        assert!(config.toggle_favorite(PathBuf::from("/work/alpha")));
        assert!(config.is_favorite(Path::new("/work/alpha")));
        assert!(!config.add_favorite(PathBuf::from("/work/alpha")));
        assert!(!config.toggle_favorite(PathBuf::from("/work/alpha")));
        assert!(!config.is_favorite(Path::new("/work/alpha")));
    }

    #[test]
    fn windows_config_dir_uses_appdata_without_home() {
        let env = ConfigDirEnv {
            appdata: Some(PathBuf::from("C:/Users/alice/AppData/Roaming")),
            ..ConfigDirEnv::default()
        };

        assert_eq!(
            relaycat_config_dir_from_env(&env, true).unwrap(),
            PathBuf::from("C:/Users/alice/AppData/Roaming").join("relaycat")
        );
    }
}
