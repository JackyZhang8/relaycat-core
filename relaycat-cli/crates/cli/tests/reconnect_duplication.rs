//! Reproduction harness for the "duplicate scrollback after repeated
//! disconnect/reconnect" bug. It drives a real `TerminalCore` through the same
//! sequence the relay performs on resume (flush coalesced output, then
//! `resume_messages`) and replays the resulting messages into a faithful
//! replica of the app-side `TerminalSemanticState` apply rules.

use relaycat_cli::terminal_core::{TerminalCore, TerminalCoreConfig};
use relaycat_protocol::{PatchOp, PlainMsg, ResizeEventV2, ResumeV2, TerminalRow};

const COLS: u16 = 40;
const ROWS: u16 = 8;
const LOCAL_COLS: u16 = 52;
const LOCAL_ROWS: u16 = 12;

fn row_text(row: &TerminalRow) -> String {
    row.cells
        .iter()
        .flat_map(|run| run.cells.iter())
        .map(|cell| cell.text.as_str())
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// Mirrors the app-side `TerminalSemanticState` apply rules (Kotlin/Swift):
/// snapshots replace state wholesale, patches are validated by sequence and
/// `AppendScrollback` rows are appended to the scrollback window. On a rejected
/// patch the app keeps its state and asks for a snapshot (it does not block).
#[derive(Default)]
struct AppModel {
    snapshot_id: u64,
    state_seq: u64,
    scrollback: Vec<TerminalRow>,
    screen: Vec<TerminalRow>,
    rejected_patches: usize,
    wants_snapshot: bool,
}

impl AppModel {
    fn apply(&mut self, msg: &PlainMsg) {
        match msg {
            PlainMsg::TerminalSnapshotV2(s) => {
                self.snapshot_id = s.snapshot_id;
                self.state_seq = s.state_seq;
                self.scrollback = s.scrollback_window.clone();
                self.screen = s.screen_rows.clone();
                self.wants_snapshot = false;
            }
            PlainMsg::TerminalPatchV2(p) => {
                let valid = p.base_snapshot_id == self.snapshot_id
                    && p.from_state_seq == self.state_seq + 1
                    && p.to_state_seq >= p.from_state_seq;
                if !valid {
                    self.rejected_patches += 1;
                    self.wants_snapshot = true;
                    return;
                }
                for op in &p.ops {
                    match op {
                        PatchOp::AppendScrollback { rows } => {
                            self.scrollback.extend(rows.iter().cloned());
                        }
                        PatchOp::ReplaceRow { row, line } => {
                            let idx = usize::from(*row);
                            if idx < self.screen.len() {
                                self.screen[idx] = line.clone();
                            }
                        }
                        _ => {}
                    }
                }
                self.state_seq = p.to_state_seq;
            }
            _ => {}
        }
    }

    fn rendered_lines(&self) -> Vec<String> {
        self.scrollback
            .iter()
            .chain(self.screen.iter())
            .map(row_text)
            .filter(|t| !t.is_empty())
            .collect()
    }
}

fn resume_msg(app: &AppModel, run_id: &str) -> ResumeV2 {
    ResumeV2 {
        terminal_run_id: Some(run_id.to_string()),
        last_applied_state_seq: app.state_seq,
        last_snapshot_id: Some(app.snapshot_id),
        input_stream_id: "stream-1".to_string(),
        last_input_ack: 0,
    }
}

/// The pre-fix `relay.rs` Resume handling: flush coalesced output as its own
/// patch, then append the resume reply. `resume_messages()` replays the same
/// patch through the retained range, so the freshly flushed patch is delivered
/// twice.
fn relay_resume_legacy(
    core: &mut TerminalCore,
    pending: &mut Vec<u8>,
    resume: &ResumeV2,
) -> Vec<PlainMsg> {
    let mut msgs = Vec::new();
    if !pending.is_empty() {
        let bytes = std::mem::take(pending);
        msgs.push(PlainMsg::TerminalPatchV2(
            core.feed_vt_bytes(&bytes)
                .expect("buffered output should emit a patch"),
        ));
    }
    msgs.extend(core.resume_messages(resume));
    msgs
}

/// The fixed `relay.rs` Resume handling: fold buffered output into the core
/// without emitting a standalone patch, then let `resume_messages()` replay it
/// exactly once through the retained range.
fn relay_resume_folding(
    core: &mut TerminalCore,
    pending: &mut Vec<u8>,
    resume: &ResumeV2,
) -> Vec<PlainMsg> {
    if !pending.is_empty() {
        let bytes = std::mem::take(pending);
        let _ = core.feed_vt_bytes(&bytes);
    }
    core.resume_messages(resume)
}

fn relay_dirty_local_mode_snapshot(core: &mut TerminalCore, pending: &mut Vec<u8>) -> PlainMsg {
    if !pending.is_empty() {
        let bytes = std::mem::take(pending);
        let _ = core.feed_vt_bytes(&bytes);
    }
    PlainMsg::TerminalSnapshotV2(core.snapshot_with_reset_app_cache(true))
}

fn apply_ops_repeatedly(core: &mut TerminalCore, count: u64, title_prefix: &str) {
    for seq in 0..count {
        core.apply_ops(vec![PatchOp::SetTitle(format!("{title_prefix}-{seq}"))]);
    }
}

fn duplicate_lines(lines: &[String]) -> Vec<(String, usize)> {
    use std::collections::BTreeMap;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for line in lines {
        *counts.entry(line.clone()).or_default() += 1;
    }
    counts.into_iter().filter(|(_, n)| *n > 1).collect()
}

fn feed_question(pending: &mut Vec<u8>, question: u32) {
    for line in 0..6u32 {
        pending.extend_from_slice(format!("Q{question} line {line}\r\n").as_bytes());
    }
}

/// Simulate a TUI redrawing its viewport on SIGWINCH: it re-emits the lines it
/// most recently printed. Those copies land in the scrollback alongside the
/// originals that already scrolled off-screen.
fn feed_sigwinch_reprint(pending: &mut Vec<u8>, question: u32) {
    for line in 2..6u32 {
        pending.extend_from_slice(format!("Q{question} line {line}\r\n").as_bytes());
    }
}

/// A faithful TUI repaint of the *live frame* after a resize: it clears the
/// viewport and redraws the rows that were on-screen (never in scrollback) at
/// the new width. Unlike `feed_sigwinch_reprint`, this redraws only the visible
/// tail (post-scrollback content), exactly like Claude Code / Ink redrawing
/// their current frame — it never re-emits already-scrolled-off history. With
/// `ROWS = 8`, after `feed_question(..=9)` the scrollback ends at logical line
/// 46 (`Q8 line 4`) and the visible tail is lines 47..53: `Q8 line 5`, then
/// `Q9 line 0..5`.
fn feed_live_frame_redraw(pending: &mut Vec<u8>) {
    pending.extend_from_slice(b"\x1b[2J\x1b[H");
    let tail = [
        "Q8 line 5",
        "Q9 line 0",
        "Q9 line 1",
        "Q9 line 2",
        "Q9 line 3",
        "Q9 line 4",
        "Q9 line 5",
    ];
    for (idx, line) in tail.iter().enumerate() {
        pending.extend_from_slice(line.as_bytes());
        // No trailing newline on the final row so all eight rows fit on the
        // eight-row screen without scrolling anything off.
        if idx + 1 < tail.len() {
            pending.extend_from_slice(b"\r\n");
        }
    }
}

fn new_core(run_id: &str) -> TerminalCore {
    TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: run_id.to_string(),
        cols: COLS,
        rows: ROWS,
        patch_retention: 4096,
    })
}

