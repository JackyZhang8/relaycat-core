#[cfg(windows)]
use std::process::Command as ProcessCommand;
#[cfg(unix)]
use std::process::Command as ProcessCommand;
use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{self, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
#[cfg(unix)]
use std::{ffi::CStr, mem::MaybeUninit, os::fd::AsRawFd};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
#[cfg(not(unix))]
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use futures_util::{SinkExt, StreamExt, stream::SplitSink, stream::SplitStream};
use portable_pty::{CommandBuilder, ExitStatus, PtySize, native_pty_system};
use rand_core::{OsRng, RngCore};
use relaycat_crypto::KeyPair;
use relaycat_protocol::{
    CliMetadata, CliStatus, Direction, HelloAckV2, HelloV2, InputAckV2, InputDecision, InputDedupe,
    InputEventV2, OuterFrame, PaletteState, PatchOp, PlainMsg, ProtocolCapabilityV2, RenderAckV2,
    RequestSnapshotV2, RequestTranscriptV2, ResizeEventV2, ResumeV2, Role,
    TERMINAL_STATE_PROTOCOL_V2, TerminalColor, decode_frame, encode_frame, encode_plain_msg,
    plain_msg_type,
};
use tokio::sync::mpsc;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};

use crate::{
    command::{RelayOptions, SessionKind, TargetCommand},
    i18n::CliLanguage,
    pairing::generate_pairing_material_for_public_key_and_kind,
    pairing_store::{
        RELAYCAT_DIR, StoredPairingSession, current_unix_timestamp, ensure_gitignore, load_session,
        project_dir, save_session, session_file_path, write_pairing_qr_pngs,
    },
    plaintext::{decode_plaintext_data, encode_plaintext_data},
    secure::{CliSecureHandshake, SecureSession, secure_join_frame},
    terminal_core::{
        TerminalCore, TerminalCoreConfig, dark_terminal_palette, default_terminal_palette,
        xterm_indexed_color,
    },
    ws_url::build_ws_url,
};

// `relay.rs` is split by responsibility into the submodules below. Each
// submodule pulls shared items in via `use super::*` and re-exports its own
// items back through this module root, so the core relay loop in this file and
// the test module keep referring to them unqualified.
mod chrome;
mod diagnostics;
mod input_filter;
mod logging;
mod messages;
mod mouse_wheel;
mod negotiation;
mod output_filter;
mod pairing;
mod palette;
mod process_stats;
mod term_mode;
mod transport;
mod work_mode;

pub(crate) use chrome::*;
pub(crate) use diagnostics::*;
pub(crate) use input_filter::*;
pub(crate) use logging::*;
pub(crate) use messages::*;
pub(crate) use mouse_wheel::*;
pub(crate) use negotiation::*;
pub(crate) use output_filter::*;
pub use pairing::*;
pub(crate) use palette::*;
pub(crate) use process_stats::*;
pub(crate) use term_mode::*;
pub(crate) use transport::*;
pub(crate) use work_mode::*;

const CLI_STATUS_INTERVAL: Duration = Duration::from_secs(3);
const MIN_REMOTE_RESIZE_COLS: u16 = 20;
const MIN_REMOTE_RESIZE_ROWS: u16 = 5;
const TERMINAL_V2_PATCH_RETENTION: usize = 4096;
const RELAY_SAFE_PLAIN_MSG_BYTES: usize = 960 * 1024;
/// PTY output is coalesced over this window before a terminal patch is emitted,
/// so a burst of small writes (e.g. a build log) becomes a handful of larger
/// patches per second rather than hundreds, keeping the relay's inbound rate
/// limit happy and reducing per-patch overhead.
const PTY_OUTPUT_COALESCE_WINDOW: Duration = Duration::from_millis(250);
/// Flush coalesced PTY output early once this many bytes have accumulated, so a
/// single window can never grow an unbounded patch.
const PTY_OUTPUT_COALESCE_MAX_BYTES: usize = 256 * 1024;
/// Quiet window used only after PTY resize to absorb delayed TUI repaint bytes
/// before app-facing scrollback history is thawed again.
const PTY_RESIZE_REPAINT_QUIET_WINDOW: Duration = Duration::from_millis(25);
/// Maximum time to keep scrollback history frozen after an explicit PTY resize.
/// The quiet window absorbs delayed TUI repaint bytes, but this cap prevents
/// real post-resize output from being discarded indefinitely.
const PTY_RESIZE_REPAINT_FREEZE_MAX_WINDOW: Duration = Duration::from_millis(150);
/// Bound on buffered PTY events. A bounded channel applies backpressure to the
/// PTY reader (and thus the child process) when the relay cannot keep up,
/// instead of letting memory grow without limit during output bursts.
const PTY_EVENT_CHANNEL_CAPACITY: usize = 256;
pub(crate) const LOCAL_INTERRUPT_EXIT_WINDOW: Duration = Duration::from_millis(900);
/// How long to wait for the relay WebSocket to accept a connection.
const RELAY_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Per-send timeout on the WebSocket writer. Prevents a stalled TCP connection
/// (e.g. NAT silently drops packets without RST/FIN) from blocking indefinitely.
const WS_SEND_TIMEOUT: Duration = Duration::from_secs(10);
/// Maximum silence tolerated on the relay websocket before the transport is
/// treated as dead and reconnected. The relay pings every connected client
/// every 10s, so a healthy link always delivers inbound traffic well within
/// this window. A half-open socket (e.g. after the host switches VPN/network
/// routes) never surfaces a read error and its small keepalive writes keep
/// landing in the kernel send buffer, so silence is the only reliable signal.
const WS_READ_IDLE_TIMEOUT: Duration = Duration::from_secs(20);
const LOCAL_WORK_MODE_TOGGLE_KEY: u8 = 0x07; // Ctrl-G.

pub async fn run_plaintext(target: TargetCommand, relay: RelayOptions) -> Result<()> {
    let language = CliLanguage::from_system_locale();
    target.validate()?;
    let project_dir = project_dir(target.cwd.as_deref())?;
    if let Some(log_path) = init_log_file(&project_dir) {
        eprintln!("{}", relaycat_logs_message(&log_path, language));
    }
    let room_id = relay
        .room_id
        .clone()
        .context("plaintext dev relay mode requires --room")?;

    let ws_url = build_ws_url(&relay.url, &room_id, "cli");
    let reconnect = Arc::new(RelayTransportReconnect::Plaintext {
        ws_url,
        room_id: room_id.clone(),
    });
    let (ws_writer, ws_reader) = reconnect
        .connect()
        .await
        .with_context(|| initial_relay_unavailable_message(&target))?;
    relaycat_log(
        "INFO",
        format!("cli connected to relay room {room_id} as cli"),
    );

    run_plaintext_pty_relay(target, room_id, reconnect, ws_writer, ws_reader).await
}

type WsStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
type WsWriter = SplitSink<WsStream, Message>;
type WsReader = SplitStream<WsStream>;
type SharedSecureSession = Arc<Mutex<SecureSession>>;
type SharedSessionKeys = Arc<Mutex<relaycat_crypto::SessionKeys>>;

