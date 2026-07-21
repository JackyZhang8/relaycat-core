//! Embedding entry points for running a relay session from this crate's source
//! inside another binary (the desktop GUI).
//!
//! The GUI links this crate and re-executes *itself* with [`RELAY_CHILD_ARG`] as
//! the first argument to host a relay session. That runs the exact same secure
//! pairing + mirror code path as the `relaycat` binary, directly from source,
//! so the GUI never needs to locate or spawn a separately compiled `relaycat`
//! executable.

use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::FromArgMatches;
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};

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
/// Scopes the per-session side channels so multiple relay tabs on the same
/// project + tool kind never read each other's pairing URL or state.
pub const GUI_SESSION_ID_ENV: &str = "RELAYCAT_GUI_SESSION_ID";
/// Loopback TCP address of the GUI-owned relay state listener.
pub const GUI_RELAY_STATE_ADDR_ENV: &str = "RELAYCAT_GUI_STATE_ADDR";
/// Per-session bearer token required on every relay state message.
pub const GUI_RELAY_STATE_TOKEN_ENV: &str = "RELAYCAT_GUI_STATE_TOKEN";

/// Relay lifecycle states reported by a GUI-hosted relay child over its
/// dedicated control channel. Terminal bytes continue to use the PTY/pipe.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GuiRelayState {
    Wait,
    Syncing,
    Paired,
    Error,
}

impl GuiRelayState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wait => "wait",
            Self::Syncing => "syncing",
            Self::Paired => "paired",
            Self::Error => "error",
        }
    }
}

/// Latest relay state for one GUI tab. Revisions are monotonically increasing
/// within the relay child so the GUI can discard delayed/replayed messages.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct GuiRelayStateSnapshot {
    pub revision: u64,
    pub state: GuiRelayState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl GuiRelayStateSnapshot {
    pub fn wait(revision: u64) -> Self {
        Self::simple(revision, GuiRelayState::Wait)
    }

    pub fn syncing(revision: u64) -> Self {
        Self::simple(revision, GuiRelayState::Syncing)
    }

    pub fn paired(revision: u64) -> Self {
        Self::simple(revision, GuiRelayState::Paired)
    }

    pub fn error(revision: u64, code: String, retryable: bool, message: String) -> Self {
        Self {
            revision,
            state: GuiRelayState::Error,
            code: Some(code),
            retryable: Some(retryable),
            message: Some(message),
        }
    }

    fn simple(revision: u64, state: GuiRelayState) -> Self {
        Self {
            revision,
            state,
            code: None,
            retryable: None,
            message: None,
        }
    }
}

/// Authentication wrapper for one newline-delimited state snapshot.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct GuiRelayStateEnvelope {
    pub token: String,
    pub snapshot: GuiRelayStateSnapshot,
}

pub fn encode_gui_relay_state_line(
    token: &str,
    snapshot: &GuiRelayStateSnapshot,
) -> serde_json::Result<String> {
    let mut line = serde_json::to_string(&GuiRelayStateEnvelope {
        token: token.to_string(),
        snapshot: snapshot.clone(),
    })?;
    line.push('\n');
    Ok(line)
}

pub fn decode_gui_relay_state_line(line: &str) -> serde_json::Result<GuiRelayStateEnvelope> {
    serde_json::from_str(line.trim_end())
}

static GUI_RELAY_STATE_SENDER: OnceLock<Sender<GuiRelayStateSnapshot>> = OnceLock::new();
static GUI_RELAY_STATE_REVISION: AtomicU64 = AtomicU64::new(0);

/// Generate a per-session token for authenticating the loopback state stream.
pub fn new_gui_relay_state_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    let mut token = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(token, "{byte:02x}");
    }
    token
}

/// Start the relay child's state writer when the GUI supplied an endpoint.
/// Publishing remains a no-op for the standalone CLI.
pub fn init_gui_relay_state_reporter() {
    if GUI_RELAY_STATE_SENDER.get().is_some() {
        return;
    }
    let Some(address) = std::env::var(GUI_RELAY_STATE_ADDR_ENV)
        .ok()
        .and_then(|value| value.parse::<SocketAddr>().ok())
    else {
        return;
    };
    let Ok(token) = std::env::var(GUI_RELAY_STATE_TOKEN_ENV) else {
        return;
    };
    if token.is_empty() {
        return;
    }

    let (sender, receiver) = channel();
    if GUI_RELAY_STATE_SENDER.set(sender).is_err() {
        return;
    }
    std::thread::spawn(move || gui_relay_state_writer_loop(address, token, receiver));
}