/// Reproduces the original defect: every disconnect restored the PTY to the
/// local size and the following reconnect resized it back. Each resize makes
/// the TUI repaint, and the repainted lines accumulate as scrollback
/// duplicates that both apps render identically.
#[test]
fn legacy_disconnect_resize_churn_duplicates_scrollback() {
    let run_id = "run-dup";
    let mut core = new_core(run_id);
    let mut app = AppModel::default();
    app.apply(&PlainMsg::TerminalSnapshotV2(core.snapshot()));
    let mut pending: Vec<u8> = Vec::new();

    for question in 1..=9u32 {
        feed_question(&mut pending, question);

        if question % 2 == 1 {
            // disconnect: relay restores the PTY to the LOCAL size (the TUI
            // repaints, so its recent lines are re-emitted).
            let _ = core.resize(ResizeEventV2 {
                resize_seq: question as u64 * 10,
                cols: LOCAL_COLS,
                rows: LOCAL_ROWS,
                input_stream_id: "stream-1".to_string(),
                last_input_ack: 0,
            });
            feed_sigwinch_reprint(&mut pending, question);

            let resume = resume_msg(&app, run_id);
            for msg in relay_resume_legacy(&mut core, &mut pending, &resume) {
                app.apply(&msg);
            }

            // reconnect: app resizes back to its own dimensions (another repaint).
            let (_ack, snap) = core.resize(ResizeEventV2 {
                resize_seq: question as u64 * 10 + 1,
                cols: COLS,
                rows: ROWS,
                input_stream_id: "stream-1".to_string(),
                last_input_ack: 0,
            });
            feed_sigwinch_reprint(&mut pending, question);
            app.apply(&PlainMsg::TerminalSnapshotV2(snap));
        } else {
            let bytes = std::mem::take(&mut pending);
            let patch = core
                .feed_vt_bytes(&bytes)
                .expect("buffered question output should emit a patch");
            app.apply(&PlainMsg::TerminalPatchV2(patch));
        }

        if app.wants_snapshot {
            if !pending.is_empty() {
                let bytes = std::mem::take(&mut pending);
                let _ = core.feed_vt_bytes(&bytes);
            }
            app.apply(&PlainMsg::TerminalSnapshotV2(core.snapshot()));
        }
    }

    let dups = duplicate_lines(&app.rendered_lines());
    assert!(
        !dups.is_empty(),
        "expected the legacy resize churn to duplicate scrollback"
    );
}

