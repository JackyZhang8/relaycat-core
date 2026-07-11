//! Embedding entry points for running a relay session from this crate's source
//! inside another binary (the desktop GUI).
//!
//! The GUI links this crate and re-executes *itself* with [`RELAY_CHILD_ARG`] as
//! the first argument to host a relay session. That runs the exact same secure
//! pairing + mirror code path as the `relaycat` binary, directly from source,
//! so the GUI never needs to locate or spawn a separately compiled `relaycat`
//! executable.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Context, Result};
use clap::FromArgMatches;

use crate::args::Cli;
use crate::command::TargetCommand;
use crate::i18n::CliLanguage;
use crate::relay;

/// First argument the GUI passes when it re-executes itself to host a relay
/// session. The remaining arguments are ordinary `relaycat` CLI arguments
/// (e.g. `shell --project <dir> --relay <url>`).
pub const RELAY_CHILD_ARG: &str = "__relay-session";

/// Version of this `relaycat-cli` crate, for the GUI's diagnostics panel.
pub const RELAYCAT_CLI_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Set to `1` by the GUI host on Windows when it drives the relay child over
/// plain pipes instead of a pseudoconsole (ConPTY).
///
/// On Windows the GUI binary is built `windows_subsystem = "windows"`, so when
/// it re-executes itself as a relay child *inside a ConPTY* the OS never wires
/// the child's std handles to that pseudoconsole (only console-subsystem
/// children get that automatic wiring). The result is that nothing the relay
/// child prints — the terminal mirror the GUI renders — ever reaches the GUI,
/// leaving the embedded terminal blank. To avoid that, the Windows GUI host
/// spawns the relay child as an ordinary process with piped stdin/stdout (which
/// *are* honoured for GUI-subsystem children) and sets this variable so the
/// relay loop knows its stdio is the GUI bridge rather than a real terminal.
pub const GUI_BRIDGE_ENV: &str = "RELAYCAT_GUI_BRIDGE";

/// Initial terminal width/height (in columns/rows) the GUI host hands the relay
/// child over the pipe bridge, since a pipe — unlike a ConPTY — carries no size.
pub const GUI_BRIDGE_COLS_ENV: &str = "RELAYCAT_GUI_BRIDGE_COLS";
pub const GUI_BRIDGE_ROWS_ENV: &str = "RELAYCAT_GUI_BRIDGE_ROWS";

/// Path to a small file the GUI host rewrites (`"<cols> <rows>"`) whenever its
/// terminal widget reflows; the relay child polls it to resize the inner PTY,
/// because a pipe carries no `SIGWINCH`/resize signal.
pub const GUI_BRIDGE_RESIZE_FILE_ENV: &str = "RELAYCAT_GUI_BRIDGE_RESIZE_FILE";

/// Unique GUI tab/session identifier the GUI host assigns to each relay child.
/// Scopes the per-session side channels (pairing-URL fallback file, log-tail
/// pairing marker) so multiple relay tabs on the same project + tool kind never
/// read each other's pairing URL or paired status.
pub const GUI_SESSION_ID_ENV: &str = "RELAYCAT_GUI_SESSION_ID";

/// The GUI host's session id for this relay child, sanitised for use in file
/// names. `None` outside a GUI-hosted relay child.
pub fn gui_session_id() -> Option<String> {
    let raw = std::env::var(GUI_SESSION_ID_ENV).ok()?;
    let id = sanitize_session_id(&raw);
    if id.is_empty() { None } else { Some(id) }
}

/// Keep only filesystem-safe characters so the id can be embedded in file
/// names on every platform.
pub fn sanitize_session_id(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(64)
        .collect()
}

/// Whether this process is a relay child driven over the GUI pipe bridge (see
/// [`GUI_BRIDGE_ENV`]). Always `false` unless the GUI host opted in, so the CLI
/// and the Mac/Linux PTY-hosted relay child keep their existing stdio path.
pub fn gui_bridge_pipe_mode() -> bool {
    std::env::var(GUI_BRIDGE_ENV)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("pipe"))
        .unwrap_or(false)
}

/// The initial `(cols, rows)` the GUI host requested for the bridged terminal.
pub fn gui_bridge_initial_size() -> Option<(u16, u16)> {
    let cols = parse_env_u16(GUI_BRIDGE_COLS_ENV)?;
    let rows = parse_env_u16(GUI_BRIDGE_ROWS_ENV)?;
    if cols == 0 || rows == 0 {
        return None;
    }
    Some((cols, rows))
}

/// Path of the GUI host's resize-notification file, when running over the pipe
/// bridge.
pub fn gui_bridge_resize_file() -> Option<PathBuf> {
    let path = std::env::var_os(GUI_BRIDGE_RESIZE_FILE_ENV)?;
    if path.is_empty() {
        return None;
    }
    Some(PathBuf::from(path))
}