#[derive(Debug)]
enum PtyEvent {
    Output(Vec<u8>),
    Plain(PlainMsg),
    Exit(ExitStatus),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RelayInputAction {
    InputEventV2 {
        input_stream_id: String,
        input_seq: u64,
        bytes: Vec<u8>,
    },
    ResumeV2(ResumeV2),
    RenderAckV2(RenderAckV2),
    RequestSnapshotV2(RequestSnapshotV2),
    RequestTranscriptV2(RequestTranscriptV2),
    ResizeEventV2(ResizeEventV2),
    HelloV2(HelloV2),
    EchoHeartbeat,
    Ignore,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TerminalV2Control {
    AppConnected,
    Resume(ResumeV2),
    RenderAck(RenderAckV2),
    RequestSnapshot(RequestSnapshotV2),
    RequestTranscript(RequestTranscriptV2),
    Resize(ResizeEventV2),
    LocalResize { cols: u16, rows: u16 },
    EnterLocalMode,
    EnterRemoteMode,
    ThawHistory,
    // Freeze the app-facing scrollback history. Sent right before the PTY is
    // resized while the app is disconnected so the TUI's repaint churn at the
    // new width is discarded instead of duplicated into the app's history.
    FreezeHistory,
}

#[derive(Debug, Clone)]
struct TerminalSnapshotRequestCoalescer {
    pending: Arc<AtomicBool>,
}

impl Default for TerminalSnapshotRequestCoalescer {
    fn default() -> Self {
        Self {
            pending: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl TerminalSnapshotRequestCoalescer {
    fn observe(&self, _request: &RequestSnapshotV2) -> bool {
        self.pending
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn mark_request_finished(&self) {
        self.pending.store(false, Ordering::Release);
    }
}

fn terminal_msgs_contain_state_patch(msgs: &[PlainMsg]) -> bool {
    msgs.iter()
        .any(|msg| matches!(msg, PlainMsg::TerminalPatchV2(_)))
}

fn relay_input_action(msg: PlainMsg) -> RelayInputAction {
    match msg {
        PlainMsg::InputEventV2(InputEventV2 {
            input_stream_id,
            input_seq,
            bytes,
        }) => RelayInputAction::InputEventV2 {
            input_stream_id,
            input_seq,
            bytes,
        },
        PlainMsg::ResumeV2(resume) => RelayInputAction::ResumeV2(resume),
        PlainMsg::RenderAckV2(ack) => RelayInputAction::RenderAckV2(ack),
        PlainMsg::RequestSnapshotV2(request) => RelayInputAction::RequestSnapshotV2(request),
        PlainMsg::RequestTranscriptV2(request) => RelayInputAction::RequestTranscriptV2(request),
        PlainMsg::ResizeEventV2(event) => RelayInputAction::ResizeEventV2(event),
        PlainMsg::HelloV2(hello) => RelayInputAction::HelloV2(hello),
        PlainMsg::Heartbeat => RelayInputAction::EchoHeartbeat,
        _ => RelayInputAction::Ignore,
    }
}

fn cli_metadata_for_target(target: &TargetCommand) -> Result<CliMetadata> {
    let project_dir = project_dir(target.cwd.as_deref())?;
    let canonical = fs::canonicalize(&project_dir).unwrap_or(project_dir);
    let project_path = strip_windows_verbatim_prefix(&canonical.display().to_string());
    Ok(CliMetadata { project_path })
}

/// Drop the Windows extended-length (`\\?\`) prefix that `fs::canonicalize`
/// returns, so the path shown to the mobile app reads `C:\relaycat` rather than
/// `\\?\C:\relaycat`. UNC paths (`\\?\UNC\server\share`) collapse back to the
/// familiar `\\server\share` form. Non-Windows paths are returned unchanged.
fn strip_windows_verbatim_prefix(path: &str) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        path.to_string()
    }
}

fn project_name_for_terminal_chrome(target_cwd: Option<&Path>) -> Option<String> {
    let project_dir = project_dir(target_cwd).ok()?;
    let project_path = fs::canonicalize(&project_dir).unwrap_or(project_dir);
    let name = project_path.file_name()?.to_str()?;
    normalized_title_part(Some(name))
}

fn normalized_title_part(value: Option<&str>) -> Option<String> {
    let value = value?
        .chars()
        .filter(|ch| !ch.is_control())
        .collect::<String>();
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn can_send_without_terminal_resume(msg: &PlainMsg) -> bool {
    matches!(
        msg,
        PlainMsg::CliMetadata(_)
            | PlainMsg::CliStatus(_)
            | PlainMsg::Heartbeat
            | PlainMsg::InputAckV2(_)
            | PlainMsg::ResizeAckV2(_)
            | PlainMsg::ProcessExit { .. }
            // The capability handshake reply must reach the app before any
            // terminal resume, so it is never gated by the resume window.
            | PlainMsg::HelloAckV2(_)
    )
}

/// Feed any coalesced PTY output into the terminal core and return the
/// resulting patch (if there was buffered output). Clears the flush deadline.
fn flush_pending_output(
    pending_output: &mut Vec<u8>,
    flush_deadline: &mut Option<Instant>,
    terminal_core: &mut TerminalCore,
    diagnostic_session_kind: Option<&str>,
) -> Vec<PlainMsg> {
    *flush_deadline = None;
    if pending_output.is_empty() {
        return Vec::new();
    }
    let bytes = std::mem::take(pending_output);
    let Some(patch) = terminal_core.feed_vt_bytes(&bytes) else {
        return Vec::new();
    };
    if let Some(session_kind) = diagnostic_session_kind {
        relaycat_log(
            "INFO",
            terminal_patch_diagnostic_line(session_kind, &bytes, terminal_core, &patch),
        );
    }
    vec![PlainMsg::TerminalPatchV2(patch)]
}

fn mark_local_mode_dirty_from_msgs(
    msgs: &[PlainMsg],
    relay_output_in_local_mode: bool,
    local_mode_dirty: &mut bool,
) {
    if relay_output_in_local_mode && terminal_msgs_contain_state_patch(msgs) {
        *local_mode_dirty = true;
    }
}

/// Resolve once `deadline` is reached; never resolves when there is no pending
/// deadline so it can be used as an inert branch in a `tokio::select!`.
async fn sleep_until_opt(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => {
            tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
        }
        None => std::future::pending::<()>().await,
    }
}

#[derive(Debug, Default)]
struct DeferredHistoryThaw {
    started_at: Option<Instant>,
    deadline: Option<Instant>,
}

impl DeferredHistoryThaw {
    fn request(&mut self, now: Instant) {
        self.started_at = Some(now);
        self.deadline = Some(now + PTY_RESIZE_REPAINT_QUIET_WINDOW);
    }

    fn observe_pty_output(&mut self, now: Instant) {
        if self.deadline.is_some()
            && let Some(started_at) = self.started_at
        {
            self.deadline = Some(
                (now + PTY_RESIZE_REPAINT_QUIET_WINDOW)
                    .min(started_at + PTY_RESIZE_REPAINT_FREEZE_MAX_WINDOW),
            );
        }
    }

    fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    fn take_due(&mut self, now: Instant) -> bool {
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            self.clear();
            return true;
        }
        false
    }

    fn take_render_ack_thaw(&self) -> bool {
        self.deadline.is_none()
    }

    fn cancel(&mut self) {
        self.clear();
    }

    fn clear(&mut self) {
        self.started_at = None;
        self.deadline = None;
    }
}

async fn run_plaintext_pty_relay(
    target: TargetCommand,
    room_id: String,
    reconnect: Arc<RelayTransportReconnect>,
    ws_writer: WsWriter,
    ws_reader: WsReader,
) -> Result<()> {
    run_pty_relay(
        target,
        move |msg, transport_seq| {
            encode_plaintext_data(room_id.clone(), Direction::CliToApp, transport_seq, msg)
        },
        move |frame| decode_plaintext_data(&frame),
        || Ok(()),
        Arc::new(AtomicBool::new(true)),
        // Plaintext relay has no capability handshake, so attribute tables are
        // always sent in full (legacy semantics).
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicU64::new(1)),
        Arc::new(Mutex::new(AppResumeGate { resume_ready: true })),
        reconnect,
        ws_writer,
        ws_reader,
    )
    .await
}

async fn run_secure_pty_relay(
    target: TargetCommand,
    room_id: String,
    keys: relaycat_crypto::SessionKeys,
    handshake: Arc<CliSecureHandshake>,
    reconnect: Arc<RelayTransportReconnect>,
    ws_writer: WsWriter,
    ws_reader: WsReader,
) -> Result<()> {
    let output_session = Arc::new(Mutex::new(SecureSession::new(
        room_id.clone(),
        keys.clone(),
    )));
    let input_session = Arc::new(Mutex::new(SecureSession::new(
        room_id.clone(),
        keys.clone(),
    )));
    let current_keys = Arc::new(Mutex::new(keys));
    let output_session_for_decode = output_session.clone();
    let input_session_for_decode = input_session.clone();
    let current_keys_for_decode = current_keys.clone();
    let app_connected = Arc::new(AtomicBool::new(true));
    let app_connected_for_decode = app_connected.clone();
    // Flipped on once the app's Hello advertises `IncrementalAttrs`; shared with
    // the terminal model so its patches start sending only the appended attr
    // tail instead of the whole table on growth.
    let incremental_attrs_negotiated = Arc::new(AtomicBool::new(false));
    let incremental_attrs_negotiated_for_decode = incremental_attrs_negotiated.clone();
    let app_join_generation = Arc::new(AtomicU64::new(1));
    let app_join_generation_for_decode = app_join_generation.clone();
    let resume_gate = Arc::new(Mutex::new(AppResumeGate::default()));
    let resume_gate_for_decode = resume_gate.clone();

    run_pty_relay(
        target,
        move |msg, _transport_seq| {
            output_session
                .lock()
                .map_err(|_| anyhow::anyhow!("secure output session lock poisoned"))?
                .encode(Direction::CliToApp, msg)
        },
        move |frame| {
            if matches!(&frame, OuterFrame::PeerLeft { role: Role::App }) {
                mark_app_disconnected(&app_connected_for_decode);
                if let Ok(mut gate) = resume_gate_for_decode.lock() {
                    gate.mark_app_disconnected();
                }
                return Ok(None);
            }
            match accept_secure_peer_joined_and_reset_sessions(
                &handshake,
                &frame,
                &room_id,
                &output_session_for_decode,
                &input_session_for_decode,
                &current_keys_for_decode,
            )? {
                SecurePeerJoined::SessionReset => {
                    app_connected_for_decode.store(true, Ordering::Release);
                    app_join_generation_for_decode.fetch_add(1, Ordering::Release);
                    if let Ok(mut gate) = resume_gate_for_decode.lock() {
                        gate.mark_app_rejoined();
                    }
                    return Ok(None);
                }
                SecurePeerJoined::SessionPreserved => {
                    app_connected_for_decode.store(true, Ordering::Release);
                    // The app kept its SecureSession (and its terminal
                    // stream), so its earlier resume still stands; re-gating
                    // on a new ResumeV2 would stall terminal output because
                    // the app has no reason to send one.
                    if let Ok(mut gate) = resume_gate_for_decode.lock() {
                        gate.mark_resume_processed();
                    }
                    return Ok(None);
                }
                SecurePeerJoined::NotPeerJoined => {}
            }
            let decoded = input_session_for_decode
                .lock()
                .map_err(|_| anyhow::anyhow!("secure input session lock poisoned"))?
                .decode(&frame)?;
            // The app's Hello advertises whether it can decode compressed
            // payloads. As soon as we see it, enable compression on the
            // CLI->App session so subsequent snapshots/patches are compressed.
            if let Some(PlainMsg::HelloV2(hello)) = &decoded
                && hello
                    .capabilities
                    .contains(&ProtocolCapabilityV2::Compression)
            {
                output_session_for_decode
                    .lock()
                    .map_err(|_| anyhow::anyhow!("secure output session lock poisoned"))?
                    .set_compress_outbound(true);
            }
            // The same Hello tells us whether the app can append attribute-table
            // tails; if so, switch patches to incremental-attr mode.
            if let Some(PlainMsg::HelloV2(hello)) = &decoded
                && hello
                    .capabilities
                    .contains(&ProtocolCapabilityV2::IncrementalAttrs)
            {
                incremental_attrs_negotiated_for_decode.store(true, Ordering::Release);
            }
            Ok(decoded)
        },
        || Ok(()),
        app_connected,
        incremental_attrs_negotiated,
        app_join_generation,
        resume_gate,
        reconnect,
        ws_writer,
        ws_reader,
    )
    .await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SecurePeerJoined {
    /// The frame was not a verifiable `PeerJoined(app)`.
    NotPeerJoined,
    /// The re-derived keys match the current session keys (neither side's
    /// salt changed), so the peer kept its `SecureSession` and counters.
    SessionPreserved,
    /// New keys were derived and both secure sessions were reset.
    SessionReset,
}

fn accept_secure_peer_joined_and_reset_sessions(
    handshake: &CliSecureHandshake,
    frame: &OuterFrame,
    room_id: &str,
    output_session: &SharedSecureSession,
    input_session: &SharedSecureSession,
    current_keys: &SharedSessionKeys,
) -> Result<SecurePeerJoined> {
    let Some(keys) = handshake.accept_peer_joined(frame)? else {
        return Ok(SecurePeerJoined::NotPeerJoined);
    };

    let mut current = current_keys
        .lock()
        .map_err(|_| anyhow::anyhow!("secure keys lock poisoned"))?;
    // Skip the reset when the derived keys are unchanged (a CLI transport
    // reconnect echoes the still-present app's PeerJoined back with the same
    // salts). Re-creating the sessions would reset the sequence counters and
    // re-encrypt from seq 1 under the same key — reusing nonces — and the
    // app, which kept its counters, would reject the replayed sequences.
    // Mirrors the iOS/Android keys-equality skip in
    // `resetSecureSessionAfterCliJoined`.
    if *current == keys {
        return Ok(SecurePeerJoined::SessionPreserved);
    }
    *current = keys.clone();
    drop(current);
    reset_secure_sessions(room_id, keys, output_session, input_session)?;

    Ok(SecurePeerJoined::SessionReset)
}

fn reset_secure_sessions(
    room_id: &str,
    keys: relaycat_crypto::SessionKeys,
    output_session: &SharedSecureSession,
    input_session: &SharedSecureSession,
) -> Result<()> {
    *output_session
        .lock()
        .map_err(|_| anyhow::anyhow!("secure output session lock poisoned"))? =
        SecureSession::new(room_id.to_string(), keys.clone());
    *input_session
        .lock()
        .map_err(|_| anyhow::anyhow!("secure input session lock poisoned"))? =
        SecureSession::new(room_id.to_string(), keys);

    Ok(())
}

/// Re-registering with the relay after a Ctrl-G return to RemoteMode.
///
/// On a mode switch the CLI deliberately closed its websocket, so the app may
/// be tearing down and reconnecting with a fresh join. That fresh app join is
/// what drives the PeerJoined handler, which re-derives keys and resets the
/// secure sessions in lockstep with the app.
///
/// Therefore a plain CLI/GUI transport reconnect must not reset sessions or
/// mark the app connected here. Until PeerJoined(app) arrives, terminal output
/// stays gated by `app_connected=false`; if the app stayed in the room, both
/// sides keep their existing secure counters.
fn preserve_sessions_on_transport_reconnect(
    _on_transport_reconnected: &mut impl FnMut() -> Result<()>,
) -> Result<()> {
    Ok(())
}

/// Clear the "reconnecting" title once the CLI has re-registered its relay
/// transport after a Ctrl-G return to RemoteMode.
///
/// Re-registering at the relay is the only thing the CLI has to do on a mode
/// switch and it never involves the app: the app was evicted when we dropped
/// the websocket for LocalMode and reconnects on its own schedule. So the CLI's
/// connection indicator should flip to connected as soon as the transport is
/// back (~80ms), not when the app eventually rejoins — gating it on the app's
/// rejoin is what made the switch look like a multi-second stall.
///
/// This intentionally does NOT mark the app connected: `app_connected` stays
/// false until the app's PeerJoined establishes whether a fresh secure session
/// is needed, so terminal state is still withheld until then.
fn mark_relay_registered_after_mode_switch(status_bar: &TerminalStatusBar) {
    status_bar.clear_hint();
}

/// Complete a reconnect after an *unexpected* transport drop (the socket failed
/// while we were still in RemoteMode).
///
/// Unlike the mode-switch path, here the app may NOT have been evicted: if our
/// new join reaches the relay before it processes the old connection's close,
/// the app stays in the room and only sees PeerJoined(cli). Since the CLI join
/// now reuses the existing salt, the app keeps its current secure session and
/// sequence counters. We therefore mark the app connected eagerly so the output
/// task resumes, but we leave `SecureSession` untouched.
fn complete_transport_reconnect(
    app_connected: &AtomicBool,
    resume_gate: &Arc<Mutex<AppResumeGate>>,
    _on_transport_reconnected: &mut impl FnMut() -> Result<()>,
) -> Result<()> {
    app_connected.store(true, Ordering::Release);
    if let Ok(mut gate) = resume_gate.lock() {
        gate.mark_app_rejoined();
    }
    Ok(())
}

/// Number of consecutive secure-decode failures on the App->CLI stream that are
/// tolerated before the transport is forcibly reconnected. A single dropped or
/// corrupt frame desyncs the expected sequence counter and every later frame
/// then fails to decode. Reconnecting renews the websocket but intentionally
/// preserves the secure session; the threshold keeps a relay that injects junk
/// from triggering a tight reconnect loop.
const MAX_CONSECUTIVE_SECURE_DECODE_ERRORS: u32 = 8;

#[allow(clippy::too_many_arguments)]
async fn run_pty_relay<Encode, Decode>(
    target: TargetCommand,
    mut encode_output: Encode,
    mut decode_input: Decode,
    mut on_transport_reconnected: impl FnMut() -> Result<()> + Send + 'static,
    app_connected: Arc<AtomicBool>,
    // Set once the app's Hello advertises `IncrementalAttrs`. Shared with the
    // terminal model so its patches send only the appended attribute-table tail
    // (instead of the whole table) once the peer can append it.
    incremental_attrs: Arc<AtomicBool>,
    app_join_generation: Arc<AtomicU64>,
    resume_gate: Arc<Mutex<AppResumeGate>>,
    reconnect: Arc<RelayTransportReconnect>,
    ws_writer: WsWriter,
    ws_reader: WsReader,
) -> Result<()>
where
    Encode: FnMut(PlainMsg, u64) -> Result<OuterFrame> + Send + 'static,
    Decode: FnMut(OuterFrame) -> Result<Option<PlainMsg>> + Send + 'static,
{
    let _local_terminal_guard = LocalTerminalModeGuard::new();
    let terminal_palette = local_terminal_palette();
    let pty_system = native_pty_system();
    // Windows GUI pipe bridge: seed the shared "host terminal" size from the
    // initial size the GUI handed us, so `current_terminal_size()` reports the
    // GUI desktop's dimensions (a pipe exposes none). The app-size clamp and the
    // Ctrl-G local resize then use the real desktop size, exactly as on
    // macOS/Linux where the CLI owns a real terminal.
    if crate::gui_bridge::gui_bridge_pipe_mode()
        && let Some((cols, rows)) = crate::gui_bridge::gui_bridge_initial_size()
    {
        crate::gui_bridge::set_bridge_terminal_size(cols, rows);
    }
    // Prefer the real terminal's size; on the Windows GUI pipe bridge this now
    // resolves to the GUI host size seeded above, and only then to a
    // conservative default.
    let pty_size = current_terminal_size()
        .or_else(|| {
            crate::gui_bridge::gui_bridge_initial_size().map(|(cols, rows)| PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
        })
        .unwrap_or(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        });
    let local_pty_size = local_content_pty_size(pty_size);
    let initial_terminal_cols = local_pty_size.cols;
    let initial_terminal_rows = local_pty_size.rows;
    let chrome_title_context = Arc::new(TerminalChromeTitleContext::for_target(&target));
    init_terminal_chrome(
        PtyWorkModeKind::Remote,
        pty_size,
        chrome_title_context.as_ref(),
    );
    let pair = pty_system
        .openpty(local_pty_size)
        .context("failed to open PTY")?;

    // On Windows, npm-installed CLIs (codex, opencode, …) are `.cmd`/`.bat`
    // shims, which `CreateProcessW` — the syscall portable_pty uses to launch
    // the ConPTY child — cannot execute directly (it only runs real PE
    // binaries). A real `.exe` like PowerShell spawns fine, which is why shell
    // relay sessions work while codex/opencode silently fail to spawn the
    // instant the app pairs (the inner tool is launched only once pairing
    // completes). Route batch-file shims through `cmd.exe` so they run.
    #[cfg(windows)]
    let (spawn_program, spawn_args) = windows_spawn_command(&target.program, &target.args);
    #[cfg(not(windows))]
    let (spawn_program, spawn_args): (String, Vec<String>) =
        (target.program.clone(), target.args.clone());

    let mut command = CommandBuilder::new(&spawn_program);
    command.args(&spawn_args);
    if let Some(cwd) = &target.cwd {
        command.cwd(cwd);
    }
    configure_child_terminal_env(&mut command, &target.session_kind);
    // On Windows, portable_pty's CommandBuilder rebuilds the child's
    // environment from the Windows Registry, which overwrites PATH with the
    // registry-only value and loses entries added at runtime (Volta, nvm,
    // user shell profiles, etc.).  Restore the parent's actual PATH so the
    // inner tool (and the cmd.exe wrapper above) and its dependencies are
    // findable.
    #[cfg(windows)]
    {
        if let Ok(path) = std::env::var("PATH") {
            command.env("PATH", &path);
        }
        // Don't leak GUI bridge variables into the inner tool's environment.
        // The inner tool runs inside a ConPTY and must not behave as if it
        // were a GUI pipe-bridge relay child.
        command.env_remove(crate::gui_bridge::GUI_BRIDGE_ENV);
        command.env_remove(crate::gui_bridge::GUI_BRIDGE_COLS_ENV);
        command.env_remove(crate::gui_bridge::GUI_BRIDGE_ROWS_ENV);
        command.env_remove(crate::gui_bridge::GUI_BRIDGE_RESIZE_FILE_ENV);
        command.env_remove("RELAYCAT_GUI_PARENT_PID");
    }

    let mut child = pair
        .slave
        .spawn_command(command)
        .with_context(|| format!("failed to spawn {}", target.program))?;
    let child_pid = child.process_id();
    let child_program_name = target.program.clone();
    let child_program_name_for_exit = child_program_name.clone();
    let child_killer = Arc::new(Mutex::new(child.clone_killer()));
    relaycat_log(
        "INFO",
        format!(
            "cli started {} {:?} in PTY; waiting for app input",
            target.program, target.args
        ),
    );
    relaycat_log(
        "INFO",
        format!(
            "size: initial PTY {}x{} (cols x rows) taken from the local terminal",
            initial_terminal_cols, initial_terminal_rows
        ),
    );
    drop(pair.slave);
    let mut pty_reader = pair
        .master
        .try_clone_reader()
        .context("failed to clone PTY reader")?;
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut pty_writer = pair
        .master
        .take_writer()
        .context("failed to open PTY writer")?;
    #[cfg(windows)]
    if target.session_kind.is_shell() {
        if let Err(err) = pty_writer
            .write_all(b"\r\n")
            .and_then(|_| pty_writer.flush())
        {
            relaycat_log(
                "WARN",
                format!("failed to prime Windows shell prompt: {err}"),
            );
        } else {
            relaycat_log(
                "INFO",
                "primed Windows shell prompt with an empty input line",
            );
        }
    }
    let pty_writer = std::sync::Arc::new(std::sync::Mutex::new(pty_writer));

    let master = std::sync::Arc::new(std::sync::Mutex::new(Some(pair.master)));
    let master_for_resize = master.clone();
    let (terminal_v2_control_tx, mut terminal_v2_control_rx) =
        mpsc::unbounded_channel::<TerminalV2Control>();
    let (reconnect_signal_tx, mut reconnect_signal_rx) = mpsc::unbounded_channel::<()>();
    let terminal_snapshot_request_coalescer = TerminalSnapshotRequestCoalescer::default();
    // Track the local terminal size, but do not let SIGWINCH or app disconnects
    // resize the PTY. Resize is now tied to explicit work-mode transitions:
    // entering RemoteMode accepts one app-reported size; entering LocalMode
    // applies the local size.
    let master_for_local_size = master.clone();
    let local_size_task = tokio::spawn(async move {
        #[cfg(not(unix))]
        {
            let _ = &master_for_local_size;
        }

        #[cfg(unix)]
        {
            let mut sigwinch =
                match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())
                {
                    Ok(s) => s,
                    Err(err) => {
                        relaycat_log("WARN", format!("failed to install SIGWINCH handler: {err}"));
                        return;
                    }
                };
            loop {
                if sigwinch.recv().await.is_none() {
                    return;
                }
                // Deliberately ignored. A window-size change alone is not a
                // takeover signal; applying it here would invalidate the app's
                // snapshot base during backgrounding/network churn.
                let _ = &master_for_local_size;
            }
        }
    });

    let (output_tx, mut output_rx) = mpsc::channel::<PtyEvent>(PTY_EVENT_CHANNEL_CAPACITY);
    let status_bar: SharedTerminalStatusBar = Arc::new(TerminalStatusBar::default());
    let status_bar_for_output_thread = status_bar.clone();
    let status_bar_for_relay_output = status_bar.clone();
    let status_bar_for_relay_input = status_bar.clone();
    let status_bar_for_chrome_refresh = status_bar.clone();
    let output_thread_tx = output_tx.clone();
    let pty_writer_for_output_thread = pty_writer.clone();
    let input_filter_session_kind = Arc::new(Mutex::new(target.session_kind.clone()));
    // opencode forwards app touch scrolls as SGR wheel reports; track the mouse
    // modes it enables so those reports can be re-encoded into the format it
    // actually negotiated (or dropped while tracking is off).
    let rewrite_app_wheel_input = target.session_kind.as_str() == "opencode";
    // Full-screen children repaint only the clamped grid after a RemoteMode
    // resize, so the host screen is cleared first to blank the leftover margin;
    // shells never repaint, so clearing would wipe their visible output.
    let clear_host_on_remote_clamp = !target.session_kind.is_shell();
    let mouse_report_modes = Arc::new(Mutex::new(MouseReportModes::default()));
    let mouse_report_modes_for_output_thread = mouse_report_modes.clone();
    let mouse_report_modes_for_relay_input = mouse_report_modes.clone();
    let output_filter_color_query_palette =
        child_color_query_palette(&terminal_palette, &target.session_kind);
    let strip_alternate_screen_from_remote = target.session_kind.uses_managed_alt_screen();
    // On Windows the GUI relay child runs under a ConPTY and the host xterm's
    // CPR response never makes it back into the inner pseudoconsole, so answer
    // the shell's cursor-position query ourselves rather than forwarding it.
    let answer_cursor_position_query = cfg!(windows) && gui_pairing_quiet();
    let pty_work_mode = Arc::new(Mutex::new(PtyWorkMode::new()));
    // Tracks the height the PTY is currently sized to (the clamped app
    // viewport). The output thread reads it to expand a full-screen child's
    // bottom-anchored scroll region to the taller host terminal's height; the
    // resize handlers below update it whenever they re-size the PTY.
    let pty_viewport_rows = Arc::new(AtomicU16::new(pty_size.rows));
    let pty_viewport_rows_for_output = pty_viewport_rows.clone();
    let pty_viewport_rows_for_local_input = pty_viewport_rows.clone();
    let pty_viewport_rows_for_relay_input = pty_viewport_rows.clone();
    let chrome_mode = pty_work_mode.clone();
    let chrome_title_context_for_refresh = chrome_title_context.clone();
    let chrome_fallback_size = pty_size;
    let chrome_refresh_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            render_terminal_chrome_for_status_bar(
                current_pty_work_mode(&chrome_mode),
                &status_bar_for_chrome_refresh,
                chrome_fallback_size,
                chrome_title_context_for_refresh.as_ref(),
            );
        }
    });
    let pty_work_mode_for_output_thread = pty_work_mode.clone();
    let output_thread = thread::spawn(move || -> std::io::Result<()> {
        // Lock briefly per write rather than holding the stdout lock for the
        // thread's entire lifetime. A long-lived lock here would block
        // set_hint / clear_hint (called from the relay_input task on a tokio
        // worker thread) from ever writing the OSC title sequence, so the
        // disconnect indicator in the terminal title would never appear.
        let stdout = io::stdout();
        let mut output_filter =
            LocalOutputFilter::new_with_color_query_palette(output_filter_color_query_palette);
        output_filter.strip_alternate_screen_from_remote = strip_alternate_screen_from_remote;
        output_filter.answer_cursor_position_query = answer_cursor_position_query;
        let _ = pty_work_mode_for_output_thread;
        let mut buffer = [0_u8; 8192];
        let mut logged_first_read = false;
        loop {
            let read = pty_reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            let log_this_read = !logged_first_read;
            if log_this_read {
                relaycat_log(
                    "INFO",
                    format!(
                        "PTY output first read: {read} bytes raw={}",
                        format_byte_preview(&buffer[..read])
                    ),
                );
                logged_first_read = true;
            }
            // Refresh the sizes the filter uses to expand a full-screen child's
            // scroll region for the host terminal. `pty_rows` is the clamped
            // viewport the PTY is sized to; `host_rows` is the physical host
            // terminal height (re-read here so a mid-session host resize is
            // picked up). TIOCGWINSZ is a cheap ioctl, so polling per read is
            // fine.
            output_filter.pty_rows = pty_viewport_rows_for_output.load(Ordering::Acquire);
            // On the Windows GUI pipe bridge the desktop terminal is letterboxed
            // to the negotiated (phone) grid, so its xterm is exactly `pty_rows`
            // tall — never taller. Expanding a full-screen child's bottom-anchored
            // scroll region to the desktop *window* height (as we do for a real,
            // physically taller host terminal) would rewrite codex/opencode's
            // `\x1b[1;{pty_rows}r` to the window height while its content was drawn
            // for `pty_rows`, trapping/misaligning it (the app renders the original
            // sequence and stays correct — hence "phone perfect, desktop garbled").
            // Report `host_rows == pty_rows` so the expansion is skipped and the
            // desktop's `local_output` matches what the phone sees.
            output_filter.host_rows = if crate::gui_bridge::gui_bridge_pipe_mode() {
                output_filter.pty_rows
            } else {
                current_terminal_size().map(|size| size.rows).unwrap_or(0)
            };
            if rewrite_app_wheel_input
                && let Ok(mut modes) = mouse_report_modes_for_output_thread.lock()
            {
                modes.observe_output(&buffer[..read]);
            }
            let filtered_output = output_filter.filter(&buffer[..read]);
            if log_this_read {
                relaycat_log(
                    "INFO",
                    format!(
                        "PTY output filtered: local={} remote={}",
                        filtered_output.local_output.len(),
                        filtered_output.remote_output.len()
                    ),
                );
            }
            if !filtered_output.pty_input.is_empty()
                && let Ok(mut writer) = pty_writer_for_output_thread.lock()
            {
                writer.write_all(&filtered_output.pty_input)?;
                writer.flush()?;
                relaycat_log(
                    "INFO",
                    format!(
                        "answered terminal query with {} bytes",
                        filtered_output.pty_input.len()
                    ),
                );
            }
            if !filtered_output.local_output.is_empty() {
                {
                    let mut handle = stdout.lock();
                    handle.write_all(&filtered_output.local_output)?;
                    handle.flush()?;
                }
                status_bar_for_output_thread.observe_child_output(&filtered_output.local_output);
            }
            if !filtered_output.remote_output.is_empty()
                && output_thread_tx
                    .blocking_send(PtyEvent::Output(filtered_output.remote_output))
                    .is_err()
            {
                break;
            }
        }
        Ok(())
    });

    // On the Windows GUI pipe bridge, stdin is a pipe (not a TTY), so
    // `is_terminal()` is false even though the GUI forwards real keystrokes
    // through it. Force the local-input thread on so the user can type into the
    // bridged terminal (drive codex/shell/... from the GUI window).
    let _local_input_thread = if io::stdin().is_terminal()
        || crate::gui_bridge::gui_bridge_pipe_mode()
    {
        let pty_writer = pty_writer.clone();
        let master = master.clone();
        let child_killer = child_killer.clone();
        let child_pid_for_input = child_pid;
        let input_output_tx = output_tx.clone();
        let input_filter_session_kind = input_filter_session_kind.clone();
        let terminal_v2_control_tx = terminal_v2_control_tx.clone();
        let pty_work_mode = pty_work_mode.clone();
        let reconnect_signal_tx = reconnect_signal_tx.clone();
        let status_bar_for_input_thread = status_bar.clone();
        let input_thread_title_context = chrome_title_context.clone();
        let local_mode_fallback_host_size = pty_size;
        Some(thread::spawn(move || -> std::io::Result<()> {
            let mut stdin = io::stdin().lock();
            let mut buffer = [0_u8; 8192];
            let mut input_filter = LocalInputFilter::new();
            let mut last_interrupt_at = None;

            loop {
                let read = stdin.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                let filtered = input_filter.filter(&buffer[..read]);
                if !filtered.mirrored_output.is_empty() {
                    // Ignore send error — relay_output may have exited due to a
                    // WebSocket failure, but that must not kill local Ctrl-C handling.
                    let _ =
                        input_output_tx.blocking_send(PtyEvent::Output(filtered.mirrored_output));
                }
                for _ in 0..filtered.toggle_work_mode_count {
                    let mode_after_toggle = pty_work_mode
                        .lock()
                        .map(|mut mode| {
                            if mode.current() == PtyWorkModeKind::Remote {
                                let _ = mode.enter_local_mode();
                            } else {
                                let _ = mode.enter_remote_mode();
                            }
                            mode.current()
                        })
                        .unwrap_or(PtyWorkModeKind::Remote);
                    match mode_after_toggle {
                        PtyWorkModeKind::Local => {
                            force_render_terminal_chrome_for_status_bar(
                                mode_after_toggle,
                                &status_bar_for_input_thread,
                                local_mode_fallback_host_size,
                                input_thread_title_context.as_ref(),
                            );
                            let host_size =
                                current_terminal_size().unwrap_or(local_mode_fallback_host_size);
                            let size = local_content_pty_size(host_size);
                            relaycat_log(
                                "INFO",
                                format!(
                                    "size: entering LocalMode from Ctrl-G; applied PTY resize to {}x{} (cols x rows)",
                                    size.cols, size.rows
                                ),
                            );
                            let _ = terminal_v2_control_tx.send(TerminalV2Control::EnterLocalMode);
                            let _ = terminal_v2_control_tx.send(TerminalV2Control::FreezeHistory);
                            if let Ok(guard) = master.lock()
                                && let Some(m) = guard.as_ref()
                            {
                                let _ = m.resize(size);
                            }
                            // Local mode drives the child from the desktop, so the
                            // PTY is sized to the full desktop window. Tell the GUI
                            // to stop letterboxing to the phone grid and use its
                            // full width; remote mode re-pins it below.
                            clear_remote_size_for_gui();
                            pty_viewport_rows_for_local_input.store(size.rows, Ordering::Release);
                            let _ = terminal_v2_control_tx.send(TerminalV2Control::LocalResize {
                                cols: size.cols,
                                rows: size.rows,
                            });
                            let _ = terminal_v2_control_tx.send(TerminalV2Control::ThawHistory);
                        }
                        PtyWorkModeKind::Remote => {
                            render_terminal_chrome_for_status_bar(
                                mode_after_toggle,
                                &status_bar_for_input_thread,
                                local_mode_fallback_host_size,
                                input_thread_title_context.as_ref(),
                            );
                            let cached_remote_size = pty_work_mode
                                .lock()
                                .ok()
                                .and_then(|mode| mode.remote_size());
                            relaycat_log(
                                "INFO",
                                format!(
                                    "size: entering RemoteMode from Ctrl-G; cached app size={:?}",
                                    cached_remote_size
                                ),
                            );
                            // Always freeze history before sending ThawHistory
                            // so that any pending resize-repaint churn is
                            // discarded even when there is no cached remote
                            // size to trigger a Resize.
                            let _ = terminal_v2_control_tx.send(TerminalV2Control::FreezeHistory);
                            if let Some(app_size) = cached_remote_size {
                                // Re-apply the RemoteMode sizing rule.
                                // `cached_remote_size` is the phone's reported
                                // size; clamping it to the current host (desktop
                                // window on the GUI pipe bridge) picks up any
                                // window resize on this Ctrl-G re-entry.
                                let (pty_cols, pty_rows) = effective_remote_size(
                                    app_size.0,
                                    app_size.1,
                                    current_terminal_size(),
                                );
                                if let Some(size) = remote_resize_pty_size(pty_cols, pty_rows) {
                                    if clear_host_on_remote_clamp {
                                        clear_host_terminal_for_remote_clamp(
                                            pty_cols,
                                            pty_rows,
                                            current_terminal_size(),
                                        );
                                    }
                                    if let Ok(guard) = master.lock()
                                        && let Some(m) = guard.as_ref()
                                    {
                                        let _ = m.resize(size);
                                    }
                                    // Tell the GUI the negotiated grid so it can
                                    // letterbox the desktop window to match.
                                    emit_remote_size_for_gui(pty_cols, pty_rows);
                                    pty_viewport_rows_for_local_input
                                        .store(pty_rows, Ordering::Release);
                                    let (model_cols, model_rows) =
                                        remote_model_size((pty_cols, pty_rows));
                                    let _ = terminal_v2_control_tx.send(TerminalV2Control::Resize(
                                        ResizeEventV2 {
                                            resize_seq: 0,
                                            cols: model_cols,
                                            rows: model_rows,
                                            input_stream_id: "remote-mode".to_string(),
                                            last_input_ack: 0,
                                        },
                                    ));
                                }
                            }
                            let _ = terminal_v2_control_tx.send(TerminalV2Control::ThawHistory);
                            let _ = terminal_v2_control_tx.send(TerminalV2Control::EnterRemoteMode);
                            let _ = reconnect_signal_tx.send(());
                        }
                    }
                }
                if filtered.pty_input.is_empty() {
                    continue;
                }
                if filtered.pty_input.contains(&0x03) {
                    let now = Instant::now();
                    let current_session_kind = input_filter_session_kind
                        .lock()
                        .map(|guard| guard.clone())
                        .unwrap_or_default();
                    match local_interrupt_action_for_session(
                        current_session_kind,
                        last_interrupt_at,
                        now,
                    ) {
                        LocalInterruptAction::Exit => {
                            // Kill the whole process group while the leader is
                            // still alive (so its pid can't be recycled), taking
                            // down any command the user was interrupting; then
                            // reap the leader itself.
                            kill_child_process_group(child_pid_for_input);
                            if let Ok(mut guard) = child_killer.lock() {
                                let _ = guard.kill();
                            }
                            if let Ok(mut guard) = master.lock() {
                                guard.take();
                            }
                            break;
                        }
                        LocalInterruptAction::Forward => {
                            last_interrupt_at = Some(now);
                        }
                    }
                }
                let Ok(mut writer) = pty_writer.lock() else {
                    break;
                };
                writer.write_all(&filtered.pty_input)?;
                writer.flush()?;
            }

            Ok(())
        }))
    } else {
        None
    };

    // Windows GUI pipe bridge: a pipe carries no resize signal, so the GUI host
    // writes the new "<cols> <rows>" to a file whenever its terminal widget
    // reflows. Poll it and publish the value as the shared host-terminal size so
    // `current_terminal_size()` tracks the live GUI desktop. Both RemoteMode and
    // Ctrl-G local mode size the child PTY to this desktop size, so a window
    // resize is picked up on the next resize/Ctrl-G, matching the desktop CLI.
    if crate::gui_bridge::gui_bridge_pipe_mode()
        && let Some(resize_path) = crate::gui_bridge::gui_bridge_resize_file()
    {
        thread::spawn(move || {
            let mut last: Option<(u16, u16)> = Some((initial_terminal_cols, initial_terminal_rows));
            loop {
                thread::sleep(Duration::from_millis(150));
                let Ok(contents) = std::fs::read_to_string(&resize_path) else {
                    continue;
                };
                let Some((cols, rows)) = crate::gui_bridge::parse_resize_line(&contents) else {
                    continue;
                };
                if last == Some((cols, rows)) {
                    continue;
                }
                last = Some((cols, rows));
                crate::gui_bridge::set_bridge_terminal_size(cols, rows);
            }
        });
    }

    let pty_writer_for_relay = pty_writer.clone();
    let (writer_update_tx, mut writer_update_rx) = mpsc::unbounded_channel::<WsWriter>();
    let reconnect_for_input = reconnect.clone();
    let writer_update_tx_for_input = writer_update_tx.clone();
    let app_connected_for_input = app_connected.clone();
    let app_join_generation_for_input = app_join_generation.clone();
    let resume_gate_for_input = resume_gate.clone();
    let heartbeat_output_tx_for_input = output_tx.clone();
    let terminal_v2_control_tx_for_input = terminal_v2_control_tx.clone();
    let reconnect_signal_tx_for_output = reconnect_signal_tx.clone();
    let pty_work_mode_for_input = pty_work_mode.clone();
    let pty_work_mode_for_relay_input = pty_work_mode.clone();
    let terminal_palette_for_relay = terminal_palette.palette.clone();
    let pty_work_mode_for_relay_output = pty_work_mode.clone();
    let status_bar_for_resize = status_bar.clone();
    let resize_title_context = chrome_title_context.clone();
    let cli_metadata = cli_metadata_for_target(&target)?;
    let record_primary_screen_frames = target.session_kind.uses_managed_alt_screen();
    let terminal_diagnostic_session_kind = (target.session_kind.uses_managed_alt_screen()
        || env::var_os("RELAYCAT_TERMINAL_DEBUG").is_some())
    .then(|| target.session_kind.as_str().to_string());
    let input_diagnostic_session_kind = terminal_diagnostic_session_kind.clone();
    let terminal_snapshot_request_coalescer_for_output =
        terminal_snapshot_request_coalescer.clone();
    let relay_output = tokio::spawn(async move {
        let mut transport_seq = 1_u64;
        let app_connected = app_connected;
        let resume_gate = resume_gate;
        let mut ws_writer = Some(ws_writer);
        let mut terminal_core = TerminalCore::new_with_palette(
            TerminalCoreConfig {
                terminal_run_id: terminal_run_id(),
                cols: initial_terminal_cols,
                rows: initial_terminal_rows,
                patch_retention: TERMINAL_V2_PATCH_RETENTION,
            },
            terminal_palette_for_relay,
        );
        terminal_core.set_record_primary_screen_frames(record_primary_screen_frames);
        terminal_core.set_incremental_attrs_flag(incremental_attrs.clone());
        // Sampling process CPU/memory can block: on Windows `collect_cli_status`
        // shells out to PowerShell, whose cold start may take ~1s — longer than a
        // fast Unix `ps`. Run it on a dedicated task and the blocking pool so a
        // slow sample never stalls this relay pump (which also forwards PTY output
        // and input). Each sample is pushed through a 1-slot channel the select
        // loop drains cheaply; the task ends when the loop drops the receiver.
        let (cli_status_tx, mut cli_status_rx) = tokio::sync::mpsc::channel::<CliStatus>(1);
        {
            let child_program_name = child_program_name.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(CLI_STATUS_INTERVAL);
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    interval.tick().await;
                    let program = child_program_name.clone();
                    let Ok(status) = tokio::task::spawn_blocking(move || {
                        collect_cli_status(child_pid, &program)
                    })
                    .await
                    else {
                        continue;
                    };
                    if cli_status_tx.send(status).await.is_err() {
                        break;
                    }
                }
            });
        }
        // Coalesced PTY output awaiting a flush, plus the deadline at which it is
        // turned into a single terminal patch. Snapshots, resizes, and exit must
        // flush this first so the app never sees output reordered around them.
        let mut pending_output: Vec<u8> = Vec::new();
        let mut flush_deadline: Option<Instant> = None;
        let mut deferred_history_thaw = DeferredHistoryThaw::default();
        let mut local_mode_dirty = false;
        let mut relay_output_in_local_mode = false;
        let mut pending_reset_app_cache_snapshot = false;
        let mut pending_cli_metadata = true;

        loop {
            let mut msgs = tokio::select! {
                Some(next_writer) = writer_update_rx.recv() => {
                    if current_pty_work_mode(&pty_work_mode_for_relay_output) == PtyWorkModeKind::Remote {
                        ws_writer = Some(next_writer);
                        pending_cli_metadata = true;
                    }
                    continue;
                }
                Some(control) = terminal_v2_control_rx.recv() => {
                    match control {
                        TerminalV2Control::AppConnected => {
                            pending_cli_metadata = true;
                            Vec::new()
                        }
                        TerminalV2Control::EnterLocalMode => {
                            flush_deadline = None;
                            let mut folded_unsent_output = false;
                            if !pending_output.is_empty() {
                                let bytes = std::mem::take(&mut pending_output);
                                if let Some(patch) = terminal_core.feed_vt_bytes(&bytes) {
                                    folded_unsent_output = true;
                                    if let Some(session_kind) =
                                        terminal_diagnostic_session_kind.as_deref()
                                    {
                                        relaycat_log(
                                            "INFO",
                                            terminal_patch_diagnostic_line(
                                                session_kind,
                                                &bytes,
                                                &terminal_core,
                                                &patch,
                                            ),
                                        );
                                    }
                                }
                            }
                            local_mode_dirty = folded_unsent_output;
                            relay_output_in_local_mode = true;
                            if let Some(mut writer) = ws_writer.take() {
                                let _ = tokio::time::timeout(
                                    WS_SEND_TIMEOUT,
                                    writer.send(Message::Close(None)),
                                )
                                .await;
                            }
                            mark_app_disconnected(&app_connected);
                            relaycat_log("INFO", "mode: entered LocalMode; closed relay websocket");
                            continue;
                        }
                        TerminalV2Control::EnterRemoteMode => {
                            let mut msgs = flush_pending_output(
                                &mut pending_output,
                                &mut flush_deadline,
                                &mut terminal_core,
                                terminal_diagnostic_session_kind.as_deref(),
                            );
                            if local_mode_dirty || terminal_msgs_contain_state_patch(&msgs) {
                                pending_reset_app_cache_snapshot = true;
                                msgs.clear();
                                local_mode_dirty = false;
                            }
                            relay_output_in_local_mode = false;
                            if pending_reset_app_cache_snapshot
                                && app_connected.load(Ordering::Acquire)
                            {
                                let snapshot = terminal_core.snapshot_with_reset_app_cache(true);
                                pending_reset_app_cache_snapshot = false;
                                msgs.push(PlainMsg::TerminalSnapshotV2(snapshot));
                            }
                            msgs
                        }
                        TerminalV2Control::Resume(resume) => {
                            // Fold buffered output into the core so the resume
                            // reply reflects it, but do not emit it as a
                            // standalone patch: resume_messages() already
                            // replays it through the retained patch range.
                            // Emitting it separately would deliver the same
                            // patch twice, which the app rejects as a sequence
                            // gap and recovers from with a full snapshot resync.
                            flush_deadline = None;
                            if !pending_output.is_empty() {
                                let bytes = std::mem::take(&mut pending_output);
                                if let Some(patch) = terminal_core.feed_vt_bytes(&bytes) {
                                    if relay_output_in_local_mode {
                                        local_mode_dirty = true;
                                    }
                                    if let Some(session_kind) =
                                        terminal_diagnostic_session_kind.as_deref()
                                    {
                                        relaycat_log(
                                            "INFO",
                                            terminal_patch_diagnostic_line(
                                                session_kind,
                                                &bytes,
                                                &terminal_core,
                                                &patch,
                                            ),
                                        );
                                    }
                                }
                            }
                            let msgs = if pending_reset_app_cache_snapshot {
                                pending_reset_app_cache_snapshot = false;
                                vec![PlainMsg::TerminalSnapshotV2(
                                    terminal_core.snapshot_with_reset_app_cache(true),
                                )]
                            } else {
                                terminal_core.resume_messages(&resume)
                            };
                            if let Ok(mut gate) = resume_gate.lock() {
                                gate.mark_resume_processed();
                            }
                            msgs
                        }
                        TerminalV2Control::RenderAck(ack) => {
                            // A render ack only arrives while the app is
                            // connected. Keep thawing idempotent for old
                            // reconnect paths.
                            if deferred_history_thaw.take_render_ack_thaw() {
                                terminal_core.thaw_history();
                            }
                            terminal_core.ack_render(ack);
                            continue;
                        }
                        TerminalV2Control::FreezeHistory => {
                            deferred_history_thaw.cancel();
                            terminal_core.freeze_history();
                            continue;
                        }
                        TerminalV2Control::ThawHistory => {
                            deferred_history_thaw.request(Instant::now());
                            continue;
                        }
                        TerminalV2Control::RequestSnapshot(_) => {
                            // Fold buffered output into the core so the snapshot
                            // reflects it, but skip the redundant patch since the
                            // snapshot it would precede already carries the state.
                            flush_deadline = None;
                            if !pending_output.is_empty() {
                                let bytes = std::mem::take(&mut pending_output);
                                if let Some(patch) = terminal_core.feed_vt_bytes(&bytes) {
                                    if relay_output_in_local_mode {
                                        local_mode_dirty = true;
                                    }
                                    if let Some(session_kind) =
                                        terminal_diagnostic_session_kind.as_deref()
                                    {
                                        relaycat_log(
                                            "INFO",
                                            terminal_patch_diagnostic_line(
                                                session_kind,
                                                &bytes,
                                                &terminal_core,
                                                &patch,
                                            ),
                                        );
                                    }
                                }
                            }
                            let snapshot = if pending_reset_app_cache_snapshot {
                                pending_reset_app_cache_snapshot = false;
                                terminal_core.snapshot_with_reset_app_cache(true)
                            } else {
                                terminal_core.snapshot()
                            };
                            if let Some(session_kind) = terminal_diagnostic_session_kind.as_deref() {
                                let core = terminal_core.debug_snapshot();
                                relaycat_log(
                                    "INFO",
                                    format!(
                                        "terminal_snapshot_diag kind={session_kind} snapshot={} seq={} scrollback_window={} screen_rows={} core_history={} vt_scrollback={} frozen={} alt={} size={}x{}",
                                        snapshot.snapshot_id,
                                        snapshot.state_seq,
                                        snapshot.scrollback_window.len(),
                                        snapshot.screen_rows.len(),
                                        core.history_len,
                                        core.vt_scrollback_len,
                                        core.history_frozen,
                                        core.alt_screen,
                                        core.cols,
                                        core.rows,
                                    ),
                                );
                            }
                            terminal_snapshot_request_coalescer_for_output.mark_request_finished();
                            vec![PlainMsg::TerminalSnapshotV2(snapshot)]
                        }
                        TerminalV2Control::RequestTranscript(request) => {
                            vec![PlainMsg::TranscriptChunkV2(
                                terminal_core.transcript_chunk(&request),
                            )]
                        }
                        TerminalV2Control::Resize(event) => {
                            let mut msgs = flush_pending_output(
                                &mut pending_output,
                                &mut flush_deadline,
                                &mut terminal_core,
                                terminal_diagnostic_session_kind.as_deref(),
                            );
                            mark_local_mode_dirty_from_msgs(
                                &msgs,
                                relay_output_in_local_mode,
                                &mut local_mode_dirty,
                            );
                            let reset_app_cache_snapshot = pending_reset_app_cache_snapshot;
                            if reset_app_cache_snapshot {
                                msgs.retain(|msg| !matches!(msg, PlainMsg::TerminalPatchV2(_)));
                                pending_reset_app_cache_snapshot = false;
                            }
                            let (ack, snapshot) =
                                terminal_core.resize_with_reset_app_cache(
                                    event,
                                    reset_app_cache_snapshot,
                                );
                            if let Some(session_kind) = terminal_diagnostic_session_kind.as_deref() {
                                let core = terminal_core.debug_snapshot();
                                relaycat_log(
                                    "INFO",
                                    format!(
                                        "terminal_resize_snapshot_diag kind={session_kind} snapshot={} seq={} scrollback_window={} core_history={} vt_scrollback={} frozen={} alt={} size={}x{}",
                                        snapshot.snapshot_id,
                                        snapshot.state_seq,
                                        snapshot.scrollback_window.len(),
                                        core.history_len,
                                        core.vt_scrollback_len,
                                        core.history_frozen,
                                        core.alt_screen,
                                        core.cols,
                                        core.rows,
                                    ),
                                );
                            }
                            msgs.push(PlainMsg::ResizeAckV2(ack));
                            msgs.push(PlainMsg::TerminalSnapshotV2(snapshot));
                            msgs
                        }
                        TerminalV2Control::LocalResize { cols, rows } => {
                            let msgs = flush_pending_output(
                                &mut pending_output,
                                &mut flush_deadline,
                                &mut terminal_core,
                                terminal_diagnostic_session_kind.as_deref(),
                            );
                            mark_local_mode_dirty_from_msgs(
                                &msgs,
                                relay_output_in_local_mode,
                                &mut local_mode_dirty,
                            );
                            let _ = terminal_core.resize(ResizeEventV2 {
                                resize_seq: 0,
                                cols,
                                rows,
                                input_stream_id: "local".to_string(),
                                last_input_ack: 0,
                            });
                            msgs
                        }
                    }
                }
                Some(status) = cli_status_rx.recv() => {
                    vec![PlainMsg::CliStatus(status)]
                }
                _ = sleep_until_opt(flush_deadline), if flush_deadline.is_some() => {
                    let msgs = flush_pending_output(
                        &mut pending_output,
                        &mut flush_deadline,
                        &mut terminal_core,
                        terminal_diagnostic_session_kind.as_deref(),
                    );
                    mark_local_mode_dirty_from_msgs(
                        &msgs,
                        relay_output_in_local_mode,
                        &mut local_mode_dirty,
                    );
                    msgs
                }
                _ = sleep_until_opt(deferred_history_thaw.deadline()), if deferred_history_thaw.deadline().is_some() => {
                    if deferred_history_thaw.take_due(Instant::now()) {
                        let msgs = flush_pending_output(
                            &mut pending_output,
                            &mut flush_deadline,
                            &mut terminal_core,
                            terminal_diagnostic_session_kind.as_deref(),
                        );
                        mark_local_mode_dirty_from_msgs(
                            &msgs,
                            relay_output_in_local_mode,
                            &mut local_mode_dirty,
                        );
                        terminal_core.thaw_history();
                        msgs
                    } else {
                        continue;
                    }
                }
                Some(event) = output_rx.recv() => {
                    match event {
                        PtyEvent::Plain(msg) => vec![msg],
                        PtyEvent::Output(bytes) => {
                            pending_output.extend_from_slice(&bytes);
                            deferred_history_thaw.observe_pty_output(Instant::now());
                            if flush_deadline.is_none() {
                                flush_deadline = Some(Instant::now() + PTY_OUTPUT_COALESCE_WINDOW);
                            }
                            if pending_output.len() >= PTY_OUTPUT_COALESCE_MAX_BYTES {
                                let msgs = flush_pending_output(
                                    &mut pending_output,
                                    &mut flush_deadline,
                                    &mut terminal_core,
                                    terminal_diagnostic_session_kind.as_deref(),
                                );
                                mark_local_mode_dirty_from_msgs(
                                    &msgs,
                                    relay_output_in_local_mode,
                                    &mut local_mode_dirty,
                                );
                                msgs
                            } else {
                                continue;
                            }
                        }
                        PtyEvent::Exit(status) => {
                            let mut msgs = flush_pending_output(
                                &mut pending_output,
                                &mut flush_deadline,
                                &mut terminal_core,
                                terminal_diagnostic_session_kind.as_deref(),
                            );
                            mark_local_mode_dirty_from_msgs(
                                &msgs,
                                relay_output_in_local_mode,
                                &mut local_mode_dirty,
                            );
                            msgs.push(process_exit_msg(&status));
                            msgs
                        },
                    }
                }
                else => break,
            };

            if pending_reset_app_cache_snapshot {
                msgs.retain(|msg| !matches!(msg, PlainMsg::TerminalPatchV2(_)));
            }
            let sending_cli_metadata =
                pending_cli_metadata && app_connected.load(Ordering::Acquire);
            if sending_cli_metadata {
                msgs.insert(0, PlainMsg::CliMetadata(cli_metadata.clone()));
            }

            let is_exit = msgs
                .iter()
                .any(|msg| matches!(msg, PlainMsg::ProcessExit { .. }));
            let app_is_connected = app_connected.load(Ordering::Acquire);
            let can_send_terminal_state = resume_gate
                .lock()
                .map(|gate| gate.can_send_terminal_state(app_is_connected))
                .unwrap_or(false);
            if is_exit || app_is_connected {
                // Set when an oversized patch has been replaced by a snapshot:
                // that snapshot already carries the state of every remaining
                // patch in the batch (and started a new snapshot_id), so those
                // patches must be dropped instead of confusing the app.
                let mut substituted_snapshot = false;
                for msg in msgs.drain(..) {
                    if !is_exit
                        && !can_send_terminal_state
                        && !can_send_without_terminal_resume(&msg)
                    {
                        continue;
                    }
                    let msg = if !plain_msg_fits_relay_budget(&msg)? {
                        if !matches!(&msg, PlainMsg::TerminalPatchV2(_)) {
                            relaycat_log(
                                "WARN",
                                format!(
                                    "skipping oversized outbound message type={}",
                                    String::from_utf8_lossy(plain_msg_type(&msg)),
                                ),
                            );
                            continue;
                        }
                        // Dropping a patch silently would strand the app on an
                        // old state_seq until the next patch exposes the gap.
                        // A snapshot is always budget-trimmed, so send one in
                        // its place.
                        relaycat_log("WARN", "oversized terminal patch; sending snapshot instead");
                        substituted_snapshot = true;
                        let snapshot = PlainMsg::TerminalSnapshotV2(terminal_core.snapshot());
                        if !plain_msg_fits_relay_budget(&snapshot)? {
                            relaycat_log("WARN", "skipping oversized snapshot substitute");
                            continue;
                        }
                        snapshot
                    } else if substituted_snapshot && matches!(&msg, PlainMsg::TerminalPatchV2(_)) {
                        continue;
                    } else {
                        msg
                    };
                    let is_cli_metadata = matches!(&msg, PlainMsg::CliMetadata(_));
                    let frame = encode_output(msg, transport_seq)?;
                    if let Some(writer) = ws_writer.as_mut() {
                        if let Err(err) =
                            ws_send(writer, Message::Binary(encode_frame(&frame)?.into())).await
                        {
                            relaycat_log("WARN", format!("relay send failed: {err:#}"));
                            ws_writer = None;
                            mark_app_disconnected(&app_connected);
                            if let Ok(mut gate) = resume_gate.lock() {
                                gate.mark_app_disconnected();
                            }
                            status_bar_for_relay_output.set_hint(DISCONNECT_TITLE_HINT);
                            if current_pty_work_mode(&pty_work_mode_for_relay_output)
                                == PtyWorkModeKind::Remote
                            {
                                let _ = reconnect_signal_tx_for_output.send(());
                            }
                        } else {
                            transport_seq += 1;
                            if is_cli_metadata {
                                pending_cli_metadata = false;
                            }
                        }
                    }
                }
            }
            if is_exit {
                break;
            }
        }

        anyhow::Ok(())
    });

    let relay_input = tokio::spawn(async move {
        let mut input_dedupe = InputDedupe::default();
        let terminal_snapshot_request_coalescer_for_input = terminal_snapshot_request_coalescer;
        let mut ws_reader = Some(ws_reader);
        let mut last_seen_join_gen = app_join_generation_for_input.load(Ordering::Acquire);
        let mut consecutive_decode_errors: u32 = 0;

        enum RelayInputEvent {
            ReconnectSignal(Option<()>),
            Message(Option<Result<Message, tokio_tungstenite::tungstenite::Error>>),
            ReadIdleTimeout,
        }

        let mut last_inbound_at = tokio::time::Instant::now();
        loop {
            if ws_reader.is_none() {
                let Some(()) = reconnect_signal_rx.recv().await else {
                    break;
                };
                drain_reconnect_signals(&mut reconnect_signal_rx);
                if current_pty_work_mode(&pty_work_mode_for_relay_input) != PtyWorkModeKind::Remote
                {
                    continue;
                }
                mark_app_disconnected(&app_connected_for_input);
                if let Ok(mut gate) = resume_gate_for_input.lock() {
                    gate.mark_app_disconnected();
                }
                status_bar_for_relay_input.set_hint(DISCONNECT_TITLE_HINT);
                let next_reader =
                    reconnect_relay_transport(&reconnect_for_input, &writer_update_tx_for_input)
                        .await?;
                preserve_sessions_on_transport_reconnect(&mut on_transport_reconnected)?;
                mark_relay_registered_after_mode_switch(&status_bar_for_relay_input);
                ws_reader = Some(next_reader);
                last_inbound_at = tokio::time::Instant::now();
                drain_reconnect_signals(&mut reconnect_signal_rx);
                continue;
            }

            let event = {
                let reader = ws_reader.as_mut().expect("reader is present");
                tokio::select! {
                    signal = reconnect_signal_rx.recv() => RelayInputEvent::ReconnectSignal(signal),
                    message = reader.next() => RelayInputEvent::Message(message),
                    _ = tokio::time::sleep_until(last_inbound_at + WS_READ_IDLE_TIMEOUT) => {
                        RelayInputEvent::ReadIdleTimeout
                    }
                }
            };

            let message = match event {
                RelayInputEvent::ReadIdleTimeout => {
                    last_inbound_at = tokio::time::Instant::now();
                    if current_pty_work_mode(&pty_work_mode_for_relay_input)
                        != PtyWorkModeKind::Remote
                    {
                        continue;
                    }
                    relaycat_log(
                        "WARN",
                        format!(
                            "no relay traffic for {}s; reconnecting transport",
                            WS_READ_IDLE_TIMEOUT.as_secs()
                        ),
                    );
                    drain_reconnect_signals(&mut reconnect_signal_rx);
                    mark_app_disconnected(&app_connected_for_input);
                    if let Ok(mut gate) = resume_gate_for_input.lock() {
                        gate.mark_app_disconnected();
                    }
                    status_bar_for_relay_input.set_hint(DISCONNECT_TITLE_HINT);
                    let next_reader = reconnect_relay_transport(
                        &reconnect_for_input,
                        &writer_update_tx_for_input,
                    )
                    .await?;
                    // Unexpected transport drop (see ReconnectSignal arm).
                    complete_transport_reconnect(
                        &app_connected_for_input,
                        &resume_gate_for_input,
                        &mut on_transport_reconnected,
                    )?;
                    let _ = terminal_v2_control_tx_for_input.send(TerminalV2Control::AppConnected);
                    status_bar_for_relay_input.clear_hint();
                    ws_reader = Some(next_reader);
                    last_inbound_at = tokio::time::Instant::now();
                    drain_reconnect_signals(&mut reconnect_signal_rx);
                    continue;
                }
                RelayInputEvent::ReconnectSignal(signal) => {
                    if signal.is_none() {
                        break;
                    }
                    drain_reconnect_signals(&mut reconnect_signal_rx);
                    if current_pty_work_mode(&pty_work_mode_for_relay_input)
                        != PtyWorkModeKind::Remote
                    {
                        continue;
                    }
                    mark_app_disconnected(&app_connected_for_input);
                    if let Ok(mut gate) = resume_gate_for_input.lock() {
                        gate.mark_app_disconnected();
                    }
                    status_bar_for_relay_input.set_hint(DISCONNECT_TITLE_HINT);
                    let next_reader = reconnect_relay_transport(
                        &reconnect_for_input,
                        &writer_update_tx_for_input,
                    )
                    .await?;
                    // Unexpected transport drop (not a Ctrl-G mode switch): the
                    // app may have stayed in the room and only seen
                    // PeerJoined(cli), in which case no PeerJoined(app) comes
                    // back to us. Mark connected eagerly so the output task
                    // resumes, but keep the secure session and sequence
                    // counters intact.
                    complete_transport_reconnect(
                        &app_connected_for_input,
                        &resume_gate_for_input,
                        &mut on_transport_reconnected,
                    )?;
                    let _ = terminal_v2_control_tx_for_input.send(TerminalV2Control::AppConnected);
                    status_bar_for_relay_input.clear_hint();
                    ws_reader = Some(next_reader);
                    last_inbound_at = tokio::time::Instant::now();
                    drain_reconnect_signals(&mut reconnect_signal_rx);
                    continue;
                }
                RelayInputEvent::Message(message) => {
                    // Any inbound traffic (including relay pings) proves the
                    // link is alive.
                    last_inbound_at = tokio::time::Instant::now();
                    message
                }
            };

            let Some(message) = message else {
                drain_reconnect_signals(&mut reconnect_signal_rx);
                mark_app_disconnected(&app_connected_for_input);
                if let Ok(mut gate) = resume_gate_for_input.lock() {
                    gate.mark_app_disconnected();
                }
                ws_reader = None;
                if current_pty_work_mode(&pty_work_mode_for_relay_input) == PtyWorkModeKind::Remote
                {
                    status_bar_for_relay_input.set_hint(DISCONNECT_TITLE_HINT);
                    let next_reader = reconnect_relay_transport(
                        &reconnect_for_input,
                        &writer_update_tx_for_input,
                    )
                    .await?;
                    // Unexpected transport drop (see ReconnectSignal arm).
                    complete_transport_reconnect(
                        &app_connected_for_input,
                        &resume_gate_for_input,
                        &mut on_transport_reconnected,
                    )?;
                    let _ = terminal_v2_control_tx_for_input.send(TerminalV2Control::AppConnected);
                    status_bar_for_relay_input.clear_hint();
                    ws_reader = Some(next_reader);
                    last_inbound_at = tokio::time::Instant::now();
                    drain_reconnect_signals(&mut reconnect_signal_rx);
                }
                continue;
            };
            let message = match message {
                Ok(message) => message,
                Err(err) => {
                    relaycat_log("WARN", format!("websocket receive failed: {err:#}"));
                    drain_reconnect_signals(&mut reconnect_signal_rx);
                    mark_app_disconnected(&app_connected_for_input);
                    if let Ok(mut gate) = resume_gate_for_input.lock() {
                        gate.mark_app_disconnected();
                    }
                    ws_reader = None;
                    if current_pty_work_mode(&pty_work_mode_for_relay_input)
                        == PtyWorkModeKind::Remote
                    {
                        status_bar_for_relay_input.set_hint(DISCONNECT_TITLE_HINT);
                        let next_reader = reconnect_relay_transport(
                            &reconnect_for_input,
                            &writer_update_tx_for_input,
                        )
                        .await?;
                        // Unexpected transport drop (see ReconnectSignal arm).
                        complete_transport_reconnect(
                            &app_connected_for_input,
                            &resume_gate_for_input,
                            &mut on_transport_reconnected,
                        )?;
                        let _ =
                            terminal_v2_control_tx_for_input.send(TerminalV2Control::AppConnected);
                        status_bar_for_relay_input.clear_hint();
                        ws_reader = Some(next_reader);
                        last_inbound_at = tokio::time::Instant::now();
                        drain_reconnect_signals(&mut reconnect_signal_rx);
                    }
                    continue;
                }
            };
            let Message::Binary(bytes) = message else {
                continue;
            };
            let frame = match decode_frame(&bytes) {
                Ok(f) => f,
                Err(err) => {
                    relaycat_log("WARN", format!("skipping malformed relay frame: {err}"));
                    continue;
                }
            };
            let was_connected = app_connected_for_input.load(Ordering::Acquire);
            let decoded = match decode_input(frame) {
                Ok(v) => {
                    consecutive_decode_errors = 0;
                    v
                }
                Err(err) => {
                    consecutive_decode_errors += 1;
                    relaycat_log(
                        "WARN",
                        format!(
                            "skipping relay frame decode error ({consecutive_decode_errors}): {err}"
                        ),
                    );
                    // A run of decode failures means the current websocket is
                    // no longer delivering a decodable stream. Tolerate the
                    // occasional bad frame, but once they pile up renew the
                    // transport while preserving secure sequence counters.
                    if consecutive_decode_errors >= MAX_CONSECUTIVE_SECURE_DECODE_ERRORS {
                        consecutive_decode_errors = 0;
                        relaycat_log(
                            "WARN",
                            "secure input stream desynced; forcing transport reconnect",
                        );
                        drain_reconnect_signals(&mut reconnect_signal_rx);
                        mark_app_disconnected(&app_connected_for_input);
                        if let Ok(mut gate) = resume_gate_for_input.lock() {
                            gate.mark_app_disconnected();
                        }
                        ws_reader = None;
                        if current_pty_work_mode(&pty_work_mode_for_relay_input)
                            == PtyWorkModeKind::Remote
                        {
                            status_bar_for_relay_input.set_hint(DISCONNECT_TITLE_HINT);
                            let next_reader = reconnect_relay_transport(
                                &reconnect_for_input,
                                &writer_update_tx_for_input,
                            )
                            .await?;
                            complete_transport_reconnect(
                                &app_connected_for_input,
                                &resume_gate_for_input,
                                &mut on_transport_reconnected,
                            )?;
                            let _ = terminal_v2_control_tx_for_input
                                .send(TerminalV2Control::AppConnected);
                            status_bar_for_relay_input.clear_hint();
                            ws_reader = Some(next_reader);
                            drain_reconnect_signals(&mut reconnect_signal_rx);
                        }
                    }
                    continue;
                }
            };
            let now_connected = app_connected_for_input.load(Ordering::Acquire);
            if was_connected && !now_connected {
                if let Ok(mut mode) = pty_work_mode_for_input.lock() {
                    let _ = mode.observe_app_disconnected();
                }
            }
            // When a new app joins the relay room (PeerJoined increments the
            // generation counter in the decode handler), trigger AppConnected
            // so the output task re-sends CliMetadata.  This handles both the
            // reconnect case (was_connected=false→true) AND the case where a
            // second app scans while the first is still connected (stays true).
            let current_join_gen = app_join_generation_for_input.load(Ordering::Acquire);
            if current_join_gen != last_seen_join_gen {
                last_seen_join_gen = current_join_gen;
                if now_connected {
                    // The app has actually rejoined (fresh handshake done in
                    // the decode handler). Clear the disconnect hint and ask
                    // the output task to re-send CliMetadata + a snapshot.
                    status_bar_for_relay_input.clear_hint();
                    let _ = terminal_v2_control_tx_for_input.send(TerminalV2Control::AppConnected);
                }
            }
            match decoded.map(relay_input_action) {
                Some(RelayInputAction::InputEventV2 {
                    input_stream_id,
                    input_seq,
                    bytes,
                }) => {
                    let decision = input_dedupe.observe(&input_stream_id, input_seq);
                    if let Some(session_kind) = input_diagnostic_session_kind.as_deref() {
                        relaycat_log(
                            "INFO",
                            terminal_input_diagnostic_line(
                                session_kind,
                                &input_stream_id,
                                input_seq,
                                &bytes,
                                decision,
                            ),
                        );
                    }
                    if decision != InputDecision::Duplicate {
                        let bytes = if rewrite_app_wheel_input {
                            let encoding = mouse_report_modes_for_relay_input
                                .lock()
                                .map(|modes| modes.wheel_encoding())
                                .unwrap_or(WheelEncoding::Sgr);
                            let rewritten = rewrite_app_wheel_reports(&bytes, encoding);
                            if rewritten.reports > 0 {
                                relaycat_log(
                                    "INFO",
                                    format!(
                                        "terminal_input_wheel_rewrite encoding={encoding:?} reports={} bytes={}",
                                        rewritten.reports,
                                        rewritten.bytes.len()
                                    ),
                                );
                            }
                            rewritten.bytes
                        } else {
                            bytes
                        };
                        let mut pty_writer = pty_writer_for_relay
                            .lock()
                            .map_err(|_| anyhow::anyhow!("pty writer lock poisoned"))?;
                        pty_writer
                            .write_all(&bytes)
                            .context("failed to write terminal input")?;
                        pty_writer
                            .flush()
                            .context("failed to flush terminal input")?;
                    }
                    let _ = heartbeat_output_tx_for_input.try_send(PtyEvent::Plain(
                        PlainMsg::InputAckV2(InputAckV2 {
                            highest_contiguous_input_seq: input_dedupe
                                .highest_contiguous_input_seq(),
                        }),
                    ));
                }
                Some(RelayInputAction::ResumeV2(resume)) => {
                    input_dedupe.synchronize_ack(&resume.input_stream_id, resume.last_input_ack);
                    let _ =
                        terminal_v2_control_tx_for_input.send(TerminalV2Control::Resume(resume));
                }
                Some(RelayInputAction::RenderAckV2(ack)) => {
                    let _ =
                        terminal_v2_control_tx_for_input.send(TerminalV2Control::RenderAck(ack));
                }
                Some(RelayInputAction::RequestSnapshotV2(request)) => {
                    if terminal_snapshot_request_coalescer_for_input.observe(&request)
                        && terminal_v2_control_tx_for_input
                            .send(TerminalV2Control::RequestSnapshot(request))
                            .is_err()
                    {
                        terminal_snapshot_request_coalescer_for_input.mark_request_finished();
                    }
                }
                Some(RelayInputAction::RequestTranscriptV2(request)) => {
                    let _ = terminal_v2_control_tx_for_input
                        .send(TerminalV2Control::RequestTranscript(request));
                }
                Some(RelayInputAction::ResizeEventV2(event)) => {
                    input_dedupe.synchronize_ack(&event.input_stream_id, event.last_input_ack);
                    // Clamp the app size to the host (the desktop window on the
                    // Windows GUI pipe bridge, a real terminal elsewhere) so the
                    // child PTY is sized to `min(app, host)`. Both the desktop
                    // and the phone then render the same layout, and
                    // bottom-anchored TUIs stay visible on both sides.
                    let (effective_cols, effective_rows) = effective_remote_size(
                        effective_remote_resize_cols(event.cols),
                        event.rows,
                        current_terminal_size(),
                    );
                    let (work_mode_kind, remote_size) = pty_work_mode_for_input
                        .lock()
                        .map(|mode| (mode.current(), mode.remote_size()))
                        .unwrap_or((PtyWorkModeKind::Remote, None));
                    relaycat_log(
                        "INFO",
                        format!(
                            "size: app reported {}x{} (cols x rows); effective {}x{}; mode={:?} remote_size={:?}",
                            event.cols,
                            event.rows,
                            effective_cols,
                            effective_rows,
                            work_mode_kind,
                            remote_size
                        ),
                    );
                    let app_size = (event.cols, event.rows);
                    if let Some(size) = remote_resize_pty_size(effective_cols, effective_rows) {
                        // Cache the phone's reported size (not the PTY size) so a
                        // later Ctrl-G re-entry can re-clamp it against the
                        // current host size.
                        let should_resize = pty_work_mode_for_input
                            .lock()
                            .map(|mut mode| pty_work_mode_observe_app_resize(&mut mode, app_size))
                            .unwrap_or(false);
                        if should_resize {
                            render_terminal_chrome_for_status_bar(
                                PtyWorkModeKind::Remote,
                                &status_bar_for_resize,
                                size,
                                resize_title_context.as_ref(),
                            );
                            let _ = terminal_v2_control_tx_for_input
                                .send(TerminalV2Control::FreezeHistory);
                            if clear_host_on_remote_clamp
                                && work_mode_kind == PtyWorkModeKind::Remote
                            {
                                clear_host_terminal_for_remote_clamp(
                                    effective_cols,
                                    effective_rows,
                                    current_terminal_size(),
                                );
                            }
                            if let Ok(guard) = master_for_resize.lock()
                                && let Some(m) = guard.as_ref()
                            {
                                let _ = m.resize(size);
                            }
                            // Tell the GUI the negotiated grid so it can
                            // letterbox the desktop window to match the phone.
                            emit_remote_size_for_gui(effective_cols, effective_rows);
                            pty_viewport_rows_for_relay_input
                                .store(effective_rows, Ordering::Release);
                            // The app-facing model mirrors the PTY so the desktop
                            // and the phone stay in lockstep.
                            let (model_cols, model_rows) =
                                remote_model_size((effective_cols, effective_rows));
                            relaycat_log(
                                "INFO",
                                format!(
                                    "size: applied PTY resize to {}x{}; model {}x{} (cols x rows)",
                                    effective_cols, effective_rows, model_cols, model_rows
                                ),
                            );
                            let _ = terminal_v2_control_tx_for_input.send(
                                TerminalV2Control::Resize(ResizeEventV2 {
                                    cols: model_cols,
                                    rows: model_rows,
                                    ..event
                                }),
                            );
                            let _ = terminal_v2_control_tx_for_input
                                .send(TerminalV2Control::ThawHistory);
                        } else {
                            relaycat_log(
                                "INFO",
                                format!(
                                    "size: ignored app {}x{} (RemoteMode size already established)",
                                    effective_cols, effective_rows
                                ),
                            );
                        }
                    }
                }
                Some(RelayInputAction::HelloV2(hello)) => {
                    // Outbound (CLI->App) compression is enabled in the decode
                    // path the moment the Hello is read; here we just answer
                    // with the negotiated capability set so the app can enable
                    // its own uplink compression.
                    let _ = heartbeat_output_tx_for_input
                        .try_send(PtyEvent::Plain(PlainMsg::HelloAckV2(hello_ack_for(&hello))));
                }
                Some(RelayInputAction::EchoHeartbeat) => {
                    let _ = heartbeat_output_tx_for_input
                        .try_send(PtyEvent::Plain(PlainMsg::Heartbeat));
                }
                _ => {}
            }
        }

        anyhow::Ok(())
    });

    // block_in_place lets the tokio runtime park other tasks on this thread
    // while we block — prevents starving async tasks during a long child wait.
    let status =
        tokio::task::block_in_place(|| child.wait()).context("child process wait failed")?;
    relaycat_log(
        "INFO",
        format!(
            "child process {} exited: {status}",
            child_program_name_for_exit
        ),
    );
    // Reap any descendant still attached to the PTY slave (e.g. a command the
    // user tried to interrupt before exiting). The child is a session leader so
    // its pid doubles as the process-group id; without this a lingering process
    // keeps the slave open, the master reader never reaches EOF, and the join
    // below would block forever, stranding the local terminal in raw mode.
    kill_child_process_group(child_pid);
    if let Ok(mut guard) = master.lock() {
        guard.take();
    }
    join_pty_output_thread(output_thread);
    let _ = output_tx.send(PtyEvent::Exit(status)).await;
    drop(output_tx);
    // Give relay_output a short window to flush the exit message, but do not
    // wait forever: a stalled TCP / WebSocket write would block here indefinitely
    // and make the process unkillable.
    let output_result = tokio::time::timeout(Duration::from_secs(5), relay_output).await;
    relay_input.abort();
    local_size_task.abort();
    chrome_refresh_task.abort();
    drop(pty_writer);
    status_bar.clear_hint();
    clear_terminal_chrome();
    match output_result {
        Ok(join_result) => join_result.context("relay output task panicked")??,
        Err(_timeout) => {} // stalled write — exit cleanly anyway
    }

    Ok(())
}