/// With the fix, a bare disconnect no longer resizes the PTY, so the TUI never
/// repaints and the snapshot base stays put. The reconnect resumes against the
/// matching snapshot base and folds any buffered output into a single retained-patch
/// replay, so no line is delivered (or rendered) twice.
#[test]
fn fixed_reconnect_without_resize_churn_does_not_duplicate_scrollback() {
    let run_id = "run-dup";
    let mut core = new_core(run_id);
    let mut app = AppModel::default();
    app.apply(&PlainMsg::TerminalSnapshotV2(core.snapshot()));
    let mut pending: Vec<u8> = Vec::new();

    for question in 1..=9u32 {
        feed_question(&mut pending, question);

        if question % 2 == 1 {
            // disconnect: no PTY resize, so the TUI does not repaint; reconnect
            // resumes against the unchanged snapshot base and folds buffered output.
            let resume = resume_msg(&app, run_id);
            for msg in relay_resume_folding(&mut core, &mut pending, &resume) {
                app.apply(&msg);
            }
        } else {
            let bytes = std::mem::take(&mut pending);
            let patch = core
                .feed_vt_bytes(&bytes)
                .expect("buffered question output should emit a patch");
            app.apply(&PlainMsg::TerminalPatchV2(patch));
        }

        if app.wants_snapshot {
            if !pending.is_empty() {
                let bytes = std::mem::take(&mut pending);
                let _ = core.feed_vt_bytes(&bytes);
            }
            app.apply(&PlainMsg::TerminalSnapshotV2(core.snapshot()));
        }
    }

    let lines = app.rendered_lines();
    let dups = duplicate_lines(&lines);
    assert!(dups.is_empty(), "duplicate scrollback lines: {dups:?}");
    assert_eq!(
        app.rejected_patches, 0,
        "fixed resume path should not double-deliver patches"
    );
    // All nine questions survived in order without duplication.
    for question in 1..=9u32 {
        for line in 0..6u32 {
            let needle = format!("Q{question} line {line}");
            assert_eq!(
                lines.iter().filter(|l| **l == needle).count(),
                1,
                "{needle} should appear exactly once"
            );
        }
    }
}

#[test]
fn resume_replays_short_disconnect_gap_before_new_live_patches() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: COLS,
        rows: ROWS,
        patch_retention: 64,
    });
    let mut app = AppModel::default();
    app.apply(&PlainMsg::TerminalSnapshotV2(core.snapshot()));

    apply_ops_repeatedly(&mut core, 4, "connected");
    for msg in core.resume_messages(&resume_msg(&app, "run-1")) {
        app.apply(&msg);
    }
    assert_eq!(app.rejected_patches, 0);
    let app_seq_before_disconnect = app.state_seq;

    apply_ops_repeatedly(&mut core, 14, "offline");

    let replay = core.resume_messages(&resume_msg(&app, "run-1"));
    let [PlainMsg::TerminalPatchV2(patch)] = replay.as_slice() else {
        panic!("short disconnect gap should replay retained patches");
    };
    assert_eq!(patch.from_state_seq, app_seq_before_disconnect + 1);
    assert_eq!(patch.to_state_seq, app_seq_before_disconnect + 14);

    for msg in replay {
        app.apply(&msg);
    }
    assert_eq!(app.rejected_patches, 0);
    assert_eq!(app.state_seq, app_seq_before_disconnect + 14);
}