pub fn publish_gui_relay_state(state: GuiRelayState) {
    init_gui_relay_state_reporter();
    let Some(sender) = GUI_RELAY_STATE_SENDER.get() else {
        return;
    };
    let revision = GUI_RELAY_STATE_REVISION.fetch_add(1, Ordering::Relaxed) + 1;
    let snapshot = match state {
        GuiRelayState::Wait => GuiRelayStateSnapshot::wait(revision),
        GuiRelayState::Syncing => GuiRelayStateSnapshot::syncing(revision),
        GuiRelayState::Paired => GuiRelayStateSnapshot::paired(revision),
        GuiRelayState::Error => return,
    };
    let _ = sender.send(snapshot);
}

pub fn publish_gui_relay_error(code: String, retryable: bool, message: String) {
    init_gui_relay_state_reporter();
    let Some(sender) = GUI_RELAY_STATE_SENDER.get() else {
        return;
    };
    let revision = GUI_RELAY_STATE_REVISION.fetch_add(1, Ordering::Relaxed) + 1;
    let code = if code.is_empty() {
        "unknown".to_string()
    } else {
        code
    };
    let _ = sender.send(GuiRelayStateSnapshot::error(
        revision, code, retryable, message,
    ));
}

fn gui_relay_state_writer_loop(
    address: SocketAddr,
    token: String,
    receiver: Receiver<GuiRelayStateSnapshot>,
) {
    let mut stream = None;
    while let Ok(mut snapshot) = receiver.recv() {
        while let Ok(newer) = receiver.try_recv() {
            snapshot = newer;
        }
        loop {
            if stream.is_none() {
                match TcpStream::connect_timeout(&address, Duration::from_millis(500)) {
                    Ok(connected) => stream = Some(connected),
                    Err(_) => {
                        while let Ok(newer) = receiver.try_recv() {
                            snapshot = newer;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                        continue;
                    }
                }
            }
            let Ok(line) = encode_gui_relay_state_line(&token, &snapshot) else {
                break;
            };
            let result = stream
                .as_mut()
                .expect("relay state stream initialized")
                .write_all(line.as_bytes());
            if result.is_ok() {
                break;
            }
            stream = None;
            while let Ok(newer) = receiver.try_recv() {
                snapshot = newer;
            }
        }
    }
}

#[cfg(test)]
fn connect_and_write_gui_relay_state(
    address: SocketAddr,
    token: &str,
    snapshot: &GuiRelayStateSnapshot,
) -> Result<()> {
    let mut stream = TcpStream::connect(address).context("failed to connect GUI state channel")?;
    let line =
        encode_gui_relay_state_line(token, snapshot).context("failed to encode GUI relay state")?;
    stream
        .write_all(line.as_bytes())
        .context("failed to write GUI relay state")
}

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
    init_gui_relay_state_reporter();
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
    use std::io::BufRead;

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

    #[test]
    fn relay_state_envelope_round_trips_as_one_json_line() {
        let snapshot = GuiRelayStateSnapshot::paired(7);

        let line = encode_gui_relay_state_line("secret-token", &snapshot).unwrap();
        let decoded = decode_gui_relay_state_line(&line).unwrap();

        assert!(line.ends_with('\n'));
        assert_eq!(decoded.token, "secret-token");
        assert_eq!(decoded.snapshot, snapshot);
    }

    #[test]
    fn relay_error_snapshot_keeps_structured_details() {
        let snapshot = GuiRelayStateSnapshot::error(
            9,
            "rate_limited".to_string(),
            true,
            "relay is busy".to_string(),
        );

        assert_eq!(snapshot.state, GuiRelayState::Error);
        assert_eq!(snapshot.code.as_deref(), Some("rate_limited"));
        assert_eq!(snapshot.retryable, Some(true));
        assert_eq!(snapshot.message.as_deref(), Some("relay is busy"));
    }

    #[test]
    fn relay_state_writer_sends_an_authenticated_snapshot() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let snapshot = GuiRelayStateSnapshot::syncing(3);
        let expected = snapshot.clone();

        let writer = std::thread::spawn(move || {
            connect_and_write_gui_relay_state(address, "secret-token", &snapshot).unwrap();
        });
        let (stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        std::io::BufReader::new(stream)
            .read_line(&mut line)
            .unwrap();
        writer.join().unwrap();

        let envelope = decode_gui_relay_state_line(&line).unwrap();
        assert_eq!(envelope.token, "secret-token");
        assert_eq!(envelope.snapshot, expected);
    }
}
