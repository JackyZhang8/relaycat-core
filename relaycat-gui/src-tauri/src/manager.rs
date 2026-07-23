//! Per-tab PTY session bookkeeping for the GUI.
//!
//! Each tab in the UI maps to one [`relaycat_cli::session::PtySession`]. A
//! dedicated reader thread streams the child's output to the frontend as
//! `session://output` events and reports exit via `session://status`. The
//! manager keeps the writer (to forward keystrokes) and the PTY handle (to
//! resize / kill) addressable by session id.
//!
//! Relay sessions run the `relaycat` CLI inside the PTY (see [`crate::relay`]).
//! For those the reader also scrapes the pairing URL the CLI prints
//! (`session://pairing`) and a dedicated loopback state channel reports when
//! the mobile app has paired (`session://relay`).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use relaycat_cli::gui_bridge::{
    GuiRelayStateEnvelope, GuiRelayStateSnapshot, decode_gui_relay_state_line,
};
use relaycat_cli::session::PtySession;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

/// Control surface for a live session's child, abstracting over the two ways
/// the GUI hosts a child:
///
/// * a pseudoconsole/PTY ([`PtySession`]) — used for local sessions on every
///   platform and for relay sessions on macOS/Linux, and
/// * plain pipes around an ordinary child process — used for relay sessions on
///   Windows, where the GUI-subsystem relay child's stdio cannot be wired to a
///   ConPTY (see [`crate::manager::PipeControl`]).
trait SessionControl: Send + Sync {
    /// Resize the terminal to `rows` x `cols`.
    fn resize(&self, rows: u16, cols: u16) -> Result<()>;
    /// Kill the child (and its descendants, best-effort).
    fn kill(&self) -> Result<()>;
    /// The child's exit code if it has already exited.
    fn exit_code(&self) -> Option<i32>;
}

/// PTY-backed control: a [`PtySession`] shared with the reader thread.
struct PtyControl {
    pty: Arc<Mutex<PtySession>>,
}

impl SessionControl for PtyControl {
    fn resize(&self, rows: u16, cols: u16) -> Result<()> {
        self.pty.lock().unwrap().resize(rows, cols)
    }

    fn kill(&self) -> Result<()> {
        self.pty.lock().unwrap().kill()
    }

    fn exit_code(&self) -> Option<i32> {
        self.pty
            .lock()
            .ok()
            .and_then(|mut guard| guard.try_wait().ok().flatten())
            .map(|status| status.exit_code() as i32)
    }
}

/// Pipe-backed control for the Windows relay child: terminal bytes flow over
/// the child's stdin/stdout pipes, and resize is delivered out-of-band through a
/// file the relay child polls (a pipe carries no resize signal).
#[cfg(windows)]
struct PipeControl {
    child: Mutex<std::process::Child>,
    resize_file: PathBuf,
}

#[cfg(windows)]
impl SessionControl for PipeControl {
    fn resize(&self, rows: u16, cols: u16) -> Result<()> {
        write_resize_file(&self.resize_file, rows, cols)
    }

    fn kill(&self) -> Result<()> {
        // Terminating the relay child closes its job-object handle, so the inner
        // shell it spawned is taken down with it (see the GUI relay-child
        // watchdog in `main.rs`).
        let mut child = self.child.lock().unwrap();
        match child.kill() {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::InvalidInput => Ok(()), // already exited
            Err(e) => Err(e).context("failed to kill relay child"),
        }
    }

    fn exit_code(&self) -> Option<i32> {
        self.child
            .lock()
            .ok()
            .and_then(|mut guard| guard.try_wait().ok().flatten())
            .and_then(|status| status.code())
    }
}

/// Write the GUI host's `"<cols> <rows>"` resize line atomically (temp + rename)
/// so the relay child's poller never reads a half-written value.
#[cfg(windows)]
fn write_resize_file(path: &std::path::Path, rows: u16, cols: u16) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, format!("{cols} {rows}")).context("failed to write resize file")?;
    std::fs::rename(&tmp, path).context("failed to publish resize file")
}

/// A live GUI session: its writable input and a control handle for
/// resize/kill from command handlers.
struct LiveSession {
    writer: Mutex<Box<dyn Write + Send>>,
    control: Arc<dyn SessionControl>,
    /// Cleared on exit/close so relay helper threads can stop.
    alive: Arc<AtomicBool>,
    /// Latest authenticated state received from this relay child.
    relay_state: Option<Arc<Mutex<Option<GuiRelayStateSnapshot>>>>,
}