/// Send `SIGKILL` to the child's entire process group so descendant processes
/// still attached to the PTY slave are reaped. The child is spawned as a
/// session leader (portable-pty calls `setsid`), so `child_pid` is also the
/// process-group id and `kill(-pid, …)` reaches the whole group. A signal to an
/// already-exited group returns `ESRCH`, which we ignore. No-op on non-Unix.
#[cfg(unix)]
pub(crate) fn kill_child_process_group(child_pid: Option<u32>) {
    let Some(pid) = child_pid.and_then(|pid| i32::try_from(pid).ok()) else {
        return;
    };
    // SAFETY: sending a signal to a process group is memory-safe; the result is
    // intentionally ignored (the group may already be gone).
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
pub(crate) fn kill_child_process_group(_child_pid: Option<u32>) {}

/// Build the `(program, args)` used to launch the inner tool inside the ConPTY
/// on Windows. Batch-file shims (`.cmd`/`.bat`, e.g. npm-installed codex /
/// opencode) are wrapped in `cmd.exe /d /c …` because `CreateProcessW` cannot
/// execute them directly; real `.exe`s (PowerShell, Volta shims) are returned
/// unchanged so their behavior is unaffected.
#[cfg(windows)]
fn windows_spawn_command(program: &str, args: &[String]) -> (String, Vec<String>) {
    if windows_program_is_batch_shim(program) {
        let comspec = std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".to_string());
        return windows_cmd_wrapped(&comspec, program, args);
    }
    (program.to_string(), args.to_vec())
}

/// Whether launching `program` by name on Windows resolves to a `.cmd`/`.bat`
/// shim that must be run through `cmd.exe`.
#[cfg(windows)]
fn windows_program_is_batch_shim(program: &str) -> bool {
    resolve_windows_program(program)
        .as_deref()
        .and_then(std::path::Path::extension)
        .and_then(|ext| ext.to_str())
        .is_some_and(extension_needs_cmd_wrapper)
}

/// Resolve `program` to an on-disk file the way Windows does when launching it
/// by name: an explicit path is used verbatim, otherwise each `PATH` entry is
/// probed for the bare name and then with each `PATHEXT` extension.
#[cfg(windows)]
fn resolve_windows_program(program: &str) -> Option<std::path::PathBuf> {
    use std::path::Path;
    let raw = Path::new(program);
    if program.contains('\\') || program.contains('/') {
        return raw.is_file().then(|| raw.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    for dir in std::env::split_paths(&path) {
        let exact = dir.join(program);
        if exact.extension().is_some() && exact.is_file() {
            return Some(exact);
        }
        for ext in pathext.split(';').map(str::trim).filter(|e| !e.is_empty()) {
            let candidate = dir
                .join(program)
                .with_extension(ext.trim_start_matches('.'));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Whether a resolved program's extension is a batch shim that must run via
/// `cmd.exe`. Pure helper, unit-tested off Windows.
#[cfg(any(windows, test))]
fn extension_needs_cmd_wrapper(extension: &str) -> bool {
    extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
}

/// Wrap `program` + `args` as a `cmd.exe /d /c <program> <args…>` invocation.
/// `cmd.exe` re-resolves `program` against `PATH`/`PATHEXT` and runs the shim
/// correctly. Pure helper, unit-tested off Windows.
#[cfg(any(windows, test))]
fn windows_cmd_wrapped(comspec: &str, program: &str, args: &[String]) -> (String, Vec<String>) {
    let mut wrapped = Vec::with_capacity(args.len() + 3);
    wrapped.push("/d".to_string());
    wrapped.push("/c".to_string());
    wrapped.push(program.to_string());
    wrapped.extend_from_slice(args);
    (comspec.to_string(), wrapped)
}

/// Join the PTY output-reader thread without ever blocking the caller forever.
/// If the thread is still stuck in a `read` on the PTY master after a short
/// grace period (e.g. a descendant kept the slave open despite the
/// process-group kill), it is detached so the caller can return and restore the
/// local terminal mode. The detached thread exits once the read finally ends or
/// the process does.
pub(crate) fn join_pty_output_thread(handle: thread::JoinHandle<io::Result<()>>) {
    if !join_pty_output_thread_within(handle, Duration::from_secs(2)) {
        relaycat_log(
            "WARN",
            "PTY output thread still blocked after child exit; detaching to restore local terminal",
        );
    }
}

/// Returns `true` if the thread finished and was joined within `grace`,
/// `false` if it was still running and got detached.
fn join_pty_output_thread_within(
    handle: thread::JoinHandle<io::Result<()>>,
    grace: Duration,
) -> bool {
    let deadline = Instant::now() + grace;
    loop {
        if handle.is_finished() {
            if let Err(e) = handle.join() {
                relaycat_log("ERROR", format!("PTY output thread panicked: {e:?}"));
            }
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn terminal_run_id() -> String {
    const ALPHABET: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let mut bytes = [0_u8; 4];
    OsRng.fill_bytes(&mut bytes);
    bytes
        .iter()
        .map(|byte| ALPHABET[usize::from(*byte) % ALPHABET.len()] as char)
        .collect()
}

fn remote_resize_pty_size(cols: u16, rows: u16) -> Option<PtySize> {
    if cols < MIN_REMOTE_RESIZE_COLS || rows < MIN_REMOTE_RESIZE_ROWS {
        return None;
    }
    Some(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })
}

fn effective_remote_resize_cols(cols: u16) -> u16 {
    cols
}

/// The size given to the app-facing semantic model (`terminal_core`). It always
/// mirrors the child PTY size: the PTY is already clamped to `min(app, host)`
/// (see [`effective_remote_size`]), so the desktop and the phone render an
/// identical layout — required for full-screen TUIs whose absolute cursor
/// positioning would desync if the model and PTY disagreed.
fn remote_model_size(pty: (u16, u16)) -> (u16, u16) {
    pty
}

/// Derive the child PTY size from the app-reported size by clamping it to the
/// host (desktop window on the Windows GUI pipe bridge, real terminal
/// elsewhere). Both the desktop and the phone then see `min(app, host)`, so
/// what the desktop shows is exactly what the phone shows.
fn effective_remote_size(cols: u16, rows: u16, host: Option<PtySize>) -> (u16, u16) {
    clamp_remote_size_to_host(cols, rows, host)
}

/// Clamp an app-reported terminal size to the host terminal's size so the PTY
/// (and the V2 semantic model that mirrors it) never exceed what the host CLI
/// can actually display.
///
/// A full-screen TUI such as codex anchors its status line and input box to the
/// bottom row of the terminal it is told about. relaycat sizes the PTY to the
/// app's reported size, so when the app's viewport is taller (or wider) than
/// the host terminal, codex draws that bottom UI below the host's last row.
/// Absolute cursor moves past the host's last row are clamped by the terminal,
/// so the input box and everything under "Working" become invisible on the host
/// while the app (which renders at the full PTY size) still shows them.
///
/// Taking the component-wise minimum mirrors tmux's "smallest client wins"
/// rule for shared sessions and guarantees the host can render the whole
/// viewport. `host` is the live host terminal size, or `None` when the host has
/// no controlling terminal (headless), in which case the app size is used
/// unchanged.
fn clamp_remote_size_to_host(cols: u16, rows: u16, host: Option<PtySize>) -> (u16, u16) {
    match host {
        Some(host) if host.cols > 0 && host.rows > 0 => (cols.min(host.cols), rows.min(host.rows)),
        _ => (cols, rows),
    }
}

#[cfg(test)]
mod tests;
