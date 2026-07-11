use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use relaycat_protocol::{
    CellAttr, CellRun, CursorState, CursorStyle, PaletteState, PatchOp, RenderAckV2,
    RequestTranscriptV2, ResizeAckV2, ResizeEventV2, ResumeV2, TerminalCell, TerminalColor,
    TerminalMode, TerminalModes, TerminalPatchV2, TerminalRow, TerminalSnapshotV2,
    TerminalTranscriptEntryKind, TerminalTranscriptEntryV2, TerminalTranscriptFrameFragmentV2,
    TranscriptChunkV2,
};
use relaycat_protocol::{
    PlainMsg, terminal_patch_v2_encoded_len, terminal_row_v2_encoded_len,
    terminal_snapshot_v2_encoded_len, terminal_transcript_entry_v2_encoded_len,
    terminal_transcript_entry_v2_parts_encoded_len, transcript_chunk_v2_encoded_len,
    transcript_chunk_v2_parts_encoded_len,
};

// Helper modules re-export free functions; the `impl TerminalCore` modules
// (grid, snapshot, vt_sync) only add methods, so they need no re-export.
mod color;
mod grid;
mod patches;
mod rows;
mod snapshot;
mod vt_screen;
mod vt_sync;

pub(crate) use color::*;
pub(crate) use patches::*;
pub(crate) use rows::*;
pub(crate) use vt_screen::*;

const TERMINAL_SNAPSHOT_SCROLLBACK_SCREENS: usize = 12;
const TERMINAL_RELAY_SAFE_PLAIN_MSG_BYTES: usize = 960 * 1024;
const TERMINAL_TRANSCRIPT_MAX_ENTRIES: usize = 2_000;
// Leaves room for a complete 4096-entry attribute table plus chunk metadata.
// Transcript chunks must carry the full table because existing apps replace,
// rather than merge, attrs when prepending an older page.
const TERMINAL_TRANSCRIPT_MAX_ENTRY_ENCODED_BYTES: usize = 512 * 1024;
// A maximum 262144-cell screen with 64-byte cell text and one high attr-id run
// per cell encodes to about 24.7 MiB after row-boundary fragmentation. Keep one
// complete worst-case frame while retaining a simple, bounded power-of-two cap.
const TERMINAL_TRANSCRIPT_MAX_TOTAL_ENCODED_BYTES: usize = 32 * 1024 * 1024;
const TERMINAL_TRANSCRIPT_MAX_TOTAL_CELLS: usize = 4 * MAX_TERMINAL_CELLS;
pub(crate) const MAX_TERMINAL_COLS: u16 = 4_096;
pub(crate) const MAX_TERMINAL_ROWS: u16 = 4_096;
pub(crate) const MAX_TERMINAL_CELLS: usize = 262_144;
pub(crate) const MAX_TERMINAL_TITLE_BYTES: usize = 4 * 1024;
pub(crate) const MAX_TERMINAL_CELL_TEXT_BYTES: usize = 64;
/// Latest scrollback rows retained on the CLI for reconnect snapshots. The app
/// never pages older history from the CLI; ordinary upward scrolling is limited
/// to the app's local cache.
const TERMINAL_HISTORY_MAX_ROWS: usize = 2048;
const TERMINAL_HISTORY_MAX_CELLS: usize = MAX_TERMINAL_CELLS;

pub(crate) fn bounded_terminal_size(cols: u16, rows: u16) -> (u16, u16) {
    let cols = cols.clamp(1, MAX_TERMINAL_COLS);
    let rows = rows.clamp(1, MAX_TERMINAL_ROWS);
    let max_rows_for_cols = (MAX_TERMINAL_CELLS / usize::from(cols)).max(1);
    let max_rows_for_cols = u16::try_from(max_rows_for_cols).unwrap_or(MAX_TERMINAL_ROWS);
    (cols, rows.min(max_rows_for_cols).min(MAX_TERMINAL_ROWS))
}

pub(crate) fn terminal_history_max_rows(cols: u16) -> usize {
    let cols = usize::from(cols.max(1));
    TERMINAL_HISTORY_MAX_ROWS.min((TERMINAL_HISTORY_MAX_CELLS / cols).max(1))
}