fn parse_env_u16(name: &str) -> Option<u16> {
    std::env::var(name).ok()?.trim().parse().ok()
}

/// Live `(cols, rows)` of the GUI host's terminal widget, packed as
/// `(cols << 16) | rows`; `0` means "unset" (this process is not a GUI
/// pipe-bridge relay child).
///
/// A pipe — unlike a real tty or a ConPTY — exposes no terminal size, so on the
/// Windows pipe bridge the relay loop cannot read its "host" terminal size from
/// the OS the way it does on macOS/Linux. The GUI host instead seeds this with
/// the initial size and republishes it on every reflow, letting
/// `current_terminal_size()` report the live GUI desktop dimensions so the
/// app-size clamp and the Ctrl-G local resize behave exactly as they do when the
/// CLI owns a real terminal.
static BRIDGE_TERMINAL_SIZE: AtomicU32 = AtomicU32::new(0);

/// Record the GUI host terminal's current `(cols, rows)`.
pub fn set_bridge_terminal_size(cols: u16, rows: u16) {
    let packed = (u32::from(cols) << 16) | u32::from(rows);
    BRIDGE_TERMINAL_SIZE.store(packed, Ordering::Release);
}

/// The GUI host terminal's last known `(cols, rows)`, or `None` when this
/// process is not a GUI pipe-bridge relay child (or no size has been published).
pub fn bridge_terminal_size() -> Option<(u16, u16)> {
    let packed = BRIDGE_TERMINAL_SIZE.load(Ordering::Acquire);
    let cols = (packed >> 16) as u16;
    let rows = (packed & 0xFFFF) as u16;
    if cols == 0 || rows == 0 {
        return None;
    }
    Some((cols, rows))
}

/// Parse a `"<cols> <rows>"` resize line written by the GUI host into
/// `(cols, rows)`.
pub fn parse_resize_line(contents: &str) -> Option<(u16, u16)> {
    let mut parts = contents.split_whitespace();
    let cols: u16 = parts.next()?.parse().ok()?;
    let rows: u16 = parts.next()?.parse().ok()?;
    if cols == 0 || rows == 0 {
        return None;
    }
    Some((cols, rows))
}

/// Run a secure-pairing relay session synchronously from CLI-style arguments.
///
/// This is the in-process equivalent of invoking the `relaycat` binary with the
/// same arguments: it installs the TLS provider, builds a Tokio runtime, and
/// drives [`relay::run_secure_pairing`]. It is meant to be called from a
/// re-executed GUI process that already owns a PTY (so the relay loop has a
/// real terminal to drive, exactly as the standalone CLI does).
pub fn run_relay_child(args: &[String]) -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    // The GUI renders the pairing QR / URL / hints in a native popup, so keep
    // the embedded terminal clean by suppressing the human-facing pairing block
    // (the URL is still handed to the GUI host via a private OSC sequence).
    relay::set_gui_pairing_quiet();
    let language = CliLanguage::from_system_locale();
    let cli = parse_cli(args, language)?;
    let target = TargetCommand::from_cli(&cli)?;
    let relay = target
        .relay
        .clone()
        .context("relay session requires --relay <url>")?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build Tokio runtime for relay session")?;
    runtime.block_on(relay::run_secure_pairing(target, relay))
}

fn parse_cli(args: &[String], language: CliLanguage) -> Result<Cli> {
    let argv = std::iter::once("relaycat".to_string()).chain(args.iter().cloned());
    let matches = Cli::command_for_language(language)
        .try_get_matches_from(argv)
        .context("failed to parse relay arguments")?;
    Cli::from_arg_matches(&matches).context("failed to build relay command")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_resize_line_reads_cols_then_rows() {
        assert_eq!(parse_resize_line("120 40"), Some((120, 40)));
        assert_eq!(parse_resize_line("  80   24  \n"), Some((80, 24)));
    }

    #[test]
    fn parse_resize_line_rejects_garbage_and_zero() {
        assert_eq!(parse_resize_line(""), None);
        assert_eq!(parse_resize_line("80"), None);
        assert_eq!(parse_resize_line("0 24"), None);
        assert_eq!(parse_resize_line("80 0"), None);
        assert_eq!(parse_resize_line("wide tall"), None);
    }

    #[test]
    fn bridge_terminal_size_round_trips_cols_and_rows() {
        // Distinct, non-symmetric values catch a cols/rows packing swap.
        set_bridge_terminal_size(200, 51);
        assert_eq!(bridge_terminal_size(), Some((200, 51)));
        set_bridge_terminal_size(80, 24);
        assert_eq!(bridge_terminal_size(), Some((80, 24)));
        // A zero component is treated as "unset" rather than a real size.
        set_bridge_terminal_size(0, 24);
        assert_eq!(bridge_terminal_size(), None);
        set_bridge_terminal_size(80, 0);
        assert_eq!(bridge_terminal_size(), None);
    }
}
