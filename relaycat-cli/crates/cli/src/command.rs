use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::args::{Cli, Command};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionKind(String);

impl SessionKind {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_session_kind(&value).with_context(|| format!("invalid session kind `{value}`"))?;
        Ok(Self(value))
    }

    pub fn codex() -> Self {
        Self("codex".to_string())
    }

    pub fn claude() -> Self {
        Self("claude".to_string())
    }

    pub fn opencode() -> Self {
        Self("opencode".to_string())
    }

    pub fn gemini() -> Self {
        Self("gemini".to_string())
    }

    pub fn aider() -> Self {
        Self("aider".to_string())
    }

    pub fn shell() -> Self {
        Self("shell".to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_shell(&self) -> bool {
        self.0 == "shell"
    }

    /// Full-screen agents that drive the alternate screen with a
    /// bottom-anchored status/input UI (codex, opencode). For these the relay
    /// strips the alternate-screen switch from the app-facing stream and records
    /// primary-screen frames, so the conversation stays in the app's managed
    /// scrollback instead of a fixed alt-screen viewport. Inline agents
    /// (claude, shell) render in the primary buffer and need neither.
    pub fn uses_managed_alt_screen(&self) -> bool {
        matches!(self.0.as_str(), "codex" | "opencode")
    }
}

impl Default for SessionKind {
    fn default() -> Self {
        SessionKind::shell()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetCommand {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub relay: Option<RelayOptions>,
    pub session_kind: SessionKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayOptions {
    pub url: String,
    pub room_id: Option<String>,
}

impl TargetCommand {
    pub fn from_cli(cli: &Cli) -> Result<Self> {
        let command = cli.command.as_ref().context("no subcommand provided")?;
        Self::from_command(command)
    }

    pub fn from_command(command: &Command) -> Result<Self> {
        match command {
            Command::Shell(shell) => {
                let (program, args) = match shell.cmd.clone() {
                    Some(program) => (program, Vec::new()),
                    None => default_shell_command(),
                };
                Ok(Self {
                    program,
                    args,
                    cwd: Some(agent_cwd(shell.project.as_ref())?),
                    relay: relay_options(shell.relay.as_ref()),
                    session_kind: SessionKind::shell(),
                })
            }
            Command::Claude(agent) => Ok(Self {
                program: "claude".to_string(),
                args: Vec::new(),
                cwd: Some(agent_cwd(agent.project.as_ref())?),
                relay: relay_options(agent.relay.as_ref()),
                session_kind: SessionKind::claude(),
            }),
            Command::Codex(agent) => Ok(Self {
                program: "codex".to_string(),
                args: codex_default_args(),
                cwd: Some(agent_cwd(agent.project.as_ref())?),
                relay: relay_options(agent.relay.as_ref()),
                session_kind: SessionKind::codex(),
            }),
            Command::Opencode(agent) => Ok(Self {
                program: "opencode".to_string(),
                args: Vec::new(),
                cwd: Some(agent_cwd(agent.project.as_ref())?),
                relay: relay_options(agent.relay.as_ref()),
                session_kind: SessionKind::opencode(),
            }),
            Command::Gemini(agent) => Ok(Self {
                program: "gemini".to_string(),
                args: Vec::new(),
                cwd: Some(agent_cwd(agent.project.as_ref())?),
                relay: relay_options(agent.relay.as_ref()),
                session_kind: SessionKind::gemini(),
            }),
            Command::Aider(agent) => Ok(Self {
                program: "aider".to_string(),
                args: Vec::new(),
                cwd: Some(agent_cwd(agent.project.as_ref())?),
                relay: relay_options(agent.relay.as_ref()),
                session_kind: SessionKind::aider(),
            }),
            Command::Tool(tool) => Ok(Self {
                program: tool.cmd.clone(),
                args: tool.args.clone(),
                cwd: Some(agent_cwd(tool.project.as_ref())?),
                relay: relay_options(tool.relay.as_ref()),
                session_kind: SessionKind::new(tool.name.clone())?,
            }),
            Command::Tui(_)
            | Command::Qr(_)
            | Command::Recent(_)
            | Command::Run(_)
            | Command::Forget(_)
            | Command::Config(_)
            | Command::Update(_) => {
                bail!("recent commands do not map directly to a target command")
            }
        }
    }

    /// Build a target for a known built-in session kind. Used by the TUI
    /// launcher (and other non-clap callers) so the program/arg defaults stay
    /// in one place. Custom tools need an explicit program, so are not
    /// supported here.
    pub fn for_kind(kind: &str, project: Option<PathBuf>, relay: Option<String>) -> Result<Self> {
        let cwd = Some(agent_cwd(project.as_ref())?);
        let relay = relay.map(|url| RelayOptions { url, room_id: None });
        let (program, args, session_kind) = match kind {
            "shell" => {
                let (program, args) = default_shell_command();
                (program, args, SessionKind::shell())
            }
            "claude" => ("claude".to_string(), Vec::new(), SessionKind::claude()),
            "codex" => (
                "codex".to_string(),
                codex_default_args(),
                SessionKind::codex(),
            ),
            "opencode" => ("opencode".to_string(), Vec::new(), SessionKind::opencode()),
            "gemini" => ("gemini".to_string(), Vec::new(), SessionKind::gemini()),
            "aider" => ("aider".to_string(), Vec::new(), SessionKind::aider()),
            other => {
                bail!(
                    "unknown tool kind `{other}`; use shell, claude, codex, opencode, gemini or aider"
                )
            }
        };
        Ok(Self {
            program,
            args,
            cwd,
            relay,
            session_kind,
        })
    }

    /// Build a target for a user-defined custom tool (from `config.json`). The
    /// `name` becomes the session kind, with an explicit `program`/`args`.
    pub fn for_custom(
        name: &str,
        program: String,
        args: Vec<String>,
        project: Option<PathBuf>,
        relay: Option<String>,
    ) -> Result<Self> {
        let cwd = Some(agent_cwd(project.as_ref())?);
        let relay = relay.map(|url| RelayOptions { url, room_id: None });
        Ok(Self {
            program,
            args,
            cwd,
            relay,
            session_kind: SessionKind::new(name)?,
        })
    }

    pub fn validate(&self) -> Result<()> {
        if self.program.trim().is_empty() {
            bail!("target command cannot be empty");
        }

        Ok(())
    }
}

fn validate_session_kind(value: &str) -> Result<()> {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        bail!("session kind cannot be empty");
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        bail!("session kind must start with a lowercase letter or digit");
    }
    if value.len() > 32 {
        bail!("session kind must be at most 32 bytes");
    }
    if !chars.all(|ch| {
        ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_' || ch == '.'
    }) {
        bail!("session kind contains unsupported characters");
    }
    Ok(())
}

fn default_shell() -> String {
    if cfg!(windows) {
        // cmd.exe (not PowerShell): it wraps output at the console width, so with
        // the relay sizing the PTY to `min(app, host)` the desktop and phone see
        // exactly the same layout. PowerShell instead formats/wraps at its own
        // console width, which the phone-width model then re-wraps inaccurately
        // (wrapped text collides with the preceding prompt path).
        "cmd.exe".to_string()
    } else if cfg!(target_os = "macos") {
        "zsh".to_string()
    } else {
        "sh".to_string()
    }
}

fn default_shell_from_env() -> Option<String> {
    if cfg!(windows) {
        None
    } else {
        default_shell_from_env_value(std::env::var("SHELL").ok().as_deref())
    }
}

fn default_shell_from_env_value(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn default_shell_command() -> (String, Vec<String>) {
    let program = default_shell_from_env().unwrap_or_else(default_shell);
    // cmd.exe launched inside the PTY is interactive and stays open with no
    // arguments, so both platforms use an empty argument list.
    let args = Vec::new();
    (program, args)
}

fn codex_default_args() -> Vec<String> {
    vec!["--no-alt-screen".to_string()]
}

fn agent_cwd(project: Option<&PathBuf>) -> Result<PathBuf> {
    match project {
        Some(path) => Ok(path.clone()),
        None => std::env::current_dir().map_err(Into::into),
    }
}

fn relay_options(relay: Option<&String>) -> Option<RelayOptions> {
    relay.map(|url| RelayOptions {
        url: url.clone(),
        room_id: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    #[cfg(windows)]
    fn windows_default_shell_is_cmd() {
        assert_eq!(default_shell(), "cmd.exe");
    }

    #[test]
    #[cfg(windows)]
    fn windows_default_shell_command_has_no_extra_args() {
        let (_program, args) = default_shell_command();
        assert!(args.is_empty());
    }

    #[test]
    #[cfg(not(windows))]
    fn unix_default_shell_is_not_cmd() {
        assert_ne!(default_shell(), "cmd.exe");
    }

    #[test]
    #[cfg(not(windows))]
    fn unix_default_shell_command_has_no_extra_args() {
        let (_program, args) = default_shell_command();
        assert!(args.is_empty());
    }

    #[test]
    #[cfg(not(windows))]
    fn empty_shell_env_is_ignored() {
        assert_eq!(default_shell_from_env_value(Some("")), None);
        assert_eq!(default_shell_from_env_value(Some("   ")), None);
        assert_eq!(
            default_shell_from_env_value(Some("/bin/fish")),
            Some("/bin/fish".to_string())
        );
    }

    #[test]
    fn for_custom_uses_program_args_and_name_as_kind() {
        let target = TargetCommand::for_custom(
            "gemini",
            "gemini".to_string(),
            vec!["--model".to_string(), "flash".to_string()],
            Some(PathBuf::from("/work/alpha")),
            Some("ws://127.0.0.1:8787".to_string()),
        )
        .unwrap();
        assert_eq!(target.program, "gemini");
        assert_eq!(target.args, ["--model", "flash"]);
        assert_eq!(target.session_kind.as_str(), "gemini");
        assert_eq!(target.cwd.as_deref(), Some(Path::new("/work/alpha")));
        assert_eq!(target.relay.unwrap().url, "ws://127.0.0.1:8787");
    }

    #[test]
    fn builtin_codex_uses_inline_mode() {
        let target = TargetCommand::for_kind("codex", None, None).unwrap();
        assert_eq!(target.program, "codex");
        assert_eq!(target.args, ["--no-alt-screen"]);
        assert_eq!(target.session_kind, SessionKind::codex());
    }

    #[test]
    fn full_screen_tui_kinds_use_managed_alt_screen() {
        // Full-screen alternate-screen TUIs are rendered in the app as managed
        // primary-screen scrollback (alt-screen stripped from the app stream,
        // primary-screen frames recorded). Inline agents must not be.
        assert!(SessionKind::codex().uses_managed_alt_screen());
        assert!(SessionKind::opencode().uses_managed_alt_screen());
        assert!(!SessionKind::claude().uses_managed_alt_screen());
        assert!(!SessionKind::shell().uses_managed_alt_screen());
        assert!(
            !SessionKind::new("gemini")
                .unwrap()
                .uses_managed_alt_screen()
        );
    }

    #[test]
    fn for_custom_rejects_invalid_kind() {
        let result = TargetCommand::for_custom("Bad Name", "x".to_string(), Vec::new(), None, None);
        assert!(result.is_err());
    }
}