/// GUI-owned state channel bound before the relay child starts. Binding first
/// lets the OS queue an immediate child connection even if the accept thread
/// has not started running yet.
pub struct RelayStateBridge {
    listener: TcpListener,
    address: String,
    token: String,
}

impl RelayStateBridge {
    pub fn bind(token: String) -> Result<Self> {
        let listener =
            TcpListener::bind(("127.0.0.1", 0)).context("failed to bind relay state listener")?;
        let address = listener
            .local_addr()
            .context("failed to read relay state listener address")?
            .to_string();
        Ok(Self {
            listener,
            address,
            token,
        })
    }
}

/// Extra wiring for a relay session: the pairing-URL fallback, resize channel,
/// and authenticated state listener owned by this GUI tab.
pub struct RelayContext {
    pub pairing_url_path: PathBuf,
    #[cfg_attr(not(windows), allow(dead_code))]
    pub resize_file: PathBuf,
    /// Per-tab id handed to the relay child (see
    /// [`relaycat_cli::gui_bridge::GUI_SESSION_ID_ENV`]); scopes the pairing
    /// file name and child-side diagnostics to this tab.
    pub gui_session_id: String,
    pub state_bridge: RelayStateBridge,
}

#[derive(Default)]
pub struct SessionManager {
    sessions: Mutex<HashMap<String, LiveSession>>,
}

#[derive(Clone, Serialize)]
struct OutputEvent {
    id: String,
    /// Raw PTY bytes; the frontend feeds them straight into xterm.js.
    data: Vec<u8>,
}

#[derive(Clone, Serialize)]
struct StatusEvent {
    id: String,
    /// `running` | `exited`
    state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<i32>,
}