pub(crate) fn truncate_utf8(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    &value[..end]
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalWireError {
    message: String,
}

impl TerminalWireError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for TerminalWireError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for TerminalWireError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalCoreConfig {
    pub terminal_run_id: String,
    pub cols: u16,
    pub rows: u16,
    pub patch_retention: usize,
}

#[derive(Debug, Default)]
pub struct TerminalFeedBatch {
    pub reset_messages: Vec<PlainMsg>,
    pub patches: Vec<TerminalPatchV2>,
}

impl TerminalFeedBatch {
    pub fn has_reset(&self) -> bool {
        !self.reset_messages.is_empty()
    }

    pub fn into_messages(mut self) -> Vec<PlainMsg> {
        self.reset_messages
            .extend(self.patches.into_iter().map(PlainMsg::TerminalPatchV2));
        self.reset_messages
    }
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
    screen_rows: Vec<TerminalRow>,
    next_line_id: u64,
    retained_patches: VecDeque<TerminalPatchV2>,
    patch_retention: usize,
    parser: vt100::Parser<TerminalCallbacks>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalCoreDebugSnapshot {
    pub history_len: usize,
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
    scrollback_cleared: bool,
}

struct TerminalTranscriptStore {
    entries: VecDeque<TerminalTranscriptEntryV2>,
    next_entry_id: u64,
    last_screen_frame: Option<StoredScreenFrame>,
    total_encoded_bytes: usize,
    total_cells: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredScreenFrame {
    frame_id: u64,
    kind: TerminalTranscriptEntryKind,
    cols: u16,
    fragment_count: u32,
    row_count: usize,
    fingerprint: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScreenFrameFragmentPlan {
    rows: Range<usize>,
    encoded_bytes: usize,
    cells: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScreenFramePlan {
    frame_id: u64,
    fragments: Vec<ScreenFrameFragmentPlan>,
    total_encoded_bytes: usize,
    total_cells: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TranscriptChunkPlan {
    start: usize,
    end: usize,
    has_more: bool,
    encoded_bytes: usize,
}

impl TerminalTranscriptStore {
    fn new() -> Self {
        Self {
            entries: VecDeque::new(),
            next_entry_id: 1,
            last_screen_frame: None,
            total_encoded_bytes: 0,
            total_cells: 0,
        }
    }

    fn chunk(
        &self,
        request: &RequestTranscriptV2,
        terminal_run_id: &str,
        attrs: Vec<CellAttr>,
    ) -> TranscriptChunkV2 {
        let plan = self.plan_chunk(request, terminal_run_id, &attrs);
        let entries = self
            .entries
            .iter()
            .take(plan.end)
            .skip(plan.start)
            .cloned()
            .collect::<Vec<_>>();
        let chunk = TranscriptChunkV2 {
            terminal_run_id: terminal_run_id.to_owned(),
            before_entry_id: request.before_entry_id,
            entries,
            attrs,
            has_more: plan.has_more,
        };
        let encoded_bytes = transcript_chunk_v2_encoded_len(&chunk);
        assert_eq!(
            encoded_bytes, plan.encoded_bytes,
            "borrowed transcript chunk plan diverged from final serialization"
        );
        assert!(
            encoded_bytes <= TERMINAL_RELAY_SAFE_PLAIN_MSG_BYTES,
            "stored transcript entry exceeded its single-entry chunk budget"
        );
        chunk
    }

    fn plan_chunk(
        &self,
        request: &RequestTranscriptV2,
        terminal_run_id: &str,
        attrs: &[CellAttr],
    ) -> TranscriptChunkPlan {
        // Walk backward over borrowed entries and stop on the first oversized
        // candidate. This bounds content scans to one relay page plus one
        // rejected entry and delays cloning until the final suffix is known.
        let max_entries = (request.max_entries as usize).max(1);
        let end = match request.before_entry_id {
            Some(before) => self
                .entries
                .iter()
                .position(|entry| entry.entry_id >= before)
                .unwrap_or(self.entries.len()),
            None => self.entries.len(),
        };
        let minimum_start = end.saturating_sub(max_entries);
        let empty_chunk_bytes = transcript_chunk_v2_parts_encoded_len(
            terminal_run_id,
            request.before_entry_id,
            &[],
            attrs,
            false,
        );
        // Both MessagePack booleans occupy one byte, so this exact empty-entry
        // envelope also applies when the final `has_more` value is true.
        let fixed_bytes = empty_chunk_bytes
            .checked_sub(messagepack_array_header_encoded_len(0))
            .expect("empty transcript entries array has an encoded header");
        let mut start = end;
        let mut entries_bytes = 0usize;
        let mut encoded_bytes = empty_chunk_bytes;
        for candidate_start in (minimum_start..end).rev() {
            let candidate_entries_bytes = entries_bytes
                .checked_add(terminal_transcript_entry_v2_encoded_len(
                    &self.entries[candidate_start],
                ))
                .unwrap_or(usize::MAX);
            let candidate_entry_count = end.saturating_sub(candidate_start);
            let candidate_encoded_bytes = fixed_bytes
                .checked_add(messagepack_array_header_encoded_len(candidate_entry_count))
                .and_then(|bytes| bytes.checked_add(candidate_entries_bytes))
                .unwrap_or(usize::MAX);
            if candidate_encoded_bytes > TERMINAL_RELAY_SAFE_PLAIN_MSG_BYTES {
                break;
            }
            start = candidate_start;
            entries_bytes = candidate_entries_bytes;
            encoded_bytes = candidate_encoded_bytes;
        }
        assert!(
            end == 0 || start < end,
            "stored transcript entry exceeded its single-entry chunk budget"
        );
        TranscriptChunkPlan {
            start,
            end,
            has_more: start > 0,
            encoded_bytes,
        }
    }

    fn clear_screen_frame_dedupe(&mut self) {
        self.last_screen_frame = None;
    }

    fn last_screen_frame_matches(
        &self,
        kind: TerminalTranscriptEntryKind,
        cols: u16,
        screen_rows: &[TerminalRow],
        fingerprint: u64,
    ) -> bool {
        let Some(last) = self.last_screen_frame.as_ref() else {
            return false;
        };
        if last.kind != kind
            || last.cols != cols
            || last.row_count != screen_rows.len()
            || last.fingerprint != fingerprint
        {
            return false;
        }

        let mut expected_fragment_index = 0_u32;
        let mut row_index = 0usize;
        for entry in self
            .entries
            .iter()
            .filter(|entry| logical_transcript_frame_id(entry) == last.frame_id)
        {
            if entry.kind != kind {
                return false;
            }
            if last.fragment_count == 1 {
                if entry.frame_fragment.is_some() || entry.entry_id != last.frame_id {
                    return false;
                }
            } else {
                let Some(fragment) = entry.frame_fragment.as_ref() else {
                    return false;
                };
                if fragment.frame_id != last.frame_id
                    || fragment.fragment_index != expected_fragment_index
                    || fragment.fragment_count != last.fragment_count
                {
                    return false;
                }
            }
            expected_fragment_index = expected_fragment_index.saturating_add(1);

            for stored_row in &entry.rows {
                let Some(screen_row) = screen_rows.get(row_index) else {
                    return false;
                };
                if !rows_render_equal(stored_row, screen_row) {
                    return false;
                }
                row_index = row_index.saturating_add(1);
            }
        }

        expected_fragment_index == last.fragment_count && row_index == screen_rows.len()
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
        let fingerprint = screen_frame_fingerprint(screen_rows);
        if self.last_screen_frame_matches(kind, cols, screen_rows, fingerprint) {
            return;
        }
        let captured_at_unix_ms = unix_time_ms();
        let Some(plan) = self.plan_screen_frame(
            kind,
            screen_rows,
            state_seq,
            terminal_run_id,
            cols,
            captured_at_unix_ms,
        ) else {
            return;
        };
        if plan.fragments.len() > TERMINAL_TRANSCRIPT_MAX_ENTRIES
            || plan.total_encoded_bytes > TERMINAL_TRANSCRIPT_MAX_TOTAL_ENCODED_BYTES
            || plan.total_cells > TERMINAL_TRANSCRIPT_MAX_TOTAL_CELLS
        {
            return;
        }

        let fragment_count = u32::try_from(plan.fragments.len()).expect("fragment count planned");
        for (fragment_index, fragment) in plan.fragments.iter().enumerate() {
            let fragment_index = u32::try_from(fragment_index).expect("fragment index planned");
            let frame_fragment =
                (fragment_count > 1).then_some(TerminalTranscriptFrameFragmentV2 {
                    frame_id: plan.frame_id,
                    fragment_index,
                    fragment_count,
                });
            let entry_id = plan
                .frame_id
                .checked_add(u64::from(fragment_index))
                .expect("fragment entry id planned");
            let entry = Self::entry_with_rows_and_fragment(
                entry_id,
                kind,
                screen_rows[fragment.rows.clone()].to_vec(),
                state_seq,
                terminal_run_id,
                cols,
                captured_at_unix_ms,
                frame_fragment,
            );
            debug_assert_eq!(
                terminal_transcript_entry_v2_encoded_len(&entry),
                fragment.encoded_bytes
            );
            debug_assert_eq!(transcript_entry_cell_count(&entry), fragment.cells);
            self.commit_entry(entry);
        }
        self.last_screen_frame = Some(StoredScreenFrame {
            frame_id: plan.frame_id,
            kind,
            cols,
            fragment_count,
            row_count: screen_rows.len(),
            fingerprint,
        });
    }

    fn plan_screen_frame(
        &self,
        kind: TerminalTranscriptEntryKind,
        screen_rows: &[TerminalRow],
        state_seq: u64,
        terminal_run_id: &str,
        cols: u16,
        captured_at_unix_ms: u64,
    ) -> Option<ScreenFramePlan> {
        if screen_rows.is_empty() {
            return None;
        }

        let row_encoded_bytes = screen_rows
            .iter()
            .map(terminal_row_v2_encoded_len)
            .collect::<Vec<_>>();
        let row_cells = screen_rows
            .iter()
            .map(transcript_row_cell_count)
            .collect::<Vec<_>>();
        let worst_fragment = TerminalTranscriptFrameFragmentV2 {
            frame_id: u64::MAX,
            fragment_index: u32::MAX,
            fragment_count: u32::MAX,
        };
        let worst_empty_entry_bytes = terminal_transcript_entry_v2_parts_encoded_len(
            u64::MAX,
            terminal_run_id,
            u64::MAX,
            kind,
            cols,
            &[],
            u64::MAX,
            Some(&worst_fragment),
        );
        // An empty MessagePack array is one byte. Replace it with the candidate
        // row-array header and the exact precomputed row payload lengths.
        let worst_fixed_bytes = worst_empty_entry_bytes.checked_sub(1)?;

        let mut ranges = Vec::new();
        let mut offset = 0usize;
        while offset < screen_rows.len() {
            let mut end = offset;
            let mut rows_bytes = 0usize;
            let mut cells = 0usize;
            while end < screen_rows.len() {
                let candidate_rows_bytes = rows_bytes.checked_add(row_encoded_bytes[end])?;
                let candidate_cells = cells.checked_add(row_cells[end])?;
                let candidate_row_count = end.saturating_sub(offset).saturating_add(1);
                let candidate_encoded_bytes = worst_fixed_bytes
                    .checked_add(messagepack_array_header_encoded_len(candidate_row_count))?
                    .checked_add(candidate_rows_bytes)?;
                if candidate_encoded_bytes > TERMINAL_TRANSCRIPT_MAX_ENTRY_ENCODED_BYTES
                    || candidate_cells > MAX_TERMINAL_CELLS
                {
                    break;
                }
                rows_bytes = candidate_rows_bytes;
                cells = candidate_cells;
                end = end.saturating_add(1);
            }
            if end == offset {
                return None;
            }
            ranges.push((offset..end, rows_bytes, cells));
            offset = end;
        }

        let fragment_count = u32::try_from(ranges.len()).ok()?;
        self.next_entry_id
            .checked_add(u64::from(fragment_count.saturating_sub(1)))?;
        let fragmented = fragment_count > 1;
        let mut fragments = Vec::with_capacity(ranges.len());
        let mut total_encoded_bytes = 0usize;
        let mut total_cells = 0usize;
        for (fragment_index, (rows, rows_bytes, cells)) in ranges.into_iter().enumerate() {
            let fragment_index = u32::try_from(fragment_index).ok()?;
            let entry_id = self.next_entry_id.checked_add(u64::from(fragment_index))?;
            let frame_fragment = fragmented.then_some(TerminalTranscriptFrameFragmentV2 {
                frame_id: self.next_entry_id,
                fragment_index,
                fragment_count,
            });
            let empty_entry_bytes = terminal_transcript_entry_v2_parts_encoded_len(
                entry_id,
                terminal_run_id,
                state_seq,
                kind,
                cols,
                &[],
                captured_at_unix_ms,
                frame_fragment.as_ref(),
            );
            let encoded_bytes = empty_entry_bytes
                .checked_sub(1)?
                .checked_add(messagepack_array_header_encoded_len(rows.len()))?
                .checked_add(rows_bytes)?;
            debug_assert!(encoded_bytes <= TERMINAL_TRANSCRIPT_MAX_ENTRY_ENCODED_BYTES);
            total_encoded_bytes = total_encoded_bytes.checked_add(encoded_bytes)?;
            total_cells = total_cells.checked_add(cells)?;
            fragments.push(ScreenFrameFragmentPlan {
                rows,
                encoded_bytes,
                cells,
            });
        }

        Some(ScreenFramePlan {
            frame_id: self.next_entry_id,
            fragments,
            total_encoded_bytes,
            total_cells,
        })
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
        let captured_at_unix_ms = unix_time_ms();
        let mut offset = 0usize;
        while offset < rows.len() {
            let mut low = 1usize;
            let mut high = rows.len() - offset;
            let mut best = 0usize;
            while low <= high {
                let middle = low + (high - low) / 2;
                let entry = Self::entry_with_rows(
                    self.next_entry_id,
                    kind,
                    rows[offset..offset + middle].to_vec(),
                    state_seq,
                    terminal_run_id,
                    cols,
                    captured_at_unix_ms,
                );
                if Self::entry_fits_storage(&entry) {
                    best = middle;
                    low = middle.saturating_add(1);
                } else {
                    high = middle.saturating_sub(1);
                }
            }
            if best == 0 {
                offset += 1;
                continue;
            }
            let entry = Self::entry_with_rows(
                self.next_entry_id,
                kind,
                rows[offset..offset + best].to_vec(),
                state_seq,
                terminal_run_id,
                cols,
                captured_at_unix_ms,
            );
            self.commit_entry(entry);
            offset += best;
        }
    }

    fn entry_with_rows(
        entry_id: u64,
        kind: TerminalTranscriptEntryKind,
        rows: Vec<TerminalRow>,
        state_seq: u64,
        terminal_run_id: &str,
        cols: u16,
        captured_at_unix_ms: u64,
    ) -> TerminalTranscriptEntryV2 {
        Self::entry_with_rows_and_fragment(
            entry_id,
            kind,
            rows,
            state_seq,
            terminal_run_id,
            cols,
            captured_at_unix_ms,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn entry_with_rows_and_fragment(
        entry_id: u64,
        kind: TerminalTranscriptEntryKind,
        rows: Vec<TerminalRow>,
        state_seq: u64,
        terminal_run_id: &str,
        cols: u16,
        captured_at_unix_ms: u64,
        frame_fragment: Option<TerminalTranscriptFrameFragmentV2>,
    ) -> TerminalTranscriptEntryV2 {
        TerminalTranscriptEntryV2 {
            entry_id,
            terminal_run_id: terminal_run_id.to_owned(),
            state_seq,
            kind,
            cols,
            rows,
            captured_at_unix_ms,
            frame_fragment,
        }
    }

    fn entry_fits_storage(entry: &TerminalTranscriptEntryV2) -> bool {
        terminal_transcript_entry_v2_encoded_len(entry)
            <= TERMINAL_TRANSCRIPT_MAX_ENTRY_ENCODED_BYTES
            && transcript_entry_cell_count(entry) <= MAX_TERMINAL_CELLS
    }

    fn commit_entry(&mut self, entry: TerminalTranscriptEntryV2) {
        self.next_entry_id = self.next_entry_id.saturating_add(1);
        let encoded_bytes = terminal_transcript_entry_v2_encoded_len(&entry);
        let cells = transcript_entry_cell_count(&entry);
        self.total_encoded_bytes = self.total_encoded_bytes.saturating_add(encoded_bytes);
        self.total_cells = self.total_cells.saturating_add(cells);
        self.entries.push_back(entry);

        while self.entries.len() > TERMINAL_TRANSCRIPT_MAX_ENTRIES
            || self.total_encoded_bytes > TERMINAL_TRANSCRIPT_MAX_TOTAL_ENCODED_BYTES
            || self.total_cells > TERMINAL_TRANSCRIPT_MAX_TOTAL_CELLS
        {
            if !self.evict_oldest_logical_entry() {
                break;
            }
        }
    }

    fn evict_oldest_logical_entry(&mut self) -> bool {
        let Some(fragmented_frame_id) = self
            .entries
            .front()
            .and_then(|entry| entry.frame_fragment.as_ref())
            .map(|fragment| fragment.frame_id)
        else {
            return self.remove_oldest_entry();
        };

        let mut removed_any = false;
        while self.entries.front().is_some_and(|entry| {
            entry
                .frame_fragment
                .as_ref()
                .is_some_and(|fragment| fragment.frame_id == fragmented_frame_id)
        }) {
            removed_any |= self.remove_oldest_entry();
        }
        removed_any
    }

    fn remove_oldest_entry(&mut self) -> bool {
        let Some(removed) = self.entries.pop_front() else {
            return false;
        };
        let removed_frame_id = logical_transcript_frame_id(&removed);
        if self
            .last_screen_frame
            .as_ref()
            .is_some_and(|last| last.frame_id == removed_frame_id)
        {
            self.last_screen_frame = None;
        }
        self.total_encoded_bytes = self
            .total_encoded_bytes
            .saturating_sub(terminal_transcript_entry_v2_encoded_len(&removed));
        self.total_cells = self
            .total_cells
            .saturating_sub(transcript_entry_cell_count(&removed));
        true
    }
}

fn transcript_entry_cell_count(entry: &TerminalTranscriptEntryV2) -> usize {
    transcript_rows_cell_count(&entry.rows)
}

fn transcript_rows_cell_count(rows: &[TerminalRow]) -> usize {
    rows.iter()
        .flat_map(|row| &row.cells)
        .map(|run| run.cells.len())
        .fold(0usize, usize::saturating_add)
}

fn transcript_row_cell_count(row: &TerminalRow) -> usize {
    row.cells
        .iter()
        .map(|run| run.cells.len())
        .fold(0usize, usize::saturating_add)
}

fn logical_transcript_frame_id(entry: &TerminalTranscriptEntryV2) -> u64 {
    entry
        .frame_fragment
        .as_ref()
        .map_or(entry.entry_id, |fragment| fragment.frame_id)
}

fn messagepack_array_header_encoded_len(len: usize) -> usize {
    if len <= 15 {
        1
    } else if u16::try_from(len).is_ok() {
        3
    } else {
        5
    }
}

fn screen_frame_fingerprint(rows: &[TerminalRow]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    fn mix_bytes(mut hash: u64, bytes: &[u8]) -> u64 {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(PRIME);
        }
        hash
    }

    fn mix_u64(hash: u64, value: u64) -> u64 {
        mix_bytes(hash, &value.to_le_bytes())
    }

    let mut hash = mix_u64(OFFSET_BASIS, rows.len() as u64);
    for row in rows {
        hash = mix_u64(hash, u64::from(row.wrapped));
        hash = mix_u64(hash, row.cells.len() as u64);
        for run in &row.cells {
            hash = mix_u64(hash, u64::from(run.attr_id));
            hash = mix_u64(hash, run.cells.len() as u64);
            for cell in &run.cells {
                hash = mix_u64(hash, u64::from(cell.width));
                hash = mix_u64(hash, cell.text.len() as u64);
                hash = mix_bytes(hash, cell.text.as_bytes());
            }
        }
    }
    hash
}

impl vt100::Callbacks for TerminalCallbacks {
    fn audible_bell(&mut self, _: &mut vt100::Screen) {
        self.bell = true;
    }

    fn visual_bell(&mut self, _: &mut vt100::Screen) {
        self.bell = true;
    }

    fn terminal_reset(&mut self, _: &mut vt100::Screen) {
        self.bell = false;
    }

    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        let title = String::from_utf8_lossy(title);
        self.title = truncate_utf8(&title, MAX_TERMINAL_TITLE_BYTES).to_string();
    }
}

impl TerminalCore {
    pub fn new(config: TerminalCoreConfig) -> Self {
        Self::new_with_palette(config, default_terminal_palette())
    }

    pub fn new_with_palette(config: TerminalCoreConfig, palette: PaletteState) -> Self {
        let (cols, rows) = bounded_terminal_size(config.cols, config.rows);
        let attrs = vec![CellAttr::default()];
        let mut attr_index = HashMap::new();
        attr_index.insert(CellAttr::default(), 0_u32);
        let mut next_line_id = 1;
        let screen_rows = blank_rows(rows, cols, &mut next_line_id);
        let mut parser = vt100::Parser::new_with_callbacks(
            rows,
            cols,
            terminal_history_max_rows(cols),
            TerminalCallbacks::default(),
        );
        parser.screen_mut().set_scrollback_updates_enabled(true);

        Self {
            terminal_run_id: config.terminal_run_id,
            active_snapshot_id: 1,
            next_snapshot_id: 2,
            snapshot_emitted: false,
            state_seq: 0,
            cols,
            rows,
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
            history_frozen: false,
            history: VecDeque::new(),
            pending_scrollback_append: Vec::new(),
            transcript_store: TerminalTranscriptStore::new(),
            record_primary_screen_frames: false,
            screen_rows,
            next_line_id,
            retained_patches: VecDeque::new(),
            patch_retention: config.patch_retention,
            parser,
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
        merge_patch_batch(self.feed_vt_bytes_batch(bytes))
    }

    /// Feed one parser drain and return every independently sequenced patch it
    /// produced. This compatibility helper drops a wire-planning error;
    /// transport code must use [`Self::feed_vt_bytes_transport_batch`].
    pub fn feed_vt_bytes_batch(&mut self, bytes: &[u8]) -> Vec<TerminalPatchV2> {
        self.feed_vt_bytes_transport_batch(bytes)
            .map(|batch| batch.patches)
            .unwrap_or_default()
    }

    /// Feed one parser drain and preserve a terminal-reset boundary as an
    /// ordered snapshot-plus-patch transaction. Relay transport code must use
    /// this fallible API so it cannot discard post-RIS history or send an
    /// incomplete oversized transaction.
    pub fn feed_vt_bytes_transport_batch(
        &mut self,
        bytes: &[u8],
    ) -> Result<TerminalFeedBatch, TerminalWireError> {
        self.parser.process(bytes);
        self.sync_from_vt_screen()
    }
}

#[cfg(test)]
mod resource_limit_tests {
    use super::*;

    fn fill_scrollback(core: &mut TerminalCore, lines: usize) {
        let mut input = Vec::with_capacity(lines.saturating_mul(3));
        for _ in 0..lines {
            input.extend_from_slice(b"x\r\n");
        }
        core.feed_vt_bytes_transport_batch(&input)
            .expect("feed scrollback");
    }

    #[test]
    fn wide_terminal_bounds_core_and_vt_history_by_the_same_cell_budget() {
        let mut core = TerminalCore::new(TerminalCoreConfig {
            terminal_run_id: "wide-history".to_string(),
            cols: 4_096,
            rows: 64,
            patch_retention: 8,
        });

        fill_scrollback(&mut core, 256);
        let debug = core.debug_snapshot();

        assert_eq!(debug.history_len, 64);
        assert_eq!(debug.vt_scrollback_len, 64);
    }

    #[test]
    fn resizing_from_narrow_to_wide_trims_both_history_stores() {
        let mut core = TerminalCore::new(TerminalCoreConfig {
            terminal_run_id: "resize-history".to_string(),
            cols: 80,
            rows: 24,
            patch_retention: 8,
        });
        fill_scrollback(&mut core, 2_200);
        let narrow = core.debug_snapshot();
        assert_eq!(narrow.history_len, 2_048);
        assert_eq!(narrow.vt_scrollback_len, 2_048);

        core.resize(ResizeEventV2 {
            resize_seq: 1,
            cols: 4_096,
            rows: 64,
            input_stream_id: "resize-history".to_string(),
            last_input_ack: 0,
        });
        let wide = core.debug_snapshot();

        assert_eq!(wide.history_len, 64);
        assert_eq!(wide.vt_scrollback_len, 64);
    }

    #[test]
    fn transcript_store_enforces_total_encoded_byte_and_cell_budgets() {
        let alternating_row = TerminalRow {
            line_id: 1,
            wrapped: false,
            cells: (0..4_096)
                .map(|index| CellRun {
                    attr_id: index % 2,
                    cells: vec![TerminalCell {
                        text: "X".to_string(),
                        width: 1,
                    }],
                })
                .collect(),
        };
        let mut store = TerminalTranscriptStore::new();
        for state_seq in 1..=320 {
            store.record_rows(
                TerminalTranscriptEntryKind::NormalScrollback,
                vec![alternating_row.clone()],
                state_seq,
                "transcript-budget",
                4_096,
            );
        }

        assert!(store.entries.len() < 320);
        assert!(store.total_encoded_bytes <= TERMINAL_TRANSCRIPT_MAX_TOTAL_ENCODED_BYTES);
        assert!(store.total_cells <= TERMINAL_TRANSCRIPT_MAX_TOTAL_CELLS);
    }

    #[test]
    fn transcript_entry_budget_reserves_a_complete_maximum_attribute_table() {
        let attrs = vec![
            CellAttr {
                fg: TerminalColor::Rgb {
                    r: u8::MAX,
                    g: u8::MAX,
                    b: u8::MAX,
                },
                bg: TerminalColor::Rgb {
                    r: u8::MAX,
                    g: u8::MAX,
                    b: u8::MAX,
                },
                bold: true,
                italic: true,
                underline: true,
                inverse: true,
                strikethrough: true,
                dim: true,
            };
            4_096
        ];
        let entry = TerminalTranscriptEntryV2 {
            entry_id: u64::MAX,
            terminal_run_id: "run1".to_string(),
            state_seq: u64::MAX,
            kind: TerminalTranscriptEntryKind::NormalScrollback,
            cols: u16::MAX,
            rows: Vec::new(),
            captured_at_unix_ms: u64::MAX,
            frame_fragment: None,
        };
        let envelope_bytes = transcript_chunk_v2_parts_encoded_len(
            "run1",
            Some(u64::MAX),
            std::slice::from_ref(&entry),
            &attrs,
            true,
        )
        .saturating_sub(terminal_transcript_entry_v2_encoded_len(&entry));

        assert!(
            TERMINAL_TRANSCRIPT_MAX_ENTRY_ENCODED_BYTES.saturating_add(envelope_bytes)
                <= TERMINAL_RELAY_SAFE_PLAIN_MSG_BYTES,
            "entry budget plus worst-case attrs encoded to {} bytes",
            TERMINAL_TRANSCRIPT_MAX_ENTRY_ENCODED_BYTES.saturating_add(envelope_bytes)
        );
    }

    #[test]
    fn normal_scrollback_batches_split_without_losing_row_order() {
        let rows = (1..=8)
            .map(|line_id| TerminalRow {
                line_id,
                wrapped: false,
                cells: (0..4_096)
                    .map(|index| CellRun {
                        attr_id: index % 2,
                        cells: vec![TerminalCell {
                            text: "X".to_string(),
                            width: 1,
                        }],
                    })
                    .collect(),
            })
            .collect::<Vec<_>>();
        let expected_line_ids = rows.iter().map(|row| row.line_id).collect::<Vec<_>>();
        let mut store = TerminalTranscriptStore::new();

        store.record_rows(
            TerminalTranscriptEntryKind::NormalScrollback,
            rows,
            1,
            "split-scrollback",
            4_096,
        );

        assert!(store.entries.len() > 1);
        assert!(store.entries.iter().all(|entry| {
            terminal_transcript_entry_v2_encoded_len(entry)
                <= TERMINAL_TRANSCRIPT_MAX_ENTRY_ENCODED_BYTES
        }));
        assert_eq!(
            store
                .entries
                .iter()
                .flat_map(|entry| entry.rows.iter().map(|row| row.line_id))
                .collect::<Vec<_>>(),
            expected_line_ids
        );
    }

    #[test]
    fn screen_frames_split_without_losing_rows_and_remain_deduplicated() {
        let rows = (1..=3)
            .map(|line_id| TerminalRow {
                line_id,
                wrapped: false,
                cells: (0..4_096)
                    .map(|attr_id| CellRun {
                        attr_id,
                        cells: vec![TerminalCell {
                            text: "X".repeat(MAX_TERMINAL_CELL_TEXT_BYTES),
                            width: 1,
                        }],
                    })
                    .collect(),
            })
            .collect::<Vec<_>>();
        let expected_line_ids = rows.iter().map(|row| row.line_id).collect::<Vec<_>>();
        let mut store = TerminalTranscriptStore::new();

        store.record_screen_frame(
            TerminalTranscriptEntryKind::AltScreenFrame,
            &rows,
            1,
            "split-screen-frame",
            4_096,
        );

        assert!(store.entries.len() > 1, "large frame must be fragmented");
        assert_eq!(
            store
                .entries
                .iter()
                .flat_map(|entry| entry.rows.iter().map(|row| row.line_id))
                .collect::<Vec<_>>(),
            expected_line_ids,
            "screen-frame fragmentation must preserve every row in order"
        );
        assert!(
            store
                .entries
                .iter()
                .all(TerminalTranscriptStore::entry_fits_storage)
        );
        let frame_id = store.entries[0]
            .frame_fragment
            .as_ref()
            .expect("fragmented frame metadata")
            .frame_id;
        let fragment_count = u32::try_from(store.entries.len()).expect("fragment count");
        for (fragment_index, entry) in store.entries.iter().enumerate() {
            let fragment = entry
                .frame_fragment
                .as_ref()
                .expect("every fragment carries grouping metadata");
            assert_eq!(fragment.frame_id, frame_id);
            assert_eq!(fragment.fragment_index, fragment_index as u32);
            assert_eq!(fragment.fragment_count, fragment_count);
        }

        let entry_count = store.entries.len();
        store.record_screen_frame(
            TerminalTranscriptEntryKind::AltScreenFrame,
            &rows,
            2,
            "split-screen-frame",
            4_096,
        );
        assert_eq!(
            store.entries.len(),
            entry_count,
            "an identical fragmented frame must still be deduplicated"
        );
    }

    #[test]
    fn screen_frame_dedupe_keeps_same_rows_at_a_new_column_count() {
        let rows = vec![TerminalRow {
            line_id: 1,
            wrapped: false,
            cells: vec![CellRun {
                attr_id: 0,
                cells: vec![TerminalCell {
                    text: "same visible row".to_string(),
                    width: 1,
                }],
            }],
        }];
        let mut store = TerminalTranscriptStore::new();

        store.record_screen_frame(
            TerminalTranscriptEntryKind::ScreenFrame,
            &rows,
            1,
            "screen-resize",
            80,
        );
        store.record_screen_frame(
            TerminalTranscriptEntryKind::ScreenFrame,
            &rows,
            2,
            "screen-resize",
            120,
        );

        assert_eq!(store.entries.len(), 2);
        assert_eq!(store.entries[0].cols, 80);
        assert_eq!(store.entries[1].cols, 120);
    }

    #[test]
    fn transcript_eviction_never_leaves_half_of_a_fragmented_screen_frame() {
        let rows = (1..=3)
            .map(|line_id| TerminalRow {
                line_id,
                wrapped: false,
                cells: (0..4_096)
                    .map(|attr_id| CellRun {
                        attr_id,
                        cells: vec![TerminalCell {
                            text: "X".repeat(MAX_TERMINAL_CELL_TEXT_BYTES),
                            width: 1,
                        }],
                    })
                    .collect(),
            })
            .collect::<Vec<_>>();
        let mut store = TerminalTranscriptStore::new();
        store.record_screen_frame(
            TerminalTranscriptEntryKind::ScreenFrame,
            &rows,
            1,
            "evict-screen-frame",
            4_096,
        );
        let frame_id = store.entries[0]
            .frame_fragment
            .as_ref()
            .expect("large frame is fragmented")
            .frame_id;

        for state_seq in 2..=TERMINAL_TRANSCRIPT_MAX_ENTRIES as u64 + 1 {
            store.record_rows(
                TerminalTranscriptEntryKind::NormalScrollback,
                vec![TerminalRow {
                    line_id: state_seq,
                    wrapped: false,
                    cells: vec![CellRun {
                        attr_id: 0,
                        cells: vec![TerminalCell {
                            text: "x".to_string(),
                            width: 1,
                        }],
                    }],
                }],
                state_seq,
                "evict-screen-frame",
                80,
            );
        }

        assert!(store.entries.iter().all(|entry| {
            entry
                .frame_fragment
                .as_ref()
                .is_none_or(|fragment| fragment.frame_id != frame_id)
        }));
        assert!(
            store.entries.iter().all(|entry| {
                entry
                    .frame_fragment
                    .as_ref()
                    .map(|fragment| fragment.fragment_index == 0)
                    .unwrap_or(true)
            }),
            "no orphaned non-initial frame fragment may remain after eviction"
        );
    }

    #[test]
    fn maximum_legal_screen_frame_fits_the_total_transcript_budget() {
        let max_cell_text = format!("\u{e9}{}", "\u{301}".repeat(31));
        assert_eq!(max_cell_text.len(), MAX_TERMINAL_CELL_TEXT_BYTES);
        let rows = (0..MAX_TERMINAL_ROWS)
            .map(|row| TerminalRow {
                line_id: u64::MAX - u64::from(row),
                wrapped: true,
                cells: (0..64_u32)
                    .map(|attr_offset| CellRun {
                        attr_id: 4_032 + attr_offset,
                        cells: vec![TerminalCell {
                            text: max_cell_text.clone(),
                            width: 1,
                        }],
                    })
                    .collect(),
            })
            .collect::<Vec<_>>();
        let store = TerminalTranscriptStore::new();
        let terminal_run_id = "r".repeat(64);

        let plan = store
            .plan_screen_frame(
                TerminalTranscriptEntryKind::AltScreenFrame,
                &rows,
                u64::MAX,
                &terminal_run_id,
                64,
                u64::MAX,
            )
            .expect("the maximum legal frame must be storable");

        assert_eq!(plan.total_cells, MAX_TERMINAL_CELLS);
        assert_eq!(
            plan.total_encoded_bytes, 25_849_318,
            "serializer changes must revalidate the 32 MiB transcript budget"
        );
        assert!(plan.fragments.len() > 1);
        assert!(plan.fragments.iter().all(|fragment| {
            fragment.encoded_bytes <= TERMINAL_TRANSCRIPT_MAX_ENTRY_ENCODED_BYTES
        }));
        let first = &plan.fragments[0];
        assert!(
            first.rows.len() > 15,
            "exercise the MessagePack array16 header"
        );
        let fragment_count = u32::try_from(plan.fragments.len()).expect("fragment count");
        let first_entry = TerminalTranscriptStore::entry_with_rows_and_fragment(
            plan.frame_id,
            TerminalTranscriptEntryKind::AltScreenFrame,
            rows[first.rows.clone()].to_vec(),
            u64::MAX,
            &terminal_run_id,
            64,
            u64::MAX,
            Some(TerminalTranscriptFrameFragmentV2 {
                frame_id: plan.frame_id,
                fragment_index: 0,
                fragment_count,
            }),
        );
        assert_eq!(
            terminal_transcript_entry_v2_encoded_len(&first_entry),
            first.encoded_bytes,
            "row-length planning must match the real fragment serializer"
        );
        assert!(
            plan.total_encoded_bytes <= TERMINAL_TRANSCRIPT_MAX_TOTAL_ENCODED_BYTES,
            "maximum legal fragmented frame encoded to {} bytes, budget is {}",
            plan.total_encoded_bytes,
            TERMINAL_TRANSCRIPT_MAX_TOTAL_ENCODED_BYTES
        );
    }

    #[test]
    fn single_scrollback_row_survives_a_maximum_attribute_table() {
        let attrs = (0..4_096)
            .map(|index| CellAttr {
                fg: TerminalColor::Rgb {
                    r: (index & 0xff) as u8,
                    g: ((index >> 4) & 0xff) as u8,
                    b: ((index >> 8) & 0xff) as u8,
                },
                bg: TerminalColor::Rgb {
                    r: ((index >> 8) & 0xff) as u8,
                    g: ((index >> 4) & 0xff) as u8,
                    b: (index & 0xff) as u8,
                },
                bold: index & 1 != 0,
                italic: index & 2 != 0,
                underline: index & 4 != 0,
                inverse: index & 8 != 0,
                strikethrough: index & 16 != 0,
                dim: index & 32 != 0,
            })
            .collect::<Vec<_>>();
        let row = TerminalRow {
            line_id: 7,
            wrapped: false,
            cells: (0..4_096)
                .map(|attr_id| CellRun {
                    attr_id,
                    cells: vec![TerminalCell {
                        text: "X".repeat(MAX_TERMINAL_CELL_TEXT_BYTES),
                        width: 1,
                    }],
                })
                .collect(),
        };
        let mut store = TerminalTranscriptStore::new();
        store.record_rows(
            TerminalTranscriptEntryKind::NormalScrollback,
            vec![row],
            1,
            "max-attrs",
            4_096,
        );

        let chunk = store.chunk(
            &RequestTranscriptV2 {
                terminal_run_id: "max-attrs".to_string(),
                before_entry_id: None,
                max_entries: 1,
            },
            "max-attrs",
            attrs,
        );

        assert_eq!(chunk.entries.len(), 1, "the only scrollback row was lost");
        assert_eq!(chunk.entries[0].rows[0].line_id, 7);
        assert!(transcript_chunk_v2_encoded_len(&chunk) <= TERMINAL_RELAY_SAFE_PLAIN_MSG_BYTES);
    }

    #[test]
    fn transcript_chunk_does_not_partially_drop_a_scrollback_entry() {
        let attrs = (0..4_096)
            .map(|index| CellAttr {
                fg: TerminalColor::Rgb {
                    r: (index & 0xff) as u8,
                    g: ((index >> 4) & 0xff) as u8,
                    b: ((index >> 8) & 0xff) as u8,
                },
                bg: TerminalColor::Default,
                ..CellAttr::default()
            })
            .collect::<Vec<_>>();
        let dense_row = |line_id| TerminalRow {
            line_id,
            wrapped: false,
            cells: (0..4_096)
                .map(|attr_id| CellRun {
                    attr_id,
                    cells: vec![TerminalCell {
                        text: "X".repeat(MAX_TERMINAL_CELL_TEXT_BYTES),
                        width: 1,
                    }],
                })
                .collect(),
        };
        let mut store = TerminalTranscriptStore::new();
        store.record_rows(
            TerminalTranscriptEntryKind::NormalScrollback,
            vec![dense_row(7), dense_row(8)],
            1,
            "max-attrs-page",
            4_096,
        );
        assert!(
            store.entries.len() > 1,
            "rows must be split before storage when attrs consume the remaining budget"
        );

        let mut before_entry_id = None;
        let mut pages = Vec::new();
        loop {
            let chunk = store.chunk(
                &RequestTranscriptV2 {
                    terminal_run_id: "max-attrs-page".to_string(),
                    before_entry_id,
                    max_entries: 2,
                },
                "max-attrs-page",
                attrs.clone(),
            );
            assert!(!chunk.entries.is_empty(), "paging returned an empty chunk");
            assert!(transcript_chunk_v2_encoded_len(&chunk) <= TERMINAL_RELAY_SAFE_PLAIN_MSG_BYTES);
            for entry in &chunk.entries {
                assert_eq!(
                    entry.rows,
                    store
                        .entries
                        .iter()
                        .find(|stored| stored.entry_id == entry.entry_id)
                        .expect("returned entry remains in store")
                        .rows,
                    "chunk must not partially trim a stored entry"
                );
            }
            pages.push(
                chunk
                    .entries
                    .iter()
                    .flat_map(|entry| entry.rows.iter().map(|row| row.line_id))
                    .collect::<Vec<_>>(),
            );
            if !chunk.has_more {
                break;
            }
            before_entry_id = Some(chunk.entries[0].entry_id);
            assert!(
                pages.len() <= store.entries.len(),
                "transcript paging did not advance"
            );
        }
        pages.reverse();
        assert_eq!(pages.into_iter().flatten().collect::<Vec<_>>(), [7, 8]);
    }

    #[test]
    fn transcript_chunk_plan_selects_the_newest_complete_suffix() {
        let attrs = vec![CellAttr::default(); 4_096];
        let dense_row = |line_id| TerminalRow {
            line_id,
            wrapped: false,
            cells: (0..4_096)
                .map(|attr_id| CellRun {
                    attr_id,
                    cells: vec![TerminalCell {
                        text: "X".repeat(MAX_TERMINAL_CELL_TEXT_BYTES),
                        width: 1,
                    }],
                })
                .collect(),
        };
        let mut store = TerminalTranscriptStore::new();
        for line_id in 1..=3 {
            store.record_rows(
                TerminalTranscriptEntryKind::NormalScrollback,
                vec![dense_row(line_id)],
                line_id,
                "chunk-plan",
                4_096,
            );
        }
        let request = RequestTranscriptV2 {
            terminal_run_id: "chunk-plan".to_string(),
            before_entry_id: None,
            max_entries: 3,
        };

        let plan = store.plan_chunk(&request, "chunk-plan", &attrs);

        assert_eq!(plan.end, store.entries.len());
        assert_eq!(plan.end - plan.start, 1);
        assert!(plan.has_more);
        assert_eq!(store.entries[plan.start].rows[0].line_id, 3);
        assert_eq!(
            plan.encoded_bytes,
            transcript_chunk_v2_parts_encoded_len(
                "chunk-plan",
                None,
                std::slice::from_ref(&store.entries[plan.start]),
                &attrs,
                true,
            )
        );
        assert!(plan.encoded_bytes <= TERMINAL_RELAY_SAFE_PLAIN_MSG_BYTES);
    }

    #[test]
    fn transcript_chunk_plan_exactly_sizes_an_array16_page() {
        let mut store = TerminalTranscriptStore::new();
        for line_id in 1..=16 {
            store.record_rows(
                TerminalTranscriptEntryKind::NormalScrollback,
                vec![TerminalRow {
                    line_id,
                    wrapped: false,
                    cells: vec![CellRun {
                        attr_id: 0,
                        cells: vec![TerminalCell {
                            text: "x".to_string(),
                            width: 1,
                        }],
                    }],
                }],
                line_id,
                "chunk-array16",
                80,
            );
        }
        let request = RequestTranscriptV2 {
            terminal_run_id: "chunk-array16".to_string(),
            before_entry_id: None,
            max_entries: 16,
        };

        let plan = store.plan_chunk(&request, "chunk-array16", &[]);
        let entries = store.entries.iter().cloned().collect::<Vec<_>>();

        assert_eq!((plan.start, plan.end), (0, 16));
        assert!(!plan.has_more);
        assert_eq!(
            plan.encoded_bytes,
            transcript_chunk_v2_parts_encoded_len("chunk-array16", None, &entries, &[], false,)
        );
    }

    #[test]
    fn older_transcript_pages_keep_the_complete_attribute_table() {
        let attrs = vec![CellAttr::default(); 4_096];
        let row = |line_id, attr_id| TerminalRow {
            line_id,
            wrapped: false,
            cells: vec![CellRun {
                attr_id,
                cells: vec![TerminalCell {
                    text: "X".to_string(),
                    width: 1,
                }],
            }],
        };
        let mut store = TerminalTranscriptStore::new();
        store.record_rows(
            TerminalTranscriptEntryKind::NormalScrollback,
            vec![row(1, 0)],
            1,
            "attr-pages",
            80,
        );
        store.record_rows(
            TerminalTranscriptEntryKind::NormalScrollback,
            vec![row(2, 4_095)],
            2,
            "attr-pages",
            80,
        );

        let newest = store.chunk(
            &RequestTranscriptV2 {
                terminal_run_id: "attr-pages".to_string(),
                before_entry_id: None,
                max_entries: 1,
            },
            "attr-pages",
            attrs.clone(),
        );
        assert_eq!(newest.attrs.len(), attrs.len());
        let older = store.chunk(
            &RequestTranscriptV2 {
                terminal_run_id: "attr-pages".to_string(),
                before_entry_id: Some(newest.entries[0].entry_id),
                max_entries: 1,
            },
            "attr-pages",
            attrs.clone(),
        );

        assert_eq!(older.entries[0].rows[0].line_id, 1);
        assert_eq!(
            older.attrs.len(),
            attrs.len(),
            "legacy apps replace, rather than merge, transcript attrs"
        );
    }

    #[test]
    fn zero_max_entries_still_returns_progressing_complete_pages() {
        let attrs = vec![CellAttr::default(); 4_096];
        let row = |line_id| TerminalRow {
            line_id,
            wrapped: false,
            cells: vec![CellRun {
                attr_id: 0,
                cells: vec![TerminalCell {
                    text: "X".to_string(),
                    width: 1,
                }],
            }],
        };
        let mut store = TerminalTranscriptStore::new();
        store.record_rows(
            TerminalTranscriptEntryKind::NormalScrollback,
            vec![row(1)],
            1,
            "zero-page",
            80,
        );
        store.record_rows(
            TerminalTranscriptEntryKind::NormalScrollback,
            vec![row(2)],
            2,
            "zero-page",
            80,
        );

        let newest = store.chunk(
            &RequestTranscriptV2 {
                terminal_run_id: "zero-page".to_string(),
                before_entry_id: None,
                max_entries: 0,
            },
            "zero-page",
            attrs.clone(),
        );
        assert_eq!(newest.entries.len(), 1);
        assert_eq!(newest.entries[0].rows[0].line_id, 2);
        assert_eq!(newest.attrs.len(), attrs.len());
        assert!(newest.has_more);
        assert!(transcript_chunk_v2_encoded_len(&newest) <= TERMINAL_RELAY_SAFE_PLAIN_MSG_BYTES);

        let older = store.chunk(
            &RequestTranscriptV2 {
                terminal_run_id: "zero-page".to_string(),
                before_entry_id: Some(newest.entries[0].entry_id),
                max_entries: 0,
            },
            "zero-page",
            attrs.clone(),
        );
        assert_eq!(older.entries.len(), 1);
        assert_eq!(older.entries[0].rows[0].line_id, 1);
        assert_eq!(older.attrs.len(), attrs.len());
        assert!(!older.has_more);
        assert!(transcript_chunk_v2_encoded_len(&older) <= TERMINAL_RELAY_SAFE_PLAIN_MSG_BYTES);
    }

    #[test]
    fn resizing_wide_history_to_narrow_normalizes_retained_rows() {
        let mut core = TerminalCore::new(TerminalCoreConfig {
            terminal_run_id: "narrow-history".to_string(),
            cols: 4_096,
            rows: 64,
            patch_retention: 8,
        });
        let mut input = Vec::new();
        for _ in 0..70 {
            input.extend(std::iter::repeat_n(b'x', 4_096));
            input.extend_from_slice(b"\r\n");
        }
        core.feed_vt_bytes_transport_batch(&input)
            .expect("feed dense wide scrollback");
        let retained_line_ids = core
            .history
            .iter()
            .map(|row| row.line_id)
            .collect::<Vec<_>>();
        assert!(!retained_line_ids.is_empty());
        assert!(
            core.history
                .iter()
                .any(|row| { row.cells.iter().map(|run| run.cells.len()).sum::<usize>() > 80 })
        );

        core.resize(ResizeEventV2 {
            resize_seq: 1,
            cols: 80,
            rows: 64,
            input_stream_id: "narrow-history".to_string(),
            last_input_ack: 0,
        });

        assert_eq!(
            core.history
                .iter()
                .map(|row| row.line_id)
                .collect::<Vec<_>>(),
            retained_line_ids
        );
        assert!(
            core.history
                .iter()
                .all(|row| { row.cells.iter().map(|run| run.cells.len()).sum::<usize>() <= 80 })
        );
    }
}
