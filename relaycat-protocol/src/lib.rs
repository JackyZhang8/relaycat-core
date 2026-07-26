use serde::{Deserialize, Serialize};

pub mod compression;
pub mod workspace;

pub use compression::{
    PAYLOAD_COMPRESSION_MIN_BYTES, PAYLOAD_FRAME_DEFLATE, PAYLOAD_FRAME_IDENTITY, frame_payload,
    unframe_payload,
};
pub use workspace::*;

pub type Result<T> = std::result::Result<T, rmp_serde::decode::Error>;

/// Maximum encoded binary WebSocket message accepted by the relay and apps.
pub const MAX_OUTER_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Cli,
    App,
}

/// Whether an App Join was initiated explicitly by the user or by automatic
/// transport recovery. The relay only lets an explicit takeover replace an
/// App that already owns the room's single App slot. A missing field is
/// intentionally treated as `Resume` so an older client cannot reclaim a slot
/// merely because it cannot express its intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AppJoinIntent {
    #[default]
    Resume,
    Takeover,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    CliToApp,
    AppToCli,
}

pub const TERMINAL_STATE_PROTOCOL_V2: u16 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolCapabilityV2 {
    TerminalState,
    SnapshotRecovery,
    ExactlyOnceInput,
    /// Scrolled-off rows are streamed incrementally via `PatchOp::AppendScrollback`
    /// instead of only appearing in full snapshots.
    IncrementalScrollback,
    /// A separate paged record of terminal content that was presented over
    /// time, including alternate-screen frames. This is intentionally separate
    /// from terminal scrollback so full-screen TUI repaint churn does not
    /// pollute the terminal model.
    TerminalTranscript,
    /// CLI-provided session metadata, currently including the project path.
    CliMetadata,
    /// Per-message payload compression (raw DEFLATE) applied to the plaintext
    /// before encryption. Negotiated via the Hello/HelloAck handshake; a peer
    /// that does not advertise it keeps receiving uncompressed frames.
    Compression,
    /// Patches carry only the attribute-table entries appended since the last
    /// emission (a tail), keyed by `TerminalPatchV2::attrs_base_len`, instead of
    /// re-sending the whole table whenever it grows. Negotiated via the
    /// Hello/HelloAck handshake; a peer that does not advertise it keeps
    /// receiving the full table on growth.
    IncrementalAttrs,
    /// Project-scoped file, Git and auxiliary shell RPC.
    WorkspaceRpc,
    /// Named auxiliary terminal streams using the Terminal V2 semantic model.
    TerminalStreams,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloV2 {
    pub protocol_versions: Vec<u16>,
    #[serde(deserialize_with = "deserialize_capabilities")]
    pub capabilities: Vec<ProtocolCapabilityV2>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloAckV2 {
    pub selected_protocol_version: u16,
    #[serde(deserialize_with = "deserialize_capabilities")]
    pub capabilities: Vec<ProtocolCapabilityV2>,
}

/// Decode a capability list while skipping entries this build does not
/// recognize, instead of failing the whole frame. This keeps the Hello/HelloAck
/// handshake forward-compatible: a peer that advertises a capability added in a
/// newer build is still understood for every capability both sides share.
fn deserialize_capabilities<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<ProtocolCapabilityV2>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum CapabilityOrUnknown {
        Known(ProtocolCapabilityV2),
        Unknown(serde::de::IgnoredAny),
    }

    let raw = Vec::<CapabilityOrUnknown>::deserialize(deserializer)?;
    Ok(raw
        .into_iter()
        .filter_map(|capability| match capability {
            CapabilityOrUnknown::Known(capability) => Some(capability),
            CapabilityOrUnknown::Unknown(_) => None,
        })
        .collect())
}

/// How the CLI will bring the app up to date after accepting a `ResumeV2`.
/// Lets the app keep its reconnect overlay visible until the replayed backlog
/// has actually arrived instead of guessing with a fixed fallback delay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeAcceptMode {
    /// The app's last applied state is already current; nothing to replay.
    UpToDate,
    /// Retained patches after the app's last applied seq will be replayed.
    ReplayingPatches,
    /// The retained range is unusable; a full snapshot follows.
    SendingSnapshot,
}

/// CLI reply to `ResumeV2`, sent before the replayed patches / snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeAcceptedV2 {
    pub mode: ResumeAcceptMode,
    /// The CLI-side state seq the app will have applied once caught up.
    pub target_state_seq: u64,
}