#[derive(Clone, Serialize)]
struct PairingEvent {
    id: String,
    url: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct RelayEvent {
    id: String,
    revision: u64,
    /// `paired` | `syncing` | `wait` | `error`
    state: String,
    /// For `error`: the relay's stable error code (or `unknown`).
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<String>,
    /// For `error`: whether the failure is retryable per the stable code.
    #[serde(skip_serializing_if = "Option::is_none")]
    retryable: Option<bool>,
    /// For `error`: the relay's free-text reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

impl RelayEvent {
    pub(crate) fn from_snapshot(id: String, snapshot: &GuiRelayStateSnapshot) -> Self {
        Self {
            id,
            revision: snapshot.revision,
            state: snapshot.state.as_str().to_string(),
            code: snapshot.code.clone(),
            retryable: snapshot.retryable,
            message: snapshot.message.clone(),
        }
    }
}

impl SessionManager {
    /// Spawn a local session: the target tool runs directly in the PTY.
    pub fn spawn(
        &self,
        app: AppHandle,
        id: String,
        target: &relaycat_cli::command::TargetCommand,
        rows: u16,
        cols: u16,
    ) -> Result<()> {
        self.spawn_inner(app, id, target, rows, cols, None)
    }

    /// Spawn a relay session: `target` runs the `relaycat` CLI, which performs
    /// the secure pairing + mirror. The reader scrapes the pairing URL and a
    /// dedicated control channel reports the current relay state.
    ///
    /// On macOS/Linux the relay child is hosted in a PTY just like a local
    /// session. On Windows it is hosted over plain pipes instead (see
    /// [`SessionManager::spawn_relay_pipe`]), because the GUI-subsystem relay
    /// child's stdio cannot be wired to a ConPTY and the embedded terminal would
    /// otherwise stay blank.
    pub fn spawn_relay(
        &self,
        app: AppHandle,
        id: String,
        target: &relaycat_cli::command::TargetCommand,
        rows: u16,
        cols: u16,
        relay: RelayContext,
    ) -> Result<()> {
        #[cfg(windows)]
        let result = self.spawn_relay_pipe(app, id, target, rows, cols, relay);
        #[cfg(not(windows))]
        let result = self.spawn_inner(app, id, target, rows, cols, Some(relay));
        result
    }

    fn spawn_inner(
        &self,
        app: AppHandle,
        id: String,
        target: &relaycat_cli::command::TargetCommand,
        rows: u16,
        cols: u16,
        relay: Option<RelayContext>,
    ) -> Result<()> {
        // Hand relay children their per-tab id and authenticated state channel
        // so all GUI side channels remain scoped to this tab.
        let envs: Vec<(&str, String)> = match &relay {
            Some(context) => vec![
                (
                    relaycat_cli::gui_bridge::GUI_SESSION_ID_ENV,
                    context.gui_session_id.clone(),
                ),
                (
                    relaycat_cli::gui_bridge::GUI_RELAY_STATE_ADDR_ENV,
                    context.state_bridge.address.clone(),
                ),
                (
                    relaycat_cli::gui_bridge::GUI_RELAY_STATE_TOKEN_ENV,
                    context.state_bridge.token.clone(),
                ),
            ],
            None => Vec::new(),
        };
        let session = PtySession::spawn_with_envs(target, rows, cols, &envs)?;
        let writer = session.writer()?;
        let reader = session.reader()?;
        let control: Arc<dyn SessionControl> = Arc::new(PtyControl {
            pty: Arc::new(Mutex::new(session)),
        });
        self.wire_session(app, id, reader, writer, control, relay);
        Ok(())
    }

    /// Windows relay path: spawn the re-executed GUI binary as an ordinary child
    /// process with piped stdin/stdout (its GUI subsystem means a ConPTY would
    /// not wire its stdio). Terminal output flows over stdout, keystrokes over
    /// stdin, and resize over the poll file in `relay.resize_file`. The relay
    /// child is also handed this process's PID so it can self-terminate when the
    /// GUI exits (see `main.rs`).
    #[cfg(windows)]
    fn spawn_relay_pipe(
        &self,
        app: AppHandle,
        id: String,
        target: &relaycat_cli::command::TargetCommand,
        rows: u16,
        cols: u16,
        relay: RelayContext,
    ) -> Result<()> {
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};

        // CREATE_NO_WINDOW: never flash a console for the relay child.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;

        let mut command = Command::new(&target.program);
        command.args(&target.args);
        if let Some(cwd) = &target.cwd {
            command.current_dir(cwd);
        }
        command
            .env(relaycat_cli::gui_bridge::GUI_BRIDGE_ENV, "1")
            .env(
                relaycat_cli::gui_bridge::GUI_BRIDGE_COLS_ENV,
                cols.max(1).to_string(),
            )
            .env(
                relaycat_cli::gui_bridge::GUI_BRIDGE_ROWS_ENV,
                rows.max(1).to_string(),
            )
            .env(
                relaycat_cli::gui_bridge::GUI_BRIDGE_RESIZE_FILE_ENV,
                &relay.resize_file,
            )
            .env(
                relaycat_cli::gui_bridge::GUI_SESSION_ID_ENV,
                &relay.gui_session_id,
            )
            .env(
                relaycat_cli::gui_bridge::GUI_RELAY_STATE_ADDR_ENV,
                &relay.state_bridge.address,
            )
            .env(
                relaycat_cli::gui_bridge::GUI_RELAY_STATE_TOKEN_ENV,
                &relay.state_bridge.token,
            )
            .env(crate::GUI_PARENT_PID_ENV, std::process::id().to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW);

        // Seed the resize file with the initial size so a window resize that
        // happens before the child first reads it still produces a change.
        let _ = write_resize_file(&relay.resize_file, rows, cols);

        let mut child = command
            .spawn()
            .with_context(|| format!("failed to spawn relay child {}", target.program))?;
        let writer: Box<dyn Write + Send> = Box::new(
            child
                .stdin
                .take()
                .ok_or_else(|| anyhow!("relay child stdin pipe missing"))?,
        );
        let reader: Box<dyn Read + Send> = Box::new(
            child
                .stdout
                .take()
                .ok_or_else(|| anyhow!("relay child stdout pipe missing"))?,
        );
        let control: Arc<dyn SessionControl> = Arc::new(PipeControl {
            child: Mutex::new(child),
            resize_file: relay.resize_file.clone(),
        });
        self.wire_session(app, id, reader, writer, control, Some(relay));
        Ok(())
    }

    /// Shared wiring for both hosting paths: stream the child's output to the
    /// frontend (scraping the pairing URL for relay sessions), report exit, and
    /// register the session so command handlers can write/resize/kill it.
    fn wire_session(
        &self,
        app: AppHandle,
        id: String,
        mut reader: Box<dyn Read + Send>,
        writer: Box<dyn Write + Send>,
        control: Arc<dyn SessionControl>,
        relay: Option<RelayContext>,
    ) {
        let alive = Arc::new(AtomicBool::new(true));
        let is_relay = relay.is_some();
        let relay_state = relay
            .as_ref()
            .map(|_| Arc::new(Mutex::new(Some(GuiRelayStateSnapshot::wait(0)))));
        let control_for_reader = control.clone();
        let alive_for_reader = alive.clone();
        let reader_app = app.clone();
        let reader_id = id.clone();
        thread::spawn(move || {
            let mut buffer = [0u8; 16 * 1024];
            let mut scanner = PairingUrlScanner::new(is_relay);
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        let chunk = &buffer[..n];
                        if let Some(url) = scanner.feed(chunk) {
                            let _ = reader_app.emit(
                                "session://pairing",
                                PairingEvent {
                                    id: reader_id.clone(),
                                    url,
                                },
                            );
                        }
                        let _ = reader_app.emit(
                            "session://output",
                            OutputEvent {
                                id: reader_id.clone(),
                                data: chunk.to_vec(),
                            },
                        );
                    }
                    Err(_) => break,
                }
            }

            alive_for_reader.store(false, Ordering::Release);
            let code = control_for_reader.exit_code();
            let _ = reader_app.emit(
                "session://status",
                StatusEvent {
                    id: reader_id.clone(),
                    state: "exited".to_string(),
                    code,
                },
            );
        });

        if let Some(relay) = relay {
            spawn_relay_state_listener(
                app.clone(),
                id.clone(),
                relay.state_bridge,
                relay_state
                    .as_ref()
                    .expect("relay state initialized")
                    .clone(),
                alive.clone(),
            );
            spawn_pairing_url_poll(app, id.clone(), relay.pairing_url_path, alive.clone());
        }

        self.sessions.lock().unwrap().insert(
            id,
            LiveSession {
                writer: Mutex::new(writer),
                control,
                alive,
                relay_state,
            },
        );
    }

    pub fn write(&self, id: &str, data: &[u8]) -> Result<()> {
        let sessions = self.sessions.lock().unwrap();
        let session = sessions
            .get(id)
            .ok_or_else(|| anyhow!("unknown session {id}"))?;
        let mut writer = session.writer.lock().unwrap();
        writer.write_all(data).context("failed to write to PTY")?;
        writer.flush().context("failed to flush PTY")
    }

    pub fn resize(&self, id: &str, rows: u16, cols: u16) -> Result<()> {
        let sessions = self.sessions.lock().unwrap();
        let session = sessions
            .get(id)
            .ok_or_else(|| anyhow!("unknown session {id}"))?;
        session.control.resize(rows, cols)
    }

    pub fn close(&self, id: &str) -> Result<()> {
        let session = self.sessions.lock().unwrap().remove(id);
        if let Some(session) = session {
            session.alive.store(false, Ordering::Release);
            let _ = session.control.kill();
        }
        Ok(())
    }

    pub fn relay_state(&self, id: &str) -> Option<GuiRelayStateSnapshot> {
        let state = self.sessions.lock().ok()?.get(id)?.relay_state.clone()?;
        state.lock().ok()?.clone()
    }
}

