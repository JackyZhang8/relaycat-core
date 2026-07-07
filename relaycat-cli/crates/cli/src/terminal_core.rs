use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use relaycat_protocol::{
    CellAttr, CellRun, CursorState, CursorStyle, PaletteState, PatchOp, RenderAckV2,
    RequestTranscriptV2, ResizeAckV2, ResizeEventV2, ResumeV2, TerminalCell, TerminalColor,
    TerminalMode, TerminalModes, TerminalPatchV2, TerminalRow, TerminalSnapshotV2,
    TerminalTranscriptEntryKind, TerminalTranscriptEntryV2, TranscriptChunkV2,
};
use relaycat_protocol::{
    PlainMsg, terminal_patch_v2_encoded_len, terminal_snapshot_v2_encoded_len,
    transcript_chunk_v2_encoded_len,
};

// Helper modules re-export free functions; the `impl TerminalCore` modules
// (grid, snapshot, vt_sync) only add methods, so they need no re-export.
mod color;
mod grid;
mod patches;
mod rows;
mod scroll_region;
mod snapshot;
mod vt_screen;
mod vt_sync;

pub(crate) use color::*;
pub(crate) use patches::*;
pub(crate) use rows::*;
pub(crate) use scroll_region::*;
pub(crate) use vt_screen::*;

const TERMINAL_SNAPSHOT_SCROLLBACK_SCREENS: usize = 12;
const TERMINAL_RESUME_PATCH_REPLAY_MAX_SCREENS: usize = 12;
const TERMINAL_RELAY_SAFE_PLAIN_MSG_BYTES: usize = 960 * 1024;
const TERMINAL_TRANSCRIPT_MAX_ENTRIES: usize = 2_000;
/// Latest scrollback rows retained on the CLI for reconnect snapshots. The app
/// never pages older history from the CLI; ordinary upward scrolling is limited
/// to the app's local cache.
const TERMINAL_HISTORY_MAX_ROWS: usize = 2048;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalCoreConfig {
    pub terminal_run_id: String,
    pub cols: u16,
    pub rows: u16,
    pub patch_retention: usize,
}

pub struct TerminalCore {
    terminal_run_id: String,
    active_snapshot_id: u64,
    next_snapshot_id: u64,
    snapshot_emitted: bool,
    state_seq: u64,
    cols: u16,
    rows: u16,
    title: String,
    cursor: CursorState,
    modes: TerminalModes,
    palette: PaletteState,
    // Append-only, deduplicated attribute table for the current terminal run. Stable
    // `attr_id`s let retained scrollback rows reference attributes that were
    // first seen long ago, and let patches re-send the table only when it grows.
    attrs: Vec<CellAttr>,
    // Reverse lookup from attribute to its `attr_id`, kept in lock-step with
    // `attrs`, so interning a cell's attributes is O(1) instead of a linear
    // scan of the (only-growing) table on every cell.
    attr_index: HashMap<CellAttr, u32>,
    // Number of attribute entries already sent to the app (via the most recent
    // snapshot or patch). Patches only re-send the table when it has grown.
    emitted_attrs_len: usize,
    // When set (peer negotiated `ProtocolCapabilityV2::IncrementalAttrs`), a
    // patch that grows the table carries only the appended tail plus its base
    // index, instead of re-sending the whole table. Shared with the relay's
    // decode path, which flips it on once the app's Hello is seen.
    incremental_attrs: Arc<AtomicBool>,
    // Number of rows in the vt100 parser's own scrollback that have already
    // been folded into `history`. The parser scrollback only grows at the
    // bottom as the screen scrolls and is never reflowed on resize, so this
    // count deterministically identifies the rows that newly scrolled off
    // since the last sync — no content-alignment guessing required.
    scrollback_seen: usize,
    // While `true`, scrollback that scrolls off the screen is discarded instead
    // of being folded into `history`. The relay freezes around explicit
    // LocalMode/RemoteMode PTY resizes so TUI repaint churn does not enter the
    // app-facing scrollback. It thaws immediately after the resize model is
    // synchronized, so real user output in the active mode is still retained.
    history_frozen: bool,
    // Latest scrollback window (oldest first) with stable `line_id`s, streamed
    // incrementally to the app and used to seed reconnect snapshots.
    history: VecDeque<TerminalRow>,
    // Rows that scrolled off during the most recent sync, awaiting emission as a
    // `PatchOp::AppendScrollback`.
    pending_scrollback_append: Vec<TerminalRow>,
    transcript_store: TerminalTranscriptStore,
    record_primary_screen_frames: bool,
    scroll_region_history: Option<TrackedScrollRegion>,
    screen_rows: Vec<TerminalRow>,
    next_line_id: u64,
    retained_patches: VecDeque<TerminalPatchV2>,
    patch_retention: usize,
    parser: vt100::Parser<TerminalCallbacks>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalCoreDebugSnapshot {
    pub history_len: usize,
    pub scrollback_seen: usize,
    pub vt_scrollback_len: usize,
    pub history_frozen: bool,
    pub alt_screen: bool,
    pub cols: u16,
    pub rows: u16,
    pub state_seq: u64,
}

#[derive(Debug, Default)]
struct TerminalCallbacks {
    title: String,
    bell: bool,
}

pub(crate) struct PreviousTerminalState {
    rows: Vec<TerminalRow>,
    cursor: CursorState,
    title: String,
    modes: TerminalModes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TrackedScrollRegion {
    top: u16,
    bottom: u16,
}

struct TerminalTranscriptStore {
    entries: VecDeque<TerminalTranscriptEntryV2>,
    next_entry_id: u64,
    last_screen_frame_rows: Option<Vec<TerminalRow>>,
}

impl TerminalTranscriptStore {
    fn new() -> Self {
        Self {
            entries: VecDeque::new(),
            next_entry_id: 1,
            last_screen_frame_rows: None,
        }
    }