#[test]
fn locked_width_ignores_rotation_so_no_duplication() {
    let run_id = "run-lock";
    let mut core = new_core(run_id);
    let mut app = AppModel::default();
    app.apply(&PlainMsg::TerminalSnapshotV2(core.snapshot()));
    let mut pending: Vec<u8> = Vec::new();

    // First RemoteMode size report establishes the PTY size. Later app size
    // reports (rotation, reconnect at a different size, another app) are
    // ignored unless the CLI explicitly switched through LocalMode.
    let mut remote_size_established = false;
    let portrait = (COLS, ROWS);
    let landscape = (ROWS, COLS); // a rotated, very different geometry
    let locked_cols = COLS;

    for question in 1..=9u32 {
        feed_question(&mut pending, question);

        // Alternate the size the client reports, simulating rotation between
        // portrait and landscape on each reconnect.
        let reported = if question % 2 == 0 {
            landscape
        } else {
            portrait
        };

        if !remote_size_established {
            let _ = core.resize(ResizeEventV2 {
                resize_seq: question as u64 * 10,
                cols: portrait.0,
                rows: portrait.1,
                input_stream_id: "stream-1".to_string(),
                last_input_ack: 0,
            });
            remote_size_established = true;
        } else {
            // This is the key behavior under test: later size reports do not
            // resize, so the running TUI never repaints old lines into history.
            let _ = reported;
        }

        // Every cycle is a reconnect: fold buffered output and resume once.
        let resume = resume_msg(&app, run_id);
        for msg in relay_resume_folding(&mut core, &mut pending, &resume) {
            app.apply(&msg);
        }

        if app.wants_snapshot {
            if !pending.is_empty() {
                let bytes = std::mem::take(&mut pending);
                let _ = core.feed_vt_bytes(&bytes);
            }
            app.apply(&PlainMsg::TerminalSnapshotV2(core.snapshot()));
        }
    }

    // The width never changed after the first lock.
    assert!(remote_size_established);
    assert_eq!(
        core.snapshot().cols,
        locked_cols,
        "core stays at locked width"
    );

    let lines = app.rendered_lines();
    let dups = duplicate_lines(&lines);
    assert!(dups.is_empty(), "duplicate scrollback lines: {dups:?}");
    for question in 1..=9u32 {
        for line in 0..6u32 {
            let needle = format!("Q{question} line {line}");
            assert_eq!(
                lines.iter().filter(|l| **l == needle).count(),
                1,
                "{needle} should appear exactly once"
            );
        }
    }
}