/// Receives authenticated state snapshots from the relay child, saves the
/// latest revision in memory, and only then emits the frontend event.
fn spawn_relay_state_listener(
    app: AppHandle,
    id: String,
    bridge: RelayStateBridge,
    current: Arc<Mutex<Option<GuiRelayStateSnapshot>>>,
    alive: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        if bridge.listener.set_nonblocking(true).is_err() {
            return;
        }
        while alive.load(Ordering::Acquire) {
            match bridge.listener.accept() {
                Ok((stream, _)) => receive_relay_state_connection(
                    &app,
                    &id,
                    stream,
                    &bridge.token,
                    &current,
                    &alive,
                ),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(50));
                }
                Err(_) => break,
            }
        }
    });
}

fn receive_relay_state_connection(
    app: &AppHandle,
    id: &str,
    stream: std::net::TcpStream,
    token: &str,
    current: &Arc<Mutex<Option<GuiRelayStateSnapshot>>>,
    alive: &Arc<AtomicBool>,
) {
    if configure_relay_state_stream(&stream).is_err() {
        return;
    }
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    while alive.load(Ordering::Acquire) {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                if line.len() > 64 * 1024 {
                    break;
                }
                let Ok(envelope) = decode_gui_relay_state_line(&line) else {
                    continue;
                };
                if envelope.token != token {
                    break;
                }
                let accepted = current
                    .lock()
                    .ok()
                    .and_then(|mut state| {
                        accept_relay_snapshot(&mut state, envelope, token)
                            .then(|| state.as_ref().cloned())
                    })
                    .flatten();
                if let Some(snapshot) = accepted {
                    let _ = app.emit(
                        "session://relay",
                        RelayEvent::from_snapshot(id.to_string(), &snapshot),
                    );
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => break,
        }
    }
}