    fn chunk(
        &self,
        request: &RequestTranscriptV2,
        terminal_run_id: &str,
        attrs: Vec<CellAttr>,
    ) -> TranscriptChunkV2 {
        let max_entries = request.max_entries as usize;
        let end = match request.before_entry_id {
            Some(before) => self
                .entries
                .iter()
                .position(|entry| entry.entry_id >= before)
                .unwrap_or(self.entries.len()),
            None => self.entries.len(),
        };
        let start = end.saturating_sub(max_entries);
        let entries = self.entries.iter().take(end).skip(start).cloned().collect();

        let mut chunk = TranscriptChunkV2 {
            terminal_run_id: terminal_run_id.to_owned(),
            before_entry_id: request.before_entry_id,
            entries,
            attrs,
            has_more: start > 0,
        };
        // Trim to the relay budget by dropping the oldest entries; the app can
        // page for them with a follow-up request, whereas an oversized chunk
        // would be dropped whole and stall transcript paging.
        while chunk.entries.len() > 1
            && transcript_chunk_v2_encoded_len(&chunk) > TERMINAL_RELAY_SAFE_PLAIN_MSG_BYTES
        {
            let remove = (chunk.entries.len() / 4).max(1);
            chunk.entries.drain(..remove);
            chunk.has_more = true;
        }
        chunk
    }

    fn clear_screen_frame_dedupe(&mut self) {
        self.last_screen_frame_rows = None;
    }

    fn record_screen_frame(
        &mut self,
        kind: TerminalTranscriptEntryKind,
        screen_rows: &[TerminalRow],
        state_seq: u64,
        terminal_run_id: &str,
        cols: u16,
    ) {
        if transcript_rows_are_blank(screen_rows) {
            return;
        }
        if self.last_screen_frame_rows.as_ref().is_some_and(|rows| {
            rows.len() == screen_rows.len()
                && rows
                    .iter()
                    .zip(screen_rows)
                    .all(|(left, right)| rows_render_equal(left, right))
        }) {
            return;
        }
        let rows = screen_rows.to_vec();
        self.last_screen_frame_rows = Some(rows.clone());
        self.record_rows(kind, rows, state_seq, terminal_run_id, cols);
    }

    fn record_rows(
        &mut self,
        kind: TerminalTranscriptEntryKind,
        rows: Vec<TerminalRow>,
        state_seq: u64,
        terminal_run_id: &str,
        cols: u16,
    ) {
        if rows.is_empty() {
            return;
        }
        let entry = TerminalTranscriptEntryV2 {
            entry_id: self.next_entry_id,
            terminal_run_id: terminal_run_id.to_owned(),
            state_seq,
            kind,
            cols,
            rows,
            captured_at_unix_ms: unix_time_ms(),
        };
        self.next_entry_id = self.next_entry_id.saturating_add(1);
        self.entries.push_back(entry);
        while self.entries.len() > TERMINAL_TRANSCRIPT_MAX_ENTRIES {
            self.entries.pop_front();
        }
    }
}

impl vt100::Callbacks for TerminalCallbacks {
    fn audible_bell(&mut self, _: &mut vt100::Screen) {
        self.bell = true;
    }

