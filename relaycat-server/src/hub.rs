use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use dashmap::DashMap;
use relaycat_protocol::{OuterFrame, RelayErrorCode, Role};
use thiserror::Error;
use tokio::sync::mpsc;

use crate::room::{ConnId, RoomError};

pub type OutboundTx = mpsc::Sender<OuterFrame>;
pub type OutboundRx = mpsc::Receiver<OuterFrame>;
pub type CloseSignalTx = mpsc::Sender<CloseSignal>;
pub type CloseSignalRx = mpsc::Receiver<CloseSignal>;

/// Out-of-band eviction notice delivered on a dedicated channel so the
/// websocket task can send a proper Close frame with a reason even when the
/// ordinary outbound queue is full.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseSignal {
    /// The peer's outbound queue overflowed; it should reconnect.
    SlowConsumer,
    /// A newer connection with the same role displaced this one.
    Replaced,
    /// Another device took over the session; the client must not auto-retry.
    Evicted,
}

impl CloseSignal {
    pub fn close_reason(self) -> &'static str {
        match self {
            CloseSignal::SlowConsumer => "slow_consumer retryable",
            CloseSignal::Replaced => "replaced retryable",
            CloseSignal::Evicted => "evicted",
        }
    }
}

#[derive(Debug)]
pub struct JoinRequest {
    pub room_id: String,
    pub role: Role,
    pub conn_id: ConnId,
    pub device_pubkey: [u8; 32],
    pub pairing_token_proof: Option<[u8; 32]>,
    pub relay_admission: Option<[u8; 32]>,
    /// The joiner's per-connection salt, forwarded verbatim to the peer in
    /// `PeerJoined` so both ends can derive a unique key for this connection.
    pub connection_salt: Option<[u8; 32]>,
    pub outbound: OutboundTx,
    pub close_signal: Option<CloseSignalTx>,
}

impl JoinRequest {
    pub fn new(
        room_id: impl Into<String>,
        role: Role,
        conn_id: ConnId,
        device_pubkey: [u8; 32],
        pairing_token_proof: Option<[u8; 32]>,
        outbound: OutboundTx,
    ) -> Self {
        Self {
            room_id: room_id.into(),
            role,
            conn_id,
            device_pubkey,
            pairing_token_proof,
            relay_admission: None,
            connection_salt: None,
            outbound,
            close_signal: None,
        }
    }

    pub fn with_relay_admission(mut self, relay_admission: [u8; 32]) -> Self {
        self.relay_admission = Some(relay_admission);
        self
    }

    pub fn with_connection_salt(mut self, connection_salt: Option<[u8; 32]>) -> Self {
        self.connection_salt = connection_salt;
        self
    }