fn configure_relay_state_stream(stream: &std::net::TcpStream) -> std::io::Result<()> {
    // The listener is nonblocking so the thread can periodically observe
    // `alive`. On macOS an accepted socket can retain that mode, which makes an
    // idle `read_line` return WouldBlock immediately and spin at full CPU.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_millis(250)))
}

fn accept_relay_snapshot(
    current: &mut Option<GuiRelayStateSnapshot>,
    envelope: GuiRelayStateEnvelope,
    token: &str,
) -> bool {
    if envelope.token != token
        || current
            .as_ref()
            .is_some_and(|snapshot| snapshot.revision >= envelope.snapshot.revision)
    {
        return false;
    }
    *current = Some(envelope.snapshot);
    true
}

/// Polls the relay child's pairing-URL fallback file and emits
/// `session://pairing` once it appears. The relay CLI also prints the URL in a
/// private OSC the PTY reader scrapes, but Windows' ConPTY drops unknown OSC
/// sequences, so this file is the reliable cross-platform channel. The stale
/// file (if any) is removed before the child starts, so any content here
/// belongs to the current session.
fn spawn_pairing_url_poll(app: AppHandle, id: String, path: PathBuf, alive: Arc<AtomicBool>) {
    thread::spawn(move || {
        let mut waited = 0u32;
        while alive.load(Ordering::Acquire) {
            if let Ok(contents) = std::fs::read_to_string(&path) {
                let url = contents.trim();
                if url.starts_with("relaycat://pair?") {
                    let _ = app.emit(
                        "session://pairing",
                        PairingEvent {
                            id: id.clone(),
                            url: url.to_string(),
                        },
                    );
                    return;
                }
            }
            thread::sleep(Duration::from_millis(200));
            waited += 1;
            if waited > 300 {
                return; // ~60s without a pairing URL: give up polling.
            }
        }
    });
}

/// Scans the early PTY output of a relay CLI for the `relaycat://pair?...`
/// pairing URL it prints, tolerating the URL being split across reads.
struct PairingUrlScanner {
    enabled: bool,
    buffer: Vec<u8>,
}

const SCANNER_LIMIT: usize = 64 * 1024;
const PAIRING_PREFIX: &[u8] = b"relaycat://pair?";

impl PairingUrlScanner {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            buffer: Vec::new(),
        }
    }

    /// Feed a new chunk; returns the pairing URL the first time it is found.
    fn feed(&mut self, chunk: &[u8]) -> Option<String> {
        if !self.enabled {
            return None;
        }
        self.buffer.extend_from_slice(chunk);
        if let Some(url) = extract_pairing_url(&self.buffer) {
            self.enabled = false;
            self.buffer = Vec::new();
            return Some(url);
        }
        if self.buffer.len() > SCANNER_LIMIT {
            // Keep a tail large enough to still match a split URL.
            let drop = self.buffer.len() - PAIRING_PREFIX.len();
            self.buffer.drain(..drop);
        }
        None
    }
}

fn extract_pairing_url(buffer: &[u8]) -> Option<String> {
    let start = buffer
        .windows(PAIRING_PREFIX.len())
        .position(|w| w == PAIRING_PREFIX)?;
    let mut end = start;
    while end < buffer.len() && !is_url_terminator(buffer[end]) {
        end += 1;
    }
    // Require a terminator so we do not emit a truncated URL still being printed.
    if end >= buffer.len() {
        return None;
    }
    std::str::from_utf8(&buffer[start..end])
        .ok()
        .map(str::to_string)
}