    fn visual_bell(&mut self, _: &mut vt100::Screen) {
        self.bell = true;
    }

    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        self.title = String::from_utf8_lossy(title).into_owned();
    }
}

impl TerminalCore {
    pub fn new(config: TerminalCoreConfig) -> Self {
        Self::new_with_palette(config, default_terminal_palette())
    }

    pub fn new_with_palette(config: TerminalCoreConfig, palette: PaletteState) -> Self {
        let attrs = vec![CellAttr::default()];
        let mut attr_index = HashMap::new();
        attr_index.insert(CellAttr::default(), 0_u32);
        let mut next_line_id = 1;
        let screen_rows = blank_rows(config.rows, config.cols, &mut next_line_id);

        Self {
            terminal_run_id: config.terminal_run_id,
            active_snapshot_id: 1,
            next_snapshot_id: 2,
            snapshot_emitted: false,
            state_seq: 0,
            cols: config.cols,
            rows: config.rows,
            title: String::new(),
            cursor: CursorState {
                row: 0,
                col: 0,
                visible: true,
                style: CursorStyle::Block,
            },
            modes: TerminalModes {
                alt_screen: false,
                bracketed_paste: false,
                application_cursor: false,
            },
            palette,
            attrs,
            attr_index,
            emitted_attrs_len: 1,
            incremental_attrs: Arc::new(AtomicBool::new(false)),
            scrollback_seen: 0,
            history_frozen: false,
            history: VecDeque::new(),
            pending_scrollback_append: Vec::new(),
            transcript_store: TerminalTranscriptStore::new(),
            record_primary_screen_frames: false,
            scroll_region_history: None,
            screen_rows,
            next_line_id,
            retained_patches: VecDeque::new(),
            patch_retention: config.patch_retention,
            parser: vt100::Parser::new_with_callbacks(
                config.rows,
                config.cols,
                TERMINAL_HISTORY_MAX_ROWS,
                TerminalCallbacks::default(),
            ),
        }
    }

    /// Freeze the app-facing scrollback history. While frozen, rows that scroll
    /// off the screen are discarded instead of being appended to `history`. The
    /// relay calls this whenever it resizes the PTY while the app is
    /// disconnected, so the TUI's repaint churn at the new width never reaches
    /// the app. Idempotent.
    pub fn freeze_history(&mut self) {
        self.history_frozen = true;
    }

    pub fn set_record_primary_screen_frames(&mut self, enabled: bool) {
        self.record_primary_screen_frames = enabled;
    }

    /// Share the per-connection "incremental attrs negotiated" flag with the
    /// relay's decode path. While the flag is set, patches that grow the
    /// attribute table carry only the appended tail (see
    /// `ProtocolCapabilityV2::IncrementalAttrs`).
    pub fn set_incremental_attrs_flag(&mut self, flag: Arc<AtomicBool>) {
        self.incremental_attrs = flag;
    }

    fn incremental_attrs_enabled(&self) -> bool {
        self.incremental_attrs.load(Ordering::Relaxed)
    }

    /// Thaw the scrollback history so newly scrolled-off rows are appended to
    /// `history` again. The relay calls this on the first render ack after the
    /// app reconnects — by then the reconnect repaint has been consumed and
    /// discarded, so only genuinely new output is recorded from here on.
    /// Idempotent.
    pub fn thaw_history(&mut self) {
        self.history_frozen = false;
    }

    /// Whether the scrollback history is currently frozen (test/diagnostic).
    pub fn history_frozen(&self) -> bool {
        self.history_frozen
    }

    pub(crate) fn debug_snapshot(&self) -> TerminalCoreDebugSnapshot {
        let mut screen = self.parser.screen().clone();
        screen.set_scrollback(usize::MAX);
        TerminalCoreDebugSnapshot {
            history_len: self.history.len(),
            scrollback_seen: self.scrollback_seen,
            vt_scrollback_len: screen.scrollback(),
            history_frozen: self.history_frozen,
            alt_screen: self.modes.alt_screen,
            cols: self.cols,
            rows: self.rows,
            state_seq: self.state_seq,
        }
    }

    pub fn apply_ops(&mut self, ops: Vec<PatchOp>) -> TerminalPatchV2 {
        for op in &ops {
            self.apply_op(op);
        }

        self.emit_patch(ops)
    }

    pub fn feed_vt_bytes(&mut self, bytes: &[u8]) -> Option<TerminalPatchV2> {
        let scroll_region_for_history = self.observe_scroll_region_controls(bytes);
        self.parser.process(bytes);
        self.sync_from_vt_screen(scroll_region_for_history)
    }
}