/// CLI reply to `HelloV2` when the peers share no protocol version or a
/// mandatory capability is missing. The receiver should surface a
/// "protocol incompatible" state instead of retrying.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolRejectV2 {
    pub reason: String,
    pub supported_versions: Vec<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeV2 {
    pub terminal_run_id: Option<String>,
    pub last_applied_state_seq: u64,
    pub last_snapshot_id: Option<u64>,
    #[serde(default = "default_input_stream_id")]
    pub input_stream_id: String,
    pub last_input_ack: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalColor {
    Default,
    Indexed(u16),
    Rgb { r: u8, g: u8, b: u8 },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CellAttr {
    pub fg: TerminalColor,
    pub bg: TerminalColor,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
    pub strikethrough: bool,
    pub dim: bool,
}

impl Default for CellAttr {
    fn default() -> Self {
        Self {
            fg: TerminalColor::Default,
            bg: TerminalColor::Default,
            bold: false,
            italic: false,
            underline: false,
            inverse: false,
            strikethrough: false,
            dim: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalCell {
    pub text: String,
    pub width: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellRun {
    pub attr_id: u32,
    pub cells: Vec<TerminalCell>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalRow {
    pub line_id: u64,
    pub wrapped: bool,
    pub cells: Vec<CellRun>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorStyle {
    Block,
    Bar,
    Underline,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorState {
    pub row: u16,
    pub col: u16,
    pub visible: bool,
    pub style: CursorStyle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalModes {
    pub alt_screen: bool,
    pub bracketed_paste: bool,
    pub application_cursor: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaletteState {
    pub default_fg: TerminalColor,
    pub default_bg: TerminalColor,
    pub cursor: TerminalColor,
    pub ansi: Vec<TerminalColor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSnapshotV2 {
    pub terminal_run_id: String,
    pub snapshot_id: u64,
    pub state_seq: u64,
    pub cols: u16,
    pub rows: u16,
    pub title: String,
    pub cursor: CursorState,
    pub modes: TerminalModes,
    pub palette: PaletteState,
    pub attrs: Vec<CellAttr>,
    #[serde(default)]
    pub reset_app_cache: bool,
    pub scrollback_window: Vec<TerminalRow>,
    pub screen_rows: Vec<TerminalRow>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalMode {
    BracketedPaste,
    ApplicationCursor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatchOp {
    PutCells {
        row: u16,
        col: u16,
        cells: Vec<CellRun>,
    },
    ClearRange {
        row: u16,
        col_start: u16,
        col_end: u16,
        attr_id: u32,
    },
    ReplaceRow {
        row: u16,
        line: TerminalRow,
    },
    ScrollRegion {
        top: u16,
        bottom: u16,
        delta: i16,
    },
    SetCursor(CursorState),
    SetTitle(String),
    SetPalette(PaletteState),
    SetMode {
        mode: TerminalMode,
        enabled: bool,
    },
    SwitchAltScreen(bool),
    Bell,
    /// Rows that have just scrolled off the top of the live screen, ordered
    /// oldest first. Receivers append them to their scrollback history so the
    /// user can scroll back through output produced during patch streaming
    /// (i.e. without waiting for the next full snapshot).
    AppendScrollback {
        rows: Vec<TerminalRow>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalPatchV2 {
    pub terminal_run_id: String,
    pub base_snapshot_id: u64,
    pub from_state_seq: u64,
    pub to_state_seq: u64,
    /// The attribute table entries referenced by this patch's `attr_id`s. The
    /// table is append-only across a snapshot base.
    ///
    /// Interpretation depends on `attrs_base_len`:
    /// - `attrs_base_len == None` (legacy): `attrs` is the **whole table** when
    ///   it grew since the previous patch, or an **empty** vector meaning
    ///   "unchanged — reuse the table already held by the receiver".
    /// - `attrs_base_len == Some(n)`: `attrs` is only the **tail** appended
    ///   after index `n`; the receiver appends it onto the `n` entries it
    ///   already holds. Only emitted when the peer negotiated
    ///   `ProtocolCapabilityV2::IncrementalAttrs`.
    pub attrs: Vec<CellAttr>,
    /// Index at which `attrs` should be appended for incremental-attr peers.
    /// `None` keeps the legacy whole-table-or-empty semantics, so the wire stays
    /// byte-identical for peers that did not negotiate `IncrementalAttrs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attrs_base_len: Option<u32>,
    pub ops: Vec<PatchOp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatchRejectReason {
    MissingBase,
    SequenceGap,
    EmptyPatchRange,
}

impl TerminalPatchV2 {
    pub fn validate_against(
        &self,
        snapshot_id: u64,
        current_state_seq: u64,
    ) -> std::result::Result<(), PatchRejectReason> {
        if self.base_snapshot_id != snapshot_id {
            return Err(PatchRejectReason::MissingBase);
        }
        if self.from_state_seq != current_state_seq.saturating_add(1) {
            return Err(PatchRejectReason::SequenceGap);
        }
        if self.to_state_seq < self.from_state_seq {
            return Err(PatchRejectReason::EmptyPatchRange);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderAckV2 {
    pub terminal_run_id: String,
    pub snapshot_id: u64,
    pub applied_state_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestSnapshotV2 {
    pub terminal_run_id: Option<String>,
    pub reason: SnapshotRequestReason,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotRequestReason {
    SeqGap,
    MissingBase,
    RendererReset,
    MemoryPressure,
    Reconnect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalTranscriptEntryKind {
    NormalScrollback,
    AltScreenFrame,
    ScreenFrame,
}

/// Identifies one row-boundary fragment of a logical screen-frame transcript
/// entry. The field containing this metadata is optional so older V2 peers can
/// ignore it while continuing to decode every fragment as an ordinary entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TerminalTranscriptFrameFragmentV2 {
    /// `entry_id` of the first fragment in this logical frame.
    pub frame_id: u64,
    /// Zero-based position within the logical frame.
    pub fragment_index: u32,
    /// Total number of fragments required to reconstruct the frame.
    pub fragment_count: u32,
}

impl<'de> Deserialize<'de> for TerminalTranscriptFrameFragmentV2 {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireFragment {
            frame_id: u64,
            fragment_index: u32,
            fragment_count: u32,
        }

        let fragment = WireFragment::deserialize(deserializer)?;
        if fragment.fragment_count == 0 || fragment.fragment_index >= fragment.fragment_count {
            return Err(<D::Error as serde::de::Error>::custom(
                "invalid transcript fragment coordinates",
            ));
        }
        Ok(Self {
            frame_id: fragment.frame_id,
            fragment_index: fragment.fragment_index,
            fragment_count: fragment.fragment_count,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalTranscriptEntryV2 {
    pub entry_id: u64,
    pub terminal_run_id: String,
    pub state_seq: u64,
    pub kind: TerminalTranscriptEntryKind,
    pub cols: u16,
    pub rows: Vec<TerminalRow>,
    pub captured_at_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_fragment: Option<TerminalTranscriptFrameFragmentV2>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestTranscriptV2 {
    pub terminal_run_id: String,
    /// Return entries with `entry_id` strictly less than this value. `None`
    /// requests the newest available transcript page.
    pub before_entry_id: Option<u64>,
    pub max_entries: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptChunkV2 {
    pub terminal_run_id: String,
    pub before_entry_id: Option<u64>,
    pub entries: Vec<TerminalTranscriptEntryV2>,
    pub attrs: Vec<CellAttr>,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputEventV2 {
    #[serde(default = "default_input_stream_id")]
    pub input_stream_id: String,
    pub input_seq: u64,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputAckV2 {
    /// The input stream the ack belongs to, so a late ack from a previous
    /// stream can never release pending inputs of a rebuilt stream. Defaults
    /// to `"legacy"` for messages from older peers.
    #[serde(default = "default_input_stream_id")]
    pub input_stream_id: String,
    pub highest_contiguous_input_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResizeEventV2 {
    pub resize_seq: u64,
    pub cols: u16,
    pub rows: u16,
    #[serde(default = "default_input_stream_id")]
    pub input_stream_id: String,
    #[serde(default)]
    pub last_input_ack: u64,
}

fn default_input_stream_id() -> String {
    "legacy".to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResizeAckV2 {
    pub resize_seq: u64,
}

/// A named auxiliary terminal stream carried over the same secure connection
/// as the primary terminal. Keeping the stream identifier outside the nested
/// terminal message preserves the existing primary-terminal wire format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalStreamV2 {
    pub stream_id: String,
    pub message: TerminalStreamMessageV2,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalStreamMessageV2 {
    Snapshot(TerminalSnapshotV2),
    Patch(TerminalPatchV2),
    RenderAck(RenderAckV2),
    RequestSnapshot(RequestSnapshotV2),
    Resume(ResumeV2),
    ResumeAccepted(ResumeAcceptedV2),
    Input {
        input_stream_id: String,
        input_seq: u64,
        bytes: Vec<u8>,
    },
    InputAck(InputAckV2),
    Resize(ResizeEventV2),
    ResizeAck(ResizeAckV2),
    Exit {
        code: Option<i32>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliMetadata {
    pub project_path: String,
    #[serde(default)]
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputDecision {
    Accept,
    Duplicate,
    Gap,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InputDedupe {
    input_stream_id: Option<String>,
    highest_contiguous_input_seq: u64,
}

impl InputDedupe {
    pub fn observe(&mut self, input_stream_id: &str, input_seq: u64) -> InputDecision {
        self.select_input_stream(input_stream_id);
        let expected = self.highest_contiguous_input_seq.saturating_add(1);
        if input_seq <= self.highest_contiguous_input_seq {
            return InputDecision::Duplicate;
        }
        if input_seq != expected {
            return InputDecision::Gap;
        }
        self.highest_contiguous_input_seq = input_seq;
        InputDecision::Accept
    }

    pub fn highest_contiguous_input_seq(&self) -> u64 {
        self.highest_contiguous_input_seq
    }

    pub fn input_stream_id(&self) -> Option<&str> {
        self.input_stream_id.as_deref()
    }

    pub fn synchronize_ack(&mut self, input_stream_id: &str, highest_contiguous_input_seq: u64) {
        self.select_input_stream(input_stream_id);
        self.highest_contiguous_input_seq = self
            .highest_contiguous_input_seq
            .max(highest_contiguous_input_seq);
    }

    fn select_input_stream(&mut self, input_stream_id: &str) {
        if self.input_stream_id.as_deref() == Some(input_stream_id) {
            return;
        }
        self.input_stream_id = Some(input_stream_id.to_string());
        self.highest_contiguous_input_seq = 0;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OuterFrame {
    Join {
        room_id: String,
        role: Role,
        device_pubkey: [u8; 32],
        pairing_token_proof: Option<[u8; 32]>,
        #[serde(default)]
        relay_admission: Option<[u8; 32]>,
        /// Fresh random salt generated per relay connection. Mixed into the
        /// session-key derivation so that every (re)connection derives a
        /// distinct key, even though the long-term ECDH key material and the
        /// sequence counters both reset to their initial values. This is what
        /// prevents ChaCha20-Poly1305 nonce reuse across reconnects.
        ///
        /// Optional for wire backward compatibility: peers that predate this
        /// field send `None`, in which case both sides fall back to the
        /// legacy (salt-free) derivation so old and new builds still pair.
        #[serde(default)]
        connection_salt: Option<[u8; 32]>,
        /// Whether the joiner understands the explicit [`OuterFrame::JoinAccepted`]
        /// acknowledgement. Optional for wire backward compatibility: peers
        /// that predate the field send `None`/`false` and the relay never
        /// sends them `JoinAccepted` (their decoders would reject the unknown
        /// frame), so they keep using the timing-based rejection probe.
        #[serde(default)]
        supports_join_accepted: bool,
        /// `takeover` is an explicit user action and may replace the active
        /// App. `resume` is an automatic retry and is rejected while another
        /// App owns the room. Missing fields default to `resume` so clients
        /// released before this arbitration rule cannot take the slot back.
        #[serde(default)]
        app_join_intent: AppJoinIntent,
    },
    /// Explicit acknowledgement that the relay admitted this connection's
    /// `Join`. Only sent to joiners that set `supports_join_accepted`; it is
    /// authoritative and replaces the 200ms "no error frame yet" probe as the
    /// join-success signal (the probe remains as a fallback for old relays).
    JoinAccepted,
    PeerJoined {
        role: Role,
        device_pubkey: [u8; 32],
        pairing_token_proof: Option<[u8; 32]>,
        /// The joining peer's per-connection salt, forwarded verbatim by the
        /// relay from its `Join`. See [`OuterFrame::Join::connection_salt`].
        #[serde(default)]
        connection_salt: Option<[u8; 32]>,
    },
    PeerLeft {
        role: Role,
    },
    Data {
        room_id: String,
        direction: Direction,
        seq: u64,
        nonce: [u8; 12],
        ciphertext: Vec<u8>,
    },
    Ack {
        room_id: String,
        direction: Direction,
        seq: u64,
    },
    /// Sent to an App peer when a newer App connection has taken its slot.
    /// The recipient should surface "另一台设备已连接" to the user and must
    /// not auto-retry (unlike transient network errors).
    Evicted,
    Ping,
    Pong,
    Error {
        message: String,
        /// Stable machine-readable reason. Optional for wire backward
        /// compatibility: relays that predate it send only `message`, and
        /// codes unknown to this build decode as `None` so apps fall back to
        /// message-based classification.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_error_code"
        )]
        code: Option<RelayErrorCode>,
    },
}

/// Stable machine-readable relay error codes, so peers classify failures
/// without parsing natural-language messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayErrorCode {
    InvalidRoomId,
    ServerAtCapacity,
    JoinTimeout,
    FrameTooLarge,
    InvalidJoin,
    JoinRoomRoleMismatch,
    PeerNotRegistered,
    AdmissionRejected,
    JoinNotificationFailed,
    HeartbeatTimeout,
    RateLimited,
    RoomExpired,
    AppSessionTaken,
}

impl RelayErrorCode {
    /// Whether an app should auto-reconnect after this error.
    pub fn is_retryable(self) -> bool {
        match self {
            RelayErrorCode::ServerAtCapacity
            | RelayErrorCode::JoinTimeout
            | RelayErrorCode::JoinNotificationFailed
            | RelayErrorCode::HeartbeatTimeout
            | RelayErrorCode::RateLimited => true,
            RelayErrorCode::PeerNotRegistered
            | RelayErrorCode::InvalidRoomId
            | RelayErrorCode::FrameTooLarge
            | RelayErrorCode::InvalidJoin
            | RelayErrorCode::JoinRoomRoleMismatch
            | RelayErrorCode::AdmissionRejected
            | RelayErrorCode::RoomExpired
            | RelayErrorCode::AppSessionTaken => false,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            RelayErrorCode::InvalidRoomId => "invalid_room_id",
            RelayErrorCode::ServerAtCapacity => "server_at_capacity",
            RelayErrorCode::JoinTimeout => "join_timeout",
            RelayErrorCode::FrameTooLarge => "frame_too_large",
            RelayErrorCode::InvalidJoin => "invalid_join",
            RelayErrorCode::JoinRoomRoleMismatch => "join_room_role_mismatch",
            RelayErrorCode::PeerNotRegistered => "peer_not_registered",
            RelayErrorCode::AdmissionRejected => "admission_rejected",
            RelayErrorCode::JoinNotificationFailed => "join_notification_failed",
            RelayErrorCode::HeartbeatTimeout => "heartbeat_timeout",
            RelayErrorCode::RateLimited => "rate_limited",
            RelayErrorCode::RoomExpired => "room_expired",
            RelayErrorCode::AppSessionTaken => "app_session_taken",
        }
    }
}

/// Decode an error code while mapping values this build does not recognize to
/// `None`, so newer relays can add codes without breaking older peers.
fn deserialize_error_code<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<RelayErrorCode>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum CodeOrUnknown {
        Known(RelayErrorCode),
        Unknown(serde::de::IgnoredAny),
    }

    Ok(
        match Option::<CodeOrUnknown>::deserialize(deserializer)? {
            Some(CodeOrUnknown::Known(code)) => Some(code),
            _ => None,
        },
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlainMsg {
    HelloV2(HelloV2),
    HelloAckV2(HelloAckV2),
    ProtocolRejectV2(ProtocolRejectV2),
    ResumeV2(ResumeV2),
    ResumeAcceptedV2(ResumeAcceptedV2),
    ProcessExit { code: Option<i32> },
    TerminalSnapshotV2(TerminalSnapshotV2),
    TerminalPatchV2(TerminalPatchV2),
    RenderAckV2(RenderAckV2),
    RequestSnapshotV2(RequestSnapshotV2),
    RequestTranscriptV2(RequestTranscriptV2),
    TranscriptChunkV2(TranscriptChunkV2),
    InputEventV2(InputEventV2),
    InputAckV2(InputAckV2),
    ResizeEventV2(ResizeEventV2),
    ResizeAckV2(ResizeAckV2),
    TerminalStreamV2(TerminalStreamV2),
    Heartbeat,
    CliStatus(CliStatus),
    CliMetadata(CliMetadata),
    WorkspaceRequest(WorkspaceRequestEnvelope),
    WorkspaceResponse(WorkspaceResponseEnvelope),
    WorkspaceEvent(WorkspaceEventEnvelope),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliStatus {
    pub cpu_percent_x10: u16,
    pub memory_bytes: u64,
    pub rx_bytes_per_sec: u64,
    pub tx_bytes_per_sec: u64,
    pub process_name: String,
    pub collected_at_unix_ms: u64,
}

pub fn encode_frame(frame: &OuterFrame) -> std::result::Result<Vec<u8>, rmp_serde::encode::Error> {
    encode_named(frame)
}

pub fn decode_frame(bytes: &[u8]) -> Result<OuterFrame> {
    rmp_serde::from_slice(bytes)
}

pub fn encode_plain_msg(msg: &PlainMsg) -> std::result::Result<Vec<u8>, rmp_serde::encode::Error> {
    encode_named(msg)
}

fn encode_named<T: Serialize + ?Sized>(
    value: &T,
) -> std::result::Result<Vec<u8>, rmp_serde::encode::Error> {
    let mut bytes = Vec::new();
    let mut serializer = rmp_serde::Serializer::new(&mut bytes)
        .with_struct_map()
        .with_bytes(rmp_serde::config::BytesMode::ForceIterables);
    value.serialize(&mut serializer)?;
    Ok(bytes)
}

/// An `io::Write` sink that only tallies how many bytes were written.
#[derive(Debug, Default)]
struct ByteCounter {
    count: usize,
}

impl std::io::Write for ByteCounter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.count += buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Returns the exact byte length [`value`] would serialize to under the same
/// named-msgpack encoding as [`encode_plain_msg`], without allocating the
/// output buffer. Used by the CLI's size budgeting, which previously cloned and
/// fully encoded large snapshots/patches just to read off the length.
fn named_encoded_len<T: Serialize + ?Sized>(value: &T) -> usize {
    let mut counter = ByteCounter::default();
    let mut serializer = rmp_serde::Serializer::new(&mut counter)
        .with_struct_map()
        .with_bytes(rmp_serde::config::BytesMode::ForceIterables);
    match value.serialize(&mut serializer) {
        Ok(()) => counter.count,
        Err(_) => usize::MAX,
    }
}

/// Length of `msg` as produced by [`encode_plain_msg`], without allocating.
pub fn plain_msg_encoded_len(msg: &PlainMsg) -> usize {
    named_encoded_len(msg)
}

/// A single-entry map `{label: value}` matching how serde encodes an
/// externally-tagged enum variant, so its serialized length equals that of the
/// corresponding [`PlainMsg`] variant without owning/cloning `value`.
struct ExternallyTagged<'a, T: Serialize + ?Sized> {
    label: &'a str,
    value: &'a T,
}

impl<T: Serialize + ?Sized> Serialize for ExternallyTagged<'_, T> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry(self.label, self.value)?;
        map.end()
    }
}

/// Length of `PlainMsg::TerminalPatchV2(patch)` as encoded on the wire, computed
/// from a borrowed patch (no clone, no output allocation).
pub fn terminal_patch_v2_encoded_len(patch: &TerminalPatchV2) -> usize {
    named_encoded_len(&ExternallyTagged {
        label: "terminal_patch_v2",
        value: patch,
    })
}

/// Length of `PlainMsg::TerminalSnapshotV2(snapshot)` as encoded on the wire,
/// computed from a borrowed snapshot (no clone, no output allocation).
pub fn terminal_snapshot_v2_encoded_len(snapshot: &TerminalSnapshotV2) -> usize {
    named_encoded_len(&ExternallyTagged {
        label: "terminal_snapshot_v2",
        value: snapshot,
    })
}

/// Exact encoded length of an encrypted data frame without allocating the
/// ciphertext body. The serializer emits non-empty `Vec<u8>` values as a
/// MessagePack binary value, so a one-byte sample frame supplies all metadata
/// overhead and only the binary payload length needs to be substituted.
pub fn outer_data_frame_encoded_len(
    room_id: &str,
    direction: Direction,
    seq: u64,
    nonce: [u8; 12],
    ciphertext_len: usize,
) -> usize {
    let sample_ciphertext = if ciphertext_len == 0 {
        Vec::new()
    } else {
        vec![0]
    };
    let sample = OuterFrame::Data {
        room_id: room_id.to_owned(),
        direction,
        seq,
        nonce,
        ciphertext: sample_ciphertext,
    };
    let sample_len = named_encoded_len(&sample);
    if ciphertext_len == 0 {
        return sample_len;
    }

    sample_len
        .checked_sub(messagepack_binary_encoded_len(1))
        .and_then(|base| base.checked_add(messagepack_binary_encoded_len(ciphertext_len)))
        .unwrap_or(usize::MAX)
}

fn messagepack_binary_encoded_len(len: usize) -> usize {
    let prefix_len: usize = if u8::try_from(len).is_ok() {
        2
    } else if u16::try_from(len).is_ok() {
        3
    } else if u32::try_from(len).is_ok() {
        5
    } else {
        return usize::MAX;
    };
    prefix_len.checked_add(len).unwrap_or(usize::MAX)
}

#[derive(Serialize)]
struct TranscriptChunkV2Parts<'a> {
    terminal_run_id: &'a str,
    before_entry_id: Option<u64>,
    entries: &'a [TerminalTranscriptEntryV2],
    attrs: &'a [CellAttr],
    has_more: bool,
}

/// Exact encoded length of a transcript chunk assembled from borrowed parts.
/// This avoids cloning large transcript entries while planning row-boundary
/// fragmentation.
pub fn transcript_chunk_v2_parts_encoded_len(
    terminal_run_id: &str,
    before_entry_id: Option<u64>,
    entries: &[TerminalTranscriptEntryV2],
    attrs: &[CellAttr],
    has_more: bool,
) -> usize {
    let parts = TranscriptChunkV2Parts {
        terminal_run_id,
        before_entry_id,
        entries,
        attrs,
        has_more,
    };
    named_encoded_len(&ExternallyTagged {
        label: "transcript_chunk_v2",
        value: &parts,
    })
}

pub fn transcript_chunk_v2_encoded_len(chunk: &TranscriptChunkV2) -> usize {
    transcript_chunk_v2_parts_encoded_len(
        &chunk.terminal_run_id,
        chunk.before_entry_id,
        &chunk.entries,
        &chunk.attrs,
        chunk.has_more,
    )
}

#[derive(Serialize)]
struct TerminalTranscriptEntryV2Parts<'a> {
    entry_id: u64,
    terminal_run_id: &'a str,
    state_seq: u64,
    kind: TerminalTranscriptEntryKind,
    cols: u16,
    rows: &'a [TerminalRow],
    captured_at_unix_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    frame_fragment: Option<&'a TerminalTranscriptFrameFragmentV2>,
}

/// Exact encoded length of a transcript entry assembled from borrowed rows.
/// Screen-frame storage uses this to choose row-boundary fragments without
/// repeatedly cloning large terminal rows during size planning.
#[allow(clippy::too_many_arguments)]
pub fn terminal_transcript_entry_v2_parts_encoded_len(
    entry_id: u64,
    terminal_run_id: &str,
    state_seq: u64,
    kind: TerminalTranscriptEntryKind,
    cols: u16,
    rows: &[TerminalRow],
    captured_at_unix_ms: u64,
    frame_fragment: Option<&TerminalTranscriptFrameFragmentV2>,
) -> usize {
    named_encoded_len(&TerminalTranscriptEntryV2Parts {
        entry_id,
        terminal_run_id,
        state_seq,
        kind,
        cols,
        rows,
        captured_at_unix_ms,
        frame_fragment,
    })
}

/// Exact encoded length of one terminal row using the named MessagePack wire
/// configuration.
pub fn terminal_row_v2_encoded_len(row: &TerminalRow) -> usize {
    named_encoded_len(row)
}

/// Exact encoded length of one transcript entry using the same named
/// MessagePack configuration as [`encode_plain_msg`].
pub fn terminal_transcript_entry_v2_encoded_len(entry: &TerminalTranscriptEntryV2) -> usize {
    terminal_transcript_entry_v2_parts_encoded_len(
        entry.entry_id,
        &entry.terminal_run_id,
        entry.state_seq,
        entry.kind,
        entry.cols,
        &entry.rows,
        entry.captured_at_unix_ms,
        entry.frame_fragment.as_ref(),
    )
}

pub fn decode_plain_msg(bytes: &[u8]) -> Result<PlainMsg> {
    rmp_serde::from_slice(bytes)
}

pub fn plain_msg_type(msg: &PlainMsg) -> &'static [u8] {
    match msg {
        PlainMsg::HelloV2(_) => b"hello_v2",
        PlainMsg::HelloAckV2(_) => b"hello_ack_v2",
        PlainMsg::ProtocolRejectV2(_) => b"protocol_reject_v2",
        PlainMsg::ResumeV2(_) => b"resume_v2",
        PlainMsg::ResumeAcceptedV2(_) => b"resume_accepted_v2",
        PlainMsg::ProcessExit { .. } => b"process_exit",
        PlainMsg::TerminalSnapshotV2(_) => b"terminal_snapshot_v2",
        PlainMsg::TerminalPatchV2(_) => b"terminal_patch_v2",
        PlainMsg::RenderAckV2(_) => b"render_ack_v2",
        PlainMsg::RequestSnapshotV2(_) => b"request_snapshot_v2",
        PlainMsg::RequestTranscriptV2(_) => b"request_transcript_v2",
        PlainMsg::TranscriptChunkV2(_) => b"transcript_chunk_v2",
        PlainMsg::InputEventV2(_) => b"input_event_v2",
        PlainMsg::InputAckV2(_) => b"input_ack_v2",
        PlainMsg::ResizeEventV2(_) => b"resize_event_v2",
        PlainMsg::ResizeAckV2(_) => b"resize_ack_v2",
        PlainMsg::TerminalStreamV2(_) => b"terminal_stream_v2",
        PlainMsg::Heartbeat => b"heartbeat",
        PlainMsg::CliStatus(_) => b"cli_status",
        PlainMsg::CliMetadata(_) => b"cli_metadata",
        PlainMsg::WorkspaceRequest(_) => b"workspace_request",
        PlainMsg::WorkspaceResponse(_) => b"workspace_response",
        PlainMsg::WorkspaceEvent(_) => b"workspace_event",
    }
}

pub fn plain_msg_types() -> &'static [&'static [u8]] {
    // The secure layer must trial-decrypt a frame against each label because the
    // message type is part of the AEAD's associated data and is not known until
    // a candidate authenticates. Ordering the labels roughly by how often each
    // type appears on the wire minimizes the number of failed decrypt attempts
    // for the common case (a steady stream of patches/input/acks). This order is
    // a pure performance hint and carries no protocol meaning.
    &[
        b"terminal_patch_v2",
        b"input_event_v2",
        b"render_ack_v2",
        b"input_ack_v2",
        b"heartbeat",
        b"terminal_snapshot_v2",
        b"resize_event_v2",
        b"resize_ack_v2",
        b"terminal_stream_v2",
        b"request_snapshot_v2",
        b"request_transcript_v2",
        b"transcript_chunk_v2",
        b"resume_v2",
        b"resume_accepted_v2",
        b"hello_ack_v2",
        b"hello_v2",
        b"protocol_reject_v2",
        b"cli_status",
        b"cli_metadata",
        b"workspace_request",
        b"workspace_response",
        b"workspace_event",
        b"process_exit",
    ]
}

#[cfg(test)]
mod capability_decode_tests {
    use super::*;
    use serde::Serialize;

    #[derive(Serialize)]
    struct RawHelloBody {
        protocol_versions: Vec<u16>,
        capabilities: Vec<&'static str>,
    }

    #[derive(Serialize)]
    struct RawHello {
        hello_v2: RawHelloBody,
    }

    #[derive(Serialize)]
    struct RawHelloAckBody {
        selected_protocol_version: u16,
        capabilities: Vec<&'static str>,
    }

    #[derive(Serialize)]
    struct RawHelloAck {
        hello_ack_v2: RawHelloAckBody,
    }

    #[test]
    fn hello_v2_decode_skips_unknown_capabilities() {
        let wire = rmp_serde::to_vec_named(&RawHello {
            hello_v2: RawHelloBody {
                protocol_versions: vec![2],
                // A capability from a hypothetical newer peer sits between two
                // capabilities this build knows.
                capabilities: vec!["terminal_state", "future_unknown_capability", "compression"],
            },
        })
        .expect("encode raw hello");

        let decoded = decode_plain_msg(&wire).expect("decode hello with unknown capability");
        match decoded {
            PlainMsg::HelloV2(hello) => {
                assert_eq!(
                    hello.capabilities,
                    vec![
                        ProtocolCapabilityV2::TerminalState,
                        ProtocolCapabilityV2::Compression,
                    ]
                );
            }
            other => panic!("expected hello_v2, got {other:?}"),
        }
    }

    #[test]
    fn hello_ack_v2_decode_skips_unknown_capabilities() {
        let wire = rmp_serde::to_vec_named(&RawHelloAck {
            hello_ack_v2: RawHelloAckBody {
                selected_protocol_version: 2,
                capabilities: vec!["future_unknown_capability", "compression"],
            },
        })
        .expect("encode raw hello ack");

        let decoded = decode_plain_msg(&wire).expect("decode hello ack with unknown capability");
        match decoded {
            PlainMsg::HelloAckV2(ack) => {
                assert_eq!(ack.capabilities, vec![ProtocolCapabilityV2::Compression]);
            }
            other => panic!("expected hello_ack_v2, got {other:?}"),
        }
    }

    #[derive(Serialize)]
    struct RawErrorBody {
        message: &'static str,
        code: &'static str,
    }

    #[derive(Serialize)]
    struct RawError {
        error: RawErrorBody,
    }

    #[derive(Serialize)]
    struct RawLegacyErrorBody {
        message: &'static str,
    }

    #[derive(Serialize)]
    struct RawLegacyError {
        error: RawLegacyErrorBody,
    }

    #[test]
    fn error_frame_round_trips_stable_code() {
        let wire = encode_frame(&OuterFrame::Error {
            message: "inbound rate limit exceeded".to_string(),
            code: Some(RelayErrorCode::RateLimited),
        })
        .expect("encode error frame");

        match decode_frame(&wire).expect("decode error frame") {
            OuterFrame::Error { message, code } => {
                assert_eq!(message, "inbound rate limit exceeded");
                assert_eq!(code, Some(RelayErrorCode::RateLimited));
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn error_frame_without_code_decodes_as_none() {
        let wire = rmp_serde::to_vec_named(&RawLegacyError {
            error: RawLegacyErrorBody {
                message: "room expired",
            },
        })
        .expect("encode legacy error");

        match decode_frame(&wire).expect("decode legacy error") {
            OuterFrame::Error { message, code } => {
                assert_eq!(message, "room expired");
                assert_eq!(code, None);
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn error_frame_with_unknown_code_decodes_as_none() {
        let wire = rmp_serde::to_vec_named(&RawError {
            error: RawErrorBody {
                message: "something new",
                code: "future_unknown_code",
            },
        })
        .expect("encode raw error");

        match decode_frame(&wire).expect("decode error with unknown code") {
            OuterFrame::Error { message, code } => {
                assert_eq!(message, "something new");
                assert_eq!(code, None);
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn relay_error_code_retryability_matrix() {
        assert!(RelayErrorCode::ServerAtCapacity.is_retryable());
        assert!(RelayErrorCode::JoinTimeout.is_retryable());
        assert!(!RelayErrorCode::PeerNotRegistered.is_retryable());
        assert!(RelayErrorCode::JoinNotificationFailed.is_retryable());
        assert!(RelayErrorCode::HeartbeatTimeout.is_retryable());
        assert!(RelayErrorCode::RateLimited.is_retryable());
        assert!(!RelayErrorCode::InvalidRoomId.is_retryable());
        assert!(!RelayErrorCode::FrameTooLarge.is_retryable());
        assert!(!RelayErrorCode::InvalidJoin.is_retryable());
        assert!(!RelayErrorCode::JoinRoomRoleMismatch.is_retryable());
        assert!(!RelayErrorCode::AdmissionRejected.is_retryable());
        assert!(!RelayErrorCode::RoomExpired.is_retryable());
        assert!(!RelayErrorCode::AppSessionTaken.is_retryable());
    }

    #[test]
    fn resume_accepted_v2_round_trips() {
        let wire = rmp_serde::to_vec_named(&PlainMsg::ResumeAcceptedV2(ResumeAcceptedV2 {
            mode: ResumeAcceptMode::ReplayingPatches,
            target_state_seq: 42,
        }))
        .expect("encode resume accepted");

        match decode_plain_msg(&wire).expect("decode resume accepted") {
            PlainMsg::ResumeAcceptedV2(accepted) => {
                assert_eq!(accepted.mode, ResumeAcceptMode::ReplayingPatches);
                assert_eq!(accepted.target_state_seq, 42);
            }
            other => panic!("expected resume_accepted_v2, got {other:?}"),
        }
    }

    #[test]
    fn protocol_reject_v2_round_trips() {
        let wire = rmp_serde::to_vec_named(&PlainMsg::ProtocolRejectV2(ProtocolRejectV2 {
            reason: "no shared protocol version".to_string(),
            supported_versions: vec![2],
        }))
        .expect("encode protocol reject");

        match decode_plain_msg(&wire).expect("decode protocol reject") {
            PlainMsg::ProtocolRejectV2(reject) => {
                assert_eq!(reject.reason, "no shared protocol version");
                assert_eq!(reject.supported_versions, vec![2]);
            }
            other => panic!("expected protocol_reject_v2, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod android_wire_compat_tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
            .collect()
    }

    /// Byte-for-byte capture of an `App` `Join` produced by the Android client's
    /// hand-written MessagePack encoder (`ProtocolCodec`/`MessagePack.kt`). It
    /// encodes 32-byte fields as a MessagePack *array of uints* rather than a
    /// `bin`, and the relay decodes it with `rmp-serde`. This guards that the
    /// per-connection salt survives that cross-language decode: if it ever
    /// regressed to `None`, the CLI would abort the v3 handshake with "missing
    /// connection salt". Regenerate with `ProtocolCodec.encode(join)` if the
    /// Android wire format intentionally changes.
    #[test]
    fn decodes_android_encoded_join_preserving_connection_salt() {
        let wire = hex(
            "81a46a6f696e86a7726f6f6d5f6964a6726f6f6d2d31a4726f6c65a3617070ad6465766963655f7075626b6579dc00200202020202020202020202020202020202020202020202020202020202020202b370616972696e675f746f6b656e5f70726f6f66dc00200909090909090909090909090909090909090909090909090909090909090909af72656c61795f61646d697373696f6edc00200404040404040404040404040404040404040404040404040404040404040404af636f6e6e656374696f6e5f73616c74dc0020030a11181f262d343b424950575e656c737acc81cc88cc8fcc96cc9dcca4ccabccb2ccb9ccc0ccc7ccceccd5ccdc",
        );
        let expected_salt = hex("030a11181f262d343b424950575e656c737a81888f969da4abb2b9c0c7ced5dc");

        match decode_frame(&wire).expect("decode android-encoded join") {
            OuterFrame::Join {
                room_id,
                role,
                device_pubkey,
                pairing_token_proof,
                relay_admission,
                connection_salt,
                supports_join_accepted: _,
                app_join_intent,
            } => {
                assert_eq!(room_id, "room-1");
                assert_eq!(role, Role::App);
                assert_eq!(device_pubkey, [2u8; 32]);
                assert_eq!(pairing_token_proof, Some([9u8; 32]));
                assert_eq!(app_join_intent, AppJoinIntent::Resume);
                assert_eq!(relay_admission, Some([4u8; 32]));
                assert_eq!(
                    connection_salt.as_ref().map(|s| s.as_slice()),
                    Some(expected_salt.as_slice()),
                    "android-encoded connection_salt must survive the rmp-serde decode"
                );
            }
            other => panic!("expected join, got {other:?}"),
        }
    }
}