fn is_url_terminator(byte: u8) -> bool {
    byte.is_ascii_whitespace() || byte < 0x20 || byte == 0x7f
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::{TcpListener, TcpStream};
    use std::time::Instant;

    const URL: &str = "relaycat://pair?relay=wss%3A%2F%2Frelay.example&room=abc&kind=shell";

    #[test]
    fn relay_state_stream_is_restored_to_blocking_reads() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind test listener");
        let address = listener.local_addr().expect("read test listener address");
        let client = TcpStream::connect(address).expect("connect test client");
        let (mut server, _) = listener.accept().expect("accept test connection");

        server
            .set_nonblocking(true)
            .expect("make regression socket nonblocking");
        configure_relay_state_stream(&server).expect("configure relay state stream");

        let started = Instant::now();
        let error = server
            .read(&mut [0u8; 1])
            .expect_err("idle blocking stream should time out");

        assert!(matches!(
            error.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ));
        assert!(
            started.elapsed() >= Duration::from_millis(100),
            "relay state read returned immediately and would busy-loop"
        );
        drop(client);
    }

    #[test]
    fn scrapes_url_from_plain_terminal_output() {
        let mut scanner = PairingUrlScanner::new(true);
        let chunk = format!("RelayCat pairing\nPairing URL:\n  {URL}\n");
        assert_eq!(scanner.feed(chunk.as_bytes()).as_deref(), Some(URL));
    }

    #[test]
    fn scrapes_url_from_gui_osc_wrapper() {
        // The GUI relay child emits the URL inside a private OSC sequence so it
        // is not rendered in the embedded terminal; the BEL terminator must
        // bound the scraped URL exactly.
        let mut scanner = PairingUrlScanner::new(true);
        let osc = format!("\x1b]9779;{URL}\x07");
        assert_eq!(scanner.feed(osc.as_bytes()).as_deref(), Some(URL));
    }

    #[test]
    fn url_split_across_reads_is_reassembled() {
        let osc = format!("\x1b]9779;{URL}\x07");
        let (head, tail) = osc.as_bytes().split_at(20);
        let mut scanner = PairingUrlScanner::new(true);
        assert_eq!(scanner.feed(head), None);
        assert_eq!(scanner.feed(tail).as_deref(), Some(URL));
    }

    #[test]
    fn disabled_scanner_never_matches() {
        let mut scanner = PairingUrlScanner::new(false);
        let osc = format!("\x1b]9779;{URL}\x07");
        assert_eq!(scanner.feed(osc.as_bytes()), None);
    }

    #[test]
    fn relay_snapshot_accepts_a_newer_authenticated_revision() {
        let mut current = Some(relaycat_cli::gui_bridge::GuiRelayStateSnapshot::syncing(2));
        let envelope = relaycat_cli::gui_bridge::GuiRelayStateEnvelope {
            token: "secret-token".to_string(),
            snapshot: relaycat_cli::gui_bridge::GuiRelayStateSnapshot::paired(3),
        };

        assert!(accept_relay_snapshot(
            &mut current,
            envelope,
            "secret-token"
        ));
        assert_eq!(
            current.unwrap().state,
            relaycat_cli::gui_bridge::GuiRelayState::Paired
        );
    }

    #[test]
    fn relay_snapshot_rejects_wrong_tokens_and_stale_revisions() {
        let initial = relaycat_cli::gui_bridge::GuiRelayStateSnapshot::syncing(4);
        let mut current = Some(initial.clone());

        assert!(!accept_relay_snapshot(
            &mut current,
            relaycat_cli::gui_bridge::GuiRelayStateEnvelope {
                token: "wrong-token".to_string(),
                snapshot: relaycat_cli::gui_bridge::GuiRelayStateSnapshot::paired(5),
            },
            "secret-token",
        ));
        assert!(!accept_relay_snapshot(
            &mut current,
            relaycat_cli::gui_bridge::GuiRelayStateEnvelope {
                token: "secret-token".to_string(),
                snapshot: relaycat_cli::gui_bridge::GuiRelayStateSnapshot::wait(4),
            },
            "secret-token",
        ));
        assert_eq!(current, Some(initial));
    }
}