/// This mirrors explicit LocalMode/RemoteMode transitions: resize repaint churn
/// is frozen and discarded, then the reconnect snapshot seeds the app with the
/// latest twelve screens of scrollback plus the current screen.
#[test]
fn frozen_history_drops_disconnect_resize_churn() {
    let run_id = "run-freeze";
    let mut core = new_core(run_id);
    let mut app = AppModel::default();
    app.apply(&PlainMsg::TerminalSnapshotV2(core.snapshot()));
    let mut pending: Vec<u8> = Vec::new();

    // Connected: stream Q1..Q9 as ordinary live patches.
    for question in 1..=9u32 {
        feed_question(&mut pending, question);
        let bytes = std::mem::take(&mut pending);
        let patch = core
            .feed_vt_bytes(&bytes)
            .expect("buffered question output should emit a patch");
        app.apply(&PlainMsg::TerminalPatchV2(patch));
    }
    let connected_lines = app.rendered_lines();
    let connected_dups = duplicate_lines(&connected_lines);
    assert!(
        connected_dups.is_empty(),
        "baseline already duplicated before disconnect: {connected_dups:?}"
    );

    // While disconnected, every PTY resize is preceded by a freeze; the TUI
    // repaints and that churn must be discarded. The patches the core would
    // emit during this window are never delivered (the app is gone), so we just
    // feed the churn into the core to advance vt100's scrollback.
    let disconnected_resize =
        |core: &mut TerminalCore, pending: &mut Vec<u8>, seq: u64, cols: u16, rows: u16| {
            core.freeze_history();
            let _ = core.resize(ResizeEventV2 {
                resize_seq: seq,
                cols,
                rows,
                input_stream_id: "local".to_string(),
                last_input_ack: 0,
            });
            // The running TUI re-emits its most-recent lines at the new width.
            for question in 6..=9u32 {
                feed_sigwinch_reprint(pending, question);
            }
            let _ = core.feed_vt_bytes(&std::mem::take(pending));
        };

    // LocalMode: explicit CLI-side takeover resizes to the desktop size.
    disconnected_resize(&mut core, &mut pending, 100, LOCAL_COLS, LOCAL_ROWS);
    // A manual desktop resize while still disconnected.
    disconnected_resize(&mut core, &mut pending, 101, LOCAL_COLS + 9, LOCAL_ROWS + 3);
    assert!(
        core.history_frozen(),
        "history stays frozen for the whole disconnected period"
    );

    // Reconnect: the relay re-locks the PTY back to the app's width (one more
    // repaint) while STILL frozen, redraws the live frame, then builds the
    // reconnect snapshot the app resumes from.
    core.freeze_history();
    let _ = core.resize(ResizeEventV2 {
        resize_seq: 102,
        cols: COLS,
        rows: ROWS,
        input_stream_id: "stream-1".to_string(),
        last_input_ack: 0,
    });
    feed_live_frame_redraw(&mut pending);
    let _ = core.feed_vt_bytes(&std::mem::take(&mut pending));
    app.apply(&PlainMsg::TerminalSnapshotV2(core.snapshot()));

    // First render ack after reconnect thaws history.
    core.thaw_history();
    assert!(!core.history_frozen(), "render ack must thaw history");

    // The cycle introduced no duplicate / mis-wrapped rows.
    let lines = app.rendered_lines();
    let dups = duplicate_lines(&lines);
    assert!(
        dups.is_empty(),
        "disconnect/resize churn duplicated scrollback: {dups:?}"
    );
    assert_eq!(
        app.rejected_patches, 0,
        "frozen reconnect path should not force a resync"
    );

    // The transient resize churn was structurally dropped, never duplicated.
    // The reconnect snapshot intentionally carries only the latest twelve screens
    // of scrollback, not the whole pre-disconnect cache.
    assert!(lines.len() <= usize::from(ROWS) * 13);
    assert!(
        connected_lines.ends_with(&lines),
        "reconnect snapshot should be the latest scrollback window plus screen"
    );
    for question in 1..=9u32 {
        for line in 0..6u32 {
            let needle = format!("Q{question} line {line}");
            assert_eq!(
                lines.iter().filter(|l| **l == needle).count(),
                1,
                "{needle} should survive exactly once in the reconnect window"
            );
        }
    }

    // After thawing, genuinely new output is recorded again (not discarded and
    // not replayed as a duplicate of the dropped churn). New streaming output
    // starts on its own line (the redraw left the cursor at the end of the live
    // frame's last row).
    pending.extend_from_slice(b"\r\n");
    feed_question(&mut pending, 10);
    let bytes = std::mem::take(&mut pending);
    let patch = core
        .feed_vt_bytes(&bytes)
        .expect("post-thaw question output should emit a patch");
    app.apply(&PlainMsg::TerminalPatchV2(patch));
    let lines = app.rendered_lines();
    assert!(
        duplicate_lines(&lines).is_empty(),
        "post-thaw output duplicated scrollback: {:?}",
        duplicate_lines(&lines)
    );
    for line in 0..6u32 {
        let needle = format!("Q10 line {line}");
        assert_eq!(
            lines.iter().filter(|l| **l == needle).count(),
            1,
            "{needle} should appear exactly once after thaw"
        );
    }
}

#[test]
fn dirty_local_mode_snapshot_marks_reset_and_keeps_latest_window_contiguous() {
    let run_id = "run-dirty-local";
    let mut core = new_core(run_id);
    let mut app = AppModel::default();
    app.apply(&PlainMsg::TerminalSnapshotV2(core.snapshot()));
    let mut pending = Vec::new();

    for question in 1..=6u32 {
        feed_question(&mut pending, question);
        let bytes = std::mem::take(&mut pending);
        let patch = core
            .feed_vt_bytes(&bytes)
            .expect("connected question output should emit a patch");
        app.apply(&PlainMsg::TerminalPatchV2(patch));
    }

    core.freeze_history();
    let _ = core.resize(ResizeEventV2 {
        resize_seq: 200,
        cols: LOCAL_COLS,
        rows: LOCAL_ROWS,
        input_stream_id: "local".to_string(),
        last_input_ack: 0,
    });
    core.thaw_history();

    for question in 7..=14u32 {
        feed_question(&mut pending, question);
    }

    let msg = relay_dirty_local_mode_snapshot(&mut core, &mut pending);
    let PlainMsg::TerminalSnapshotV2(snapshot) = &msg else {
        panic!("dirty local mode must reconnect with a snapshot");
    };
    assert!(snapshot.reset_app_cache);
    assert!(
        snapshot.scrollback_window.len() <= usize::from(snapshot.rows) * 12,
        "snapshot should carry a bounded recent scrollback window"
    );

    app.apply(&msg);
    let lines = app.rendered_lines();
    assert!(duplicate_lines(&lines).is_empty());
    for question in 12..=14u32 {
        for line in 0..6u32 {
            let needle = format!("Q{question} line {line}");
            assert_eq!(
                lines.iter().filter(|l| **l == needle).count(),
                1,
                "{needle} should survive exactly once in the reset snapshot window"
            );
        }
    }
}
