//! Interactive TUI launcher (`relaycat tui`, and the default when `relaycat` is
//! run with no subcommand).
//!
//! Flow:
//! 1. Pick a tool: the built-in shell/codex/claude/opencode/gemini/aider
//!    entries plus any custom tools from `config.json`. The cursor starts on
//!    the configured
//!    default tool.
//! 2. Pick a project: favorite projects (★) and the current directory, then
//!    recently used projects, then a custom path typed in. Press `f` to
//!    toggle the highlighted project as a favorite (persisted to `config.json`).
//! 3. Enter the relay WebSocket URL. It is pre-filled with the relay last used
//!    for the chosen project, falling back to the configured default relay,
//!    then the most recent relay overall.
//! 4. Fully tear down the `ratatui`/`crossterm` UI, restoring the real
//!    terminal. This terminal hand-off is the highest-risk part of the launcher
//!    and is what lets the existing PTY/relay code take over cleanly.
//! 5. With a relay URL: print the pairing URL + QR and wait *in place* for the
//!    mobile app to join. Esc returns to relay editing, Ctrl-C returns to the
//!    tool picker, and a successful scan hands off to the secure PTY relay. With
//!    an empty relay URL: just run the tool locally in a PTY (no pairing).
//!
//! When `config.return_to_launcher` is set (the default), the launcher
//! reappears after each session ends instead of the process exiting.

use std::io::{self, Stdout};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::{
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, Borders, List, ListItem, ListState, Padding, Paragraph, Wrap},
};

use crate::config::{self, Config};
use crate::i18n::CliLanguage;
use crate::recent_store::RecentRecord;
use crate::update::{self, UpdateNotice};
use crate::{command::TargetCommand, pty, recent_store, relay};

mod tools;
mod config_load;
mod session;
mod select;
mod render;

pub(crate) use tools::*;
pub(crate) use config_load::*;
pub(crate) use session::*;
pub(crate) use select::*;
pub(crate) use render::*;

/// What a tool entry launches: either a built-in session kind or a user-defined
/// custom tool with an explicit program/args.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ToolKind {
    Builtin(&'static str),
    Custom {
        name: String,
        program: String,
        args: Vec<String>,
    },
}

/// One selectable tool entry in the picker.
#[derive(Debug, Clone)]
pub(crate) struct ToolChoice {
    label: String,
    tool: ToolKind,
}

const BUILTIN_TOOLS: &[(&str, &str)] = &[
    ("Shell", "shell"),
    ("Codex", "codex"),
    ("Claude Code", "claude"),
    ("OpenCode", "opencode"),
    ("Gemini CLI", "gemini"),
    ("Aider", "aider"),
];

const UPDATE_NOTICE_POLL_INTERVAL: Duration = Duration::from_millis(250);





/// RAII guard that owns the alternate-screen/raw-mode terminal and restores the
/// real terminal on drop, including on panic or early return. Restoring on
/// `Drop` (rather than only on the happy path) is what guarantees a clean
/// hand-off to the PTY code afterwards.
pub(crate) struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("failed to enable raw mode")?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)
            .context("failed to enter alternate screen")?;
        let terminal =
            Terminal::new(CrosstermBackend::new(stdout)).context("failed to create terminal")?;
        Ok(Self { terminal })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Best-effort restore; nothing useful to do if these fail while unwinding.
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), DisableBracketedPaste, LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

/// A confirmed launcher selection: which tool, which project (or `None` for the
/// current directory), and the relay URL (or `None` for a local-only session).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Launch {
    tool: ToolKind,
    project: Option<PathBuf>,
    relay: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LauncherStart {
    Tool,
    Relay(Launch),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionOutcome {
    Completed,
    BackToRelay(Launch),
    BackToTools,
}

/// Which step of the launcher wizard is on screen.
pub(crate) enum Step {
    Tool,
    Project,
    ProjectCustom,
    Relay,
}

/// One row in the project picker.
pub(crate) struct ProjectChoice {
    label: String,
    kind: ProjectChoiceKind,
}

pub(crate) enum ProjectChoiceKind {
    /// Run in the process's current working directory.
    CurrentDir,
    /// A concrete project path (a favorite or a recent project).
    Path(PathBuf),
    /// Prompt for a path typed in by the user.
    Custom,
}

impl ProjectChoiceKind {
    /// The concrete path a favorite toggle would apply to, if any.
    fn favorite_path(&self) -> Option<PathBuf> {
        match self {
            ProjectChoiceKind::CurrentDir => std::env::current_dir().ok(),
            ProjectChoiceKind::Path(path) => Some(path.clone()),
            ProjectChoiceKind::Custom => None,
        }
    }
}

/// Run the launcher loop: pick a tool/project/relay, run the session, and
/// (unless disabled in the config) return to the launcher afterwards.
pub async fn run() -> Result<()> {
    let language = CliLanguage::from_system_locale();
    let config_path = config::config_file_path().ok();
    let mut config = load_config(config_path.as_deref(), language);
    let mut launcher_start = LauncherStart::Tool;
    let update_notice_rx = spawn_update_notice_check();
    let mut update_notice = None;

    loop {
        // `select_launch` owns the TUI terminal and drops it (restoring the real
        // terminal) before returning, so the relay/PTY hand-off below starts
        // from a clean terminal state.
        let Some(launch) = select_launch(
            &mut config,
            config_path.as_deref(),
            language,
            launcher_start.clone(),
            &update_notice_rx,
            &mut update_notice,
        )?
        else {
            return Ok(());
        };

        match run_session(launch, language).await? {
            SessionOutcome::Completed => {
                if !config.return_to_launcher {
                    return Ok(());
                }
                launcher_start = LauncherStart::Tool;
            }
            SessionOutcome::BackToRelay(launch) => {
                launcher_start = LauncherStart::Relay(launch);
            }
            SessionOutcome::BackToTools => {
                launcher_start = LauncherStart::Tool;
            }
        }
    }
}
















#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LauncherMainLayout {
    content: Rect,
    brand: Option<Rect>,
}















#[cfg(test)]
mod tests;