    pub fn with_close_signal(mut self, close_signal: CloseSignalTx) -> Self {
        self.close_signal = Some(close_signal);
        self
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum HubError {
    #[error(transparent)]
    Room(#[from] RoomError),
    #[error("{role} not registered")]
    PeerMissing { role: &'static str },
    #[error("relay admission rejected")]
    AdmissionRejected,
    #[error("join notification undeliverable")]
    JoinNotificationFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionCheck {
    Authorized,
    Unauthorized,
    RoomMissing,
    RoomUnprotected,
}

#[derive(Debug, Default)]
pub struct Hub {
    rooms: DashMap<String, HubRoom>,
    metrics: HubMetrics,
}

/// Slow-consumer observability counters, accumulated for the lifetime of the
/// relay process and reported through `HubStats`.
#[derive(Debug, Default)]
struct HubMetrics {
    slow_consumer_evictions_total: AtomicU64,
    slow_consumer_evictions_cli: AtomicU64,
    slow_consumer_evictions_app: AtomicU64,
    slow_consumer_discarded_bytes: AtomicU64,
    outbound_queue_high_water: AtomicU64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HubStats {
    pub rooms_total: usize,
    pub cli_connected_total: usize,
    pub app_connected_total: usize,
    pub cli_paired: usize,
    pub cli_idle: usize,
    pub app_paired: usize,
    pub app_without_cli: usize,
    pub slow_consumer_evictions_total: u64,
    pub slow_consumer_evictions_cli: u64,
    pub slow_consumer_evictions_app: u64,
    pub slow_consumer_discarded_bytes: u64,
    pub outbound_queue_high_water: u64,
}

impl Hub {
    /// Returns `Ok(true)` when an existing App peer was evicted to make room
    /// for the new connection, `Ok(false)` otherwise.
    pub fn join(&self, request: JoinRequest) -> Result<bool, HubError> {
        // Acquire the entry write-lock first, then check for CLI presence.
        // Doing a separate get() read-lock followed by entry() write-lock
        // creates a window where CLI could disconnect between the two —
        // app would then end up in a room with no CLI.
        let mut room = self.rooms.entry(request.room_id.clone()).or_default();

        if request.role == Role::App && room.cli.is_none() {
            // Drop the entry lock before returning.  If or_default() just
            // created an empty room, remove it; but only if it is still empty
            // (a CLI might have joined in the tiny window after we drop).
            drop(room);
            self.rooms
                .remove_if(&request.room_id, |_, r| r.cli.is_none() && r.app.is_none());
            return Err(HubError::PeerMissing { role: "cli" });
        }
        if !room.accepts_admission(request.relay_admission) {
            return Err(HubError::AdmissionRejected);
        }
        room.touch();
        room.register_admission(request.role, request.relay_admission);
        let peer = room.peer_for(request.role).cloned();
        let mut app_evicted = false;

        let newcomer = Peer {
            conn_id: request.conn_id,
            outbound: request.outbound.clone(),
            device_pubkey: request.device_pubkey,
            pairing_token_proof: request.pairing_token_proof,
            connection_salt: request.connection_salt,
            close_signal: request.close_signal.clone(),
        };

        match request.role {
            Role::Cli => {
                let evicted = room.cli.take();
                room.cli = Some(newcomer);
                if let Some(old_cli) = evicted {
                    // A CLI rejoin is the same terminal process replacing its
                    // relay transport. App receives PeerJoined below and resets
                    // its secure session; sending PeerLeft first makes App
                    // close and auto-reconnect, which can create a reconnect
                    // storm with the CLI transport. The old transport is already
                    // displaced, so a failed notice only leaves it to time out.
                    if let Err(err) = old_cli
                        .outbound
                        .try_send(OuterFrame::PeerLeft { role: Role::Cli })
                    {
                        log_control_send_failure(&request.room_id, "PeerLeft", "old cli", &err);
                    }
                    // Out-of-band close so the displaced transport terminates
                    // even when its ordinary outbound queue is full.
                    signal_close(&old_cli, CloseSignal::Replaced);
                }
            }
            Role::App => {
                let evicted = room.app.take();
                room.app = Some(newcomer);
                if let Some(old_app) = evicted {
                    // Tell old app it was displaced by a newer connection so it
                    // can surface "另一台设备已连接" and not auto-retry.
                    // CLI learns about the new app connection from PeerJoined
                    // below; app disconnects are intentionally invisible to CLI
                    // so it can remain idle. The old transport is already
                    // displaced, so a failed notice only leaves it to time out.
                    if let Err(err) = old_app.outbound.try_send(OuterFrame::Evicted) {
                        log_control_send_failure(&request.room_id, "Evicted", "old app", &err);
                    }
                    // Out-of-band close so the displaced transport terminates
                    // even when its ordinary outbound queue is full.
                    signal_close(&old_app, CloseSignal::Evicted);
                    app_evicted = true;
                }
            }
        }

        if let Some(peer) = peer {
            let peer_role = match request.role {
                Role::Cli => Role::App,
                Role::App => Role::Cli,
            };
            // Tell the already-present peer that the newcomer joined. A lost
            // PeerJoined desyncs per-connection salts / key epochs, so a peer
            // whose queue cannot accept it is evicted instead of silently left
            // stale; its websocket task terminates and it reconnects cleanly.
            if let Err(err) = peer.outbound.try_send(OuterFrame::PeerJoined {
                role: request.role,
                device_pubkey: request.device_pubkey,
                pairing_token_proof: request.pairing_token_proof,
                connection_salt: request.connection_salt,
            }) {
                log_control_send_failure(&request.room_id, "PeerJoined", "present peer", &err);
                self.record_slow_consumer_eviction(&request.room_id, peer_role, 0, control_send_reason(&err));
                signal_close(&peer, CloseSignal::SlowConsumer);
                room.leave_active(peer_role, peer.conn_id);
                if let Some(newcomer) = room.peer_for(peer_role) {
                    let _ = newcomer
                        .outbound
                        .try_send(OuterFrame::PeerLeft { role: peer_role });
                }
                return Ok(app_evicted);
            }
            // Also tell the newcomer about the already-present peer. The side
            // that joins or reconnects second must learn the peer's current
            // per-connection salt to derive matching session keys; without this
            // the second joiner would never see the first joiner's salt and the
            // two ends would derive different keys.
            if let Err(err) = request.outbound.try_send(OuterFrame::PeerJoined {
                role: peer_role,
                device_pubkey: peer.device_pubkey,
                pairing_token_proof: peer.pairing_token_proof,
                connection_salt: peer.connection_salt,
            }) {
                log_control_send_failure(&request.room_id, "PeerJoined", "newcomer", &err);
                room.leave_active(request.role, request.conn_id);
                if let Some(survivor) = room.peer_for(request.role) {
                    let _ = survivor
                        .outbound
                        .try_send(OuterFrame::PeerLeft { role: request.role });
                }
                return Err(HubError::JoinNotificationFailed);
            }
        }

        Ok(app_evicted)
    }

    pub fn forward(
        &self,
        room_id: &str,
        from_role: Role,
        from_conn_id: ConnId,
        frame: OuterFrame,
    ) -> Result<(), HubError> {
        if let Some(mut room) = self.rooms.get_mut(room_id) {
            if !room.matches(from_role, from_conn_id) {
                return Ok(());
            }

            room.touch();
            let (to_role, peer) = match from_role {
                Role::Cli => (Role::App, room.app.as_ref()),
                Role::App => (Role::Cli, room.cli.as_ref()),
            };
            if let Some(peer) = peer {
                // Silent drops here desync the receiver's SecureSession seq
                // counter. Evict the slow recipient instead so its websocket
                // task terminates and the secure stream restarts cleanly.
                let queue_used = peer
                    .outbound
                    .max_capacity()
                    .saturating_sub(peer.outbound.capacity()) as u64;
                self.metrics
                    .outbound_queue_high_water
                    .fetch_max(queue_used, Ordering::Relaxed);
                let close_signal = peer.close_signal.clone();
                let failed_send = peer.outbound.try_send(frame).err().map(|err| {
                    let reason = control_send_reason(&err);
                    let frame_bytes = match &err {
                        mpsc::error::TrySendError::Full(frame)
                        | mpsc::error::TrySendError::Closed(frame) => frame_payload_bytes(frame),
                    };
                    (peer.conn_id, reason, frame_bytes)
                });
                if let Some((to_conn_id, reason, frame_bytes)) = failed_send {
                    self.record_slow_consumer_eviction(room_id, to_role, frame_bytes, reason);
                    if let Some(close_signal) = close_signal {
                        let _ = close_signal.try_send(CloseSignal::SlowConsumer);
                    }
                    room.leave_active(to_role, to_conn_id);
                    // Tell the surviving sender that the recipient left so its
                    // SecureSession resets cleanly. Without this the sender keeps
                    // advancing its sequence counter past the dropped frame and
                    // the receiver desyncs once it eventually reconnects. The
                    // subsequent `leave` from the evicted peer's own task can no
                    // longer deliver this notice because its slot is already
                    // cleared here, so it must be sent now.
                    if let Some(survivor) = room.peer_for(to_role) {
                        let _ = survivor
                            .outbound
                            .try_send(OuterFrame::PeerLeft { role: to_role });
                    }
                    eprintln!(
                        "WARN relaycat: relay forward evicted peer room={room_id} from={from_role:?} to={to_role:?} reason={reason} discarded_frame_bytes={frame_bytes} queue_high_water={}",
                        self.metrics.outbound_queue_high_water.load(Ordering::Relaxed),
                    );
                }
            }
        }

        Ok(())
    }

    pub fn check_room_admission(&self, room_id: &str, relay_admission: [u8; 32]) -> AdmissionCheck {
        let Some(room) = self.rooms.get(room_id) else {
            return AdmissionCheck::RoomMissing;
        };
        match room.relay_admission {
            Some(expected) if constant_time_eq_32(&expected, &relay_admission) => {
                AdmissionCheck::Authorized
            }
            Some(_) => AdmissionCheck::Unauthorized,
            None => AdmissionCheck::RoomUnprotected,
        }
    }

    pub fn leave(&self, room_id: &str, role: Role, conn_id: ConnId) {
        if let Some(mut room) = self.rooms.get_mut(room_id) {
            if room.matches(role, conn_id) {
                let peer = (role == Role::Cli)
                    .then(|| room.peer_for(role).cloned())
                    .flatten();
                room.leave_active(role, conn_id);
                if let Some(peer) = peer {
                    // A lost PeerLeft leaves the app waiting on a dead CLI; a
                    // peer whose queue cannot accept it is evicted so its
                    // websocket task terminates and it reconnects cleanly.
                    if let Err(err) = peer.outbound.try_send(OuterFrame::PeerLeft { role }) {
                        log_control_send_failure(room_id, "PeerLeft", "surviving peer", &err);
                        let survivor_role = match role {
                            Role::Cli => Role::App,
                            Role::App => Role::Cli,
                        };
                        self.record_slow_consumer_eviction(
                            room_id,
                            survivor_role,
                            0,
                            control_send_reason(&err),
                        );
                        signal_close(&peer, CloseSignal::SlowConsumer);
                        room.leave_active(
                            match role {
                                Role::Cli => Role::App,
                                Role::App => Role::Cli,
                            },
                            peer.conn_id,
                        );
                    }
                }
            }
        }

        // Use remove_if rather than a separate remove(): between releasing the
        // get_mut() lock above and calling remove(), a new peer could have
        // joined.  remove_if is atomic — it only removes the entry if both
        // slots are still empty at the moment of removal.
        self.rooms
            .remove_if(room_id, |_, room| room.cli.is_none() && room.app.is_none());
    }

    pub fn room_count(&self) -> usize {
        self.rooms.len()
    }

    fn record_slow_consumer_eviction(
        &self,
        room_id: &str,
        role: Role,
        frame_bytes: usize,
        reason: &str,
    ) {
        self.metrics
            .slow_consumer_evictions_total
            .fetch_add(1, Ordering::Relaxed);
        match role {
            Role::Cli => &self.metrics.slow_consumer_evictions_cli,
            Role::App => &self.metrics.slow_consumer_evictions_app,
        }
        .fetch_add(1, Ordering::Relaxed);
        self.metrics
            .slow_consumer_discarded_bytes
            .fetch_add(frame_bytes as u64, Ordering::Relaxed);
        eprintln!(
            "WARN relaycat: slow consumer eviction room={room_id} role={role:?} reason={reason} discarded_frame_bytes={frame_bytes} evictions_total={}",
            self.metrics
                .slow_consumer_evictions_total
                .load(Ordering::Relaxed),
        );
    }

    pub fn stats(&self) -> HubStats {
        let mut stats = HubStats {
            rooms_total: self.rooms.len(),
            slow_consumer_evictions_total: self
                .metrics
                .slow_consumer_evictions_total
                .load(Ordering::Relaxed),
            slow_consumer_evictions_cli: self
                .metrics
                .slow_consumer_evictions_cli
                .load(Ordering::Relaxed),
            slow_consumer_evictions_app: self
                .metrics
                .slow_consumer_evictions_app
                .load(Ordering::Relaxed),
            slow_consumer_discarded_bytes: self
                .metrics
                .slow_consumer_discarded_bytes
                .load(Ordering::Relaxed),
            outbound_queue_high_water: self
                .metrics
                .outbound_queue_high_water
                .load(Ordering::Relaxed),
            ..HubStats::default()
        };

        for room in self.rooms.iter() {
            let has_cli = room.cli.is_some();
            let has_app = room.app.is_some();

            if has_cli {
                stats.cli_connected_total += 1;
            }
            if has_app {
                stats.app_connected_total += 1;
            }
            match (has_cli, has_app) {
                (true, true) => {
                    stats.cli_paired += 1;
                    stats.app_paired += 1;
                }
                (true, false) => {
                    stats.cli_idle += 1;
                }
                (false, true) => {
                    stats.app_without_cli += 1;
                }
                (false, false) => {}
            }
        }

        stats
    }

    pub fn cleanup_inactive_rooms(&self, ttl: Duration) -> usize {
        let now = Instant::now();
        let expired_keys: Vec<String> = self
            .rooms
            .iter()
            .filter(|room| now.duration_since(room.last_active_at) > ttl)
            .map(|room| room.key().clone())
            .collect();

        let mut removed = 0;
        for room_id in expired_keys {
            // Re-check expiry atomically: a room may have been touched (peer
            // joined or forwarded a frame) between the iter() above and now.
            // remove_if only removes if the predicate holds at removal time.
            if let Some((_, room)) = self.rooms.remove_if(&room_id, |_, room| {
                now.duration_since(room.last_active_at) > ttl
            }) {
                room.notify_all(OuterFrame::Error {
                    message: "room expired".to_string(),
                    code: Some(RelayErrorCode::RoomExpired),
                });
                removed += 1;
            }
        }

        removed
    }
}

#[derive(Debug)]
struct HubRoom {
    cli: Option<Peer>,
    app: Option<Peer>,
    relay_admission: Option<[u8; 32]>,
    last_active_at: Instant,
}

impl Default for HubRoom {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            cli: None,
            app: None,
            relay_admission: None,
            last_active_at: now,
        }
    }
}

impl HubRoom {
    fn touch(&mut self) {
        self.last_active_at = Instant::now();
    }

    fn accepts_admission(&self, relay_admission: Option<[u8; 32]>) -> bool {
        match self.relay_admission {
            Some(expected) => {
                relay_admission.is_some_and(|actual| constant_time_eq_32(&actual, &expected))
            }
            None => true,
        }
    }

    fn register_admission(&mut self, role: Role, relay_admission: Option<[u8; 32]>) {
        if role == Role::Cli && self.relay_admission.is_none() {
            self.relay_admission = relay_admission;
        }
    }

    fn peer_for(&self, role: Role) -> Option<&Peer> {
        match role {
            Role::Cli => self.app.as_ref(),
            Role::App => self.cli.as_ref(),
        }
    }

    fn matches(&self, role: Role, conn_id: ConnId) -> bool {
        match role {
            Role::Cli => self
                .cli
                .as_ref()
                .is_some_and(|peer| peer.conn_id == conn_id),
            Role::App => self
                .app
                .as_ref()
                .is_some_and(|peer| peer.conn_id == conn_id),
        }
    }

    fn leave_active(&mut self, role: Role, conn_id: ConnId) {
        match role {
            Role::Cli
                if self
                    .cli
                    .as_ref()
                    .is_some_and(|peer| peer.conn_id == conn_id) =>
            {
                self.cli = None;
            }
            Role::App
                if self
                    .app
                    .as_ref()
                    .is_some_and(|peer| peer.conn_id == conn_id) =>
            {
                self.app = None;
            }
            _ => {}
        }
    }

    fn notify_all(&self, frame: OuterFrame) {
        if let Some(peer) = &self.cli {
            let _ = peer.outbound.try_send(frame.clone());
        }
        if let Some(peer) = &self.app {
            let _ = peer.outbound.try_send(frame);
        }
    }
}

#[derive(Debug, Clone)]
struct Peer {
    conn_id: ConnId,
    outbound: OutboundTx,
    device_pubkey: [u8; 32],
    pairing_token_proof: Option<[u8; 32]>,
    connection_salt: Option<[u8; 32]>,
    close_signal: Option<CloseSignalTx>,
}

fn signal_close(peer: &Peer, signal: CloseSignal) {
    if let Some(close_signal) = &peer.close_signal {
        let _ = close_signal.try_send(signal);
    }
}

fn control_send_reason(err: &mpsc::error::TrySendError<OuterFrame>) -> &'static str {
    match err {
        mpsc::error::TrySendError::Full(_) => "channel full",
        mpsc::error::TrySendError::Closed(_) => "channel closed",
    }
}

fn frame_payload_bytes(frame: &OuterFrame) -> usize {
    match frame {
        OuterFrame::Data { ciphertext, .. } => ciphertext.len(),
        _ => 0,
    }
}

fn log_control_send_failure(
    room_id: &str,
    frame: &str,
    target: &str,
    err: &mpsc::error::TrySendError<OuterFrame>,
) {
    let reason = control_send_reason(err);
    eprintln!(
        "WARN relaycat: relay control frame {frame} to {target} dropped room={room_id} reason={reason}",
    );
}

/// Compare two 32-byte admission tokens in constant time so the relay does not
/// leak how many leading bytes of a guess were correct via early-exit timing.
fn constant_time_eq_32(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0_u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}
