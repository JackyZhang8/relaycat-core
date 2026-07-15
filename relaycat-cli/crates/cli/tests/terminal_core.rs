use relaycat_cli::terminal_core::{TerminalCore, TerminalCoreConfig};
use relaycat_protocol::{
    CellRun, PaletteState, PatchOp, PlainMsg, RenderAckV2, RequestTranscriptV2, ResizeEventV2,
    ResumeV2, TerminalCell, TerminalColor, TerminalMode, TerminalRow, TerminalTranscriptEntryKind,
    encode_plain_msg,
};
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

const RELAY_SAFE_PLAIN_MSG_BYTES: usize = 960 * 1024;
const MAX_WIRE_ATTRS: usize = 4_096;

fn text_run(attr_id: u32, text: &str) -> Vec<CellRun> {
    vec![CellRun {
        attr_id,
        cells: text
            .chars()
            .map(|ch| TerminalCell {
                text: ch.to_string(),
                width: 1,
            })
            .collect(),
    }]
}

fn row_text(row: &relaycat_protocol::TerminalRow, cols: usize) -> String {
    let mut text: String = row
        .cells
        .iter()
        .flat_map(|run| run.cells.iter())
        .map(|cell| cell.text.as_str())
        .collect();
    while text.chars().count() < cols {
        text.push(' ');
    }
    text
}

fn encoded_plain_len(msg: PlainMsg) -> usize {
    encode_plain_msg(&msg).expect("encode plain message").len()
}

fn assert_large_truecolor_batch_is_bounded(incremental_attrs: bool) {
    const UNIQUE_ATTRS: usize = 12_000;
    let cols = 4_096_u16;
    let rows = 3_u16;
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols,
        rows,
        patch_retention: 64,
    });
    if incremental_attrs {
        core.set_incremental_attrs_flag(Arc::new(AtomicBool::new(true)));
    }
    core.snapshot();

    let mut input = String::new();
    for color in 0..UNIQUE_ATTRS {
        let r = (color >> 16) & 0xff;
        let g = (color >> 8) & 0xff;
        let b = color & 0xff;
        write!(input, "\x1b[38;2;{r};{g};{b}mX").expect("write truecolor VT input");
    }

    let patches = core.feed_vt_bytes_batch(input.as_bytes());

    assert!(!patches.is_empty());
    for (index, patch) in patches.iter().enumerate() {
        let encoded_len = encoded_plain_len(PlainMsg::TerminalPatchV2(patch.clone()));
        assert!(
            encoded_len <= RELAY_SAFE_PLAIN_MSG_BYTES,
            "fragment {index} encoded to {encoded_len} bytes"
        );
    }
    let snapshot = core.snapshot();
    assert_eq!(snapshot.attrs.len(), MAX_WIRE_ATTRS);
    let visible_text = snapshot
        .screen_rows
        .iter()
        .map(|row| row_text(row, usize::from(cols)))
        .collect::<String>();
    assert_eq!(visible_text.trim_end(), "X".repeat(UNIQUE_ATTRS));
    assert!(snapshot.screen_rows.iter().all(|row| {
        row.cells
            .iter()
            .all(|run| usize::try_from(run.attr_id).is_ok_and(|id| id < MAX_WIRE_ATTRS))
    }));
}

#[test]
fn terminal_core_initial_snapshot_is_complete_keyframe() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 4,
        rows: 2,
        patch_retention: 8,
    });

    let snapshot = core.snapshot();
    assert_eq!(snapshot.terminal_run_id, "run-1");
    assert_eq!(snapshot.snapshot_id, 1);
    assert_eq!(snapshot.state_seq, 0);
    assert_eq!(snapshot.cols, 4);
    assert_eq!(snapshot.rows, 2);
    assert_eq!(snapshot.screen_rows.len(), 2);
    assert_eq!(row_text(&snapshot.screen_rows[0], 4), "    ");
    assert_eq!(snapshot.attrs.len(), 1);
}

#[test]
fn terminal_core_initial_snapshot_carries_configured_palette() {
    let palette = PaletteState {
        default_fg: TerminalColor::Rgb { r: 1, g: 2, b: 3 },
        default_bg: TerminalColor::Rgb {
            r: 250,
            g: 251,
            b: 252,
        },
        cursor: TerminalColor::Rgb { r: 7, g: 8, b: 9 },
        ansi: (0..16).map(TerminalColor::Indexed).collect(),
    };
    let mut core = TerminalCore::new_with_palette(
        TerminalCoreConfig {
            terminal_run_id: "run-1".to_string(),
            cols: 4,
            rows: 2,
            patch_retention: 8,
        },
        palette.clone(),
    );

    let snapshot = core.snapshot();

    assert_eq!(snapshot.palette, palette);
}

#[test]
fn terminal_core_apply_ops_emits_contiguous_patch() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 4,
        rows: 2,
        patch_retention: 8,
    });
    let base = core.snapshot();

    let patch = core.apply_ops(vec![
        PatchOp::PutCells {
            row: 0,
            col: 0,
            cells: text_run(0, "hi"),
        },
        PatchOp::SetMode {
            mode: TerminalMode::BracketedPaste,
            enabled: true,
        },
    ]);

    assert_eq!(patch.base_snapshot_id, base.snapshot_id);
    assert_eq!(patch.from_state_seq, 1);
    assert_eq!(patch.to_state_seq, 1);
    assert_eq!(
        patch.validate_against(base.snapshot_id, base.state_seq),
        Ok(())
    );

    let snapshot = core.snapshot();
    assert_eq!(snapshot.state_seq, 1);
    assert_eq!(row_text(&snapshot.screen_rows[0], 4), "hi  ");
    assert!(snapshot.modes.bracketed_paste);
}

#[test]
fn terminal_core_resends_full_attr_table_without_incremental_capability() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 8,
        rows: 2,
        patch_retention: 8,
    });
    core.snapshot();

    let patch = core
        .feed_vt_bytes(b"\x1b[31mred")
        .expect("colored output should emit a patch");

    // The table grew (default + red); legacy peers receive the whole table.
    assert_eq!(patch.attrs_base_len, None);
    assert_eq!(patch.attrs.len(), 2);
}

#[test]
fn terminal_core_emits_incremental_attr_tail_when_capability_negotiated() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 8,
        rows: 2,
        patch_retention: 8,
    });
    core.set_incremental_attrs_flag(Arc::new(AtomicBool::new(true)));
    core.snapshot();

    let patch = core
        .feed_vt_bytes(b"\x1b[31mred")
        .expect("colored output should emit a patch");

    // Only the appended tail (the red attr) is sent, keyed by its base index.
    assert_eq!(patch.attrs_base_len, Some(1));
    assert_eq!(patch.attrs.len(), 1);

    // A later patch that does not grow the table sends neither attrs nor a base.
    let noop_growth = core
        .feed_vt_bytes(b"\x1b[31mX")
        .expect("more red output should emit a patch");
    assert_eq!(noop_growth.attrs_base_len, None);
    assert!(noop_growth.attrs.is_empty());
}

#[test]
fn terminal_core_does_not_replay_incremental_attr_tail_to_legacy_peer() {
    let incremental_attrs = Arc::new(AtomicBool::new(true));
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 8,
        rows: 2,
        patch_retention: 64,
    });
    core.set_incremental_attrs_flag(incremental_attrs.clone());
    let base = core.snapshot();
    let patch = core
        .feed_vt_bytes(b"\x1b[31mX")
        .expect("new attribute should emit a patch");
    assert!(patch.attrs_base_len.is_some());

    incremental_attrs.store(false, std::sync::atomic::Ordering::Release);
    let messages = core
        .resume_messages(&ResumeV2 {
            terminal_run_id: Some(base.terminal_run_id),
            last_applied_state_seq: base.state_seq,
            last_snapshot_id: Some(base.snapshot_id),
            input_stream_id: "stream-1".to_string(),
            last_input_ack: 0,
        })
        .expect("legacy resume fallback");

    assert!(matches!(
        messages.first(),
        Some(PlainMsg::TerminalSnapshotV2(snapshot)) if snapshot.reset_app_cache
    ));
}

#[test]
fn terminal_core_bounds_large_truecolor_attr_table_for_legacy_peers() {
    assert_large_truecolor_batch_is_bounded(false);
}

#[test]
fn terminal_core_bounds_large_truecolor_attr_table_for_incremental_peers() {
    assert_large_truecolor_batch_is_bounded(true);
}

#[test]
fn terminal_core_suppresses_noop_vt_chunks() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 4,
        rows: 2,
        patch_retention: 8,
    });
    let base = core.snapshot();

    let patch = core.feed_vt_bytes(b"\x1b[H");

    assert!(
        patch.is_none(),
        "VT bytes that leave the semantic terminal unchanged must not emit a patch"
    );
    assert_eq!(core.snapshot().state_seq, base.state_seq);
}

#[test]
fn terminal_core_resize_creates_new_snapshot_base() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 4,
        rows: 2,
        patch_retention: 8,
    });
    let first = core.snapshot();

    let (ack, snapshot) = core.resize(ResizeEventV2 {
        resize_seq: 7,
        cols: 6,
        rows: 3,
        input_stream_id: "stream-1".to_string(),
        last_input_ack: 0,
    });

    assert_eq!(ack.resize_seq, 7);
    assert_ne!(snapshot.snapshot_id, first.snapshot_id);
    assert_eq!(snapshot.state_seq, 0);
    assert_eq!(snapshot.cols, 6);
    assert_eq!(snapshot.rows, 3);
    assert_eq!(row_text(&snapshot.screen_rows[0], 6), "      ");
}

#[test]
fn terminal_core_resize_snapshot_preserves_current_screen_contents() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 8,
        rows: 3,
        patch_retention: 8,
    });
    core.feed_vt_bytes(b"hello");

    let (_ack, snapshot) = core.resize(ResizeEventV2 {
        resize_seq: 7,
        cols: 10,
        rows: 4,
        input_stream_id: "stream-1".to_string(),
        last_input_ack: 0,
    });

    assert_eq!(snapshot.cols, 10);
    assert_eq!(snapshot.rows, 4);
    assert_eq!(row_text(&snapshot.screen_rows[0], 10), "hello     ");
}

#[test]
fn terminal_core_resize_to_same_size_does_not_reset_snapshot_base_or_screen() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 8,
        rows: 3,
        patch_retention: 8,
    });
    core.feed_vt_bytes(b"hello");
    let before = core.snapshot();

    let (ack, snapshot) = core.resize(ResizeEventV2 {
        resize_seq: 8,
        cols: 8,
        rows: 3,
        input_stream_id: "stream-1".to_string(),
        last_input_ack: 0,
    });

    assert_eq!(ack.resize_seq, 8);
    assert_eq!(snapshot.snapshot_id, before.snapshot_id);
    assert_eq!(snapshot.state_seq, before.state_seq);
    assert_eq!(row_text(&snapshot.screen_rows[0], 8), "hello   ");
}

#[test]
fn terminal_core_resize_trims_trailing_wide_cell_without_vt100_panic() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 50,
        rows: 4,
        patch_retention: 8,
    });

    core.feed_vt_bytes("\x1b[1;49H界".as_bytes());
    core.resize(ResizeEventV2 {
        resize_seq: 1,
        cols: 49,
        rows: 4,
        input_stream_id: "stream-1".to_string(),
        last_input_ack: 0,
    });
    core.feed_vt_bytes(b"\x1b[K");
}

#[test]
fn terminal_core_repeated_resizes_do_not_duplicate_screen_into_scrollback() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 8,
        rows: 3,
        patch_retention: 8,
    });
    core.feed_vt_bytes(b"hello");

    core.resize(ResizeEventV2 {
        resize_seq: 1,
        cols: 10,
        rows: 4,
        input_stream_id: "stream-1".to_string(),
        last_input_ack: 0,
    });
    core.resize(ResizeEventV2 {
        resize_seq: 2,
        cols: 8,
        rows: 3,
        input_stream_id: "stream-1".to_string(),
        last_input_ack: 0,
    });
    let (_ack, snapshot) = core.resize(ResizeEventV2 {
        resize_seq: 3,
        cols: 10,
        rows: 4,
        input_stream_id: "stream-1".to_string(),
        last_input_ack: 0,
    });

    let repeated_current_screen_rows = snapshot
        .scrollback_window
        .iter()
        .filter(|row| row_text(row, 10).trim_end() == "hello")
        .count();
    assert_eq!(0, repeated_current_screen_rows);
    assert_eq!(row_text(&snapshot.screen_rows[0], 10), "hello     ");
}

#[test]
fn terminal_core_resize_redraw_captures_only_genuinely_scrolled_rows() {
    // A resize followed by a TUI repaint must record only the rows that truly
    // scrolled off the top of the screen, exactly as the local terminal shows
    // them — never an amplified copy of the whole window (the old heuristic's
    // failure mode). Here a single line ("top") scrolls off when the repaint
    // pushes the screen down; the reprinted "top" legitimately appears on
    // screen too, but the scrollback holds just the one row that scrolled.
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 8,
        rows: 2,
        patch_retention: 8,
    });
    core.feed_vt_bytes(b"top\r\nbottom");

    core.resize(ResizeEventV2 {
        resize_seq: 1,
        cols: 10,
        rows: 3,
        input_stream_id: "local".to_string(),
        last_input_ack: 0,
    });
    core.feed_vt_bytes(b"\r\ntop\r\nbottom");

    let snapshot = core.snapshot();
    let scrollback: Vec<String> = snapshot
        .scrollback_window
        .iter()
        .map(|row| {
            row_text(row, usize::from(snapshot.cols))
                .trim_end()
                .to_string()
        })
        .collect();

    assert_eq!(scrollback, vec!["top".to_string()]);
}

#[test]
fn terminal_core_retains_contiguous_patches_after_render_ack() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 4,
        rows: 2,
        patch_retention: 8,
    });
    let base = core.snapshot();
    let first = core.apply_ops(vec![PatchOp::SetTitle("one".to_string())]);
    let second = core.apply_ops(vec![PatchOp::SetTitle("two".to_string())]);

    assert_eq!(
        core.retained_patches_after(base.snapshot_id, base.state_seq)
            .expect("patches retained")
            .len(),
        2
    );

    core.ack_render(RenderAckV2 {
        terminal_run_id: "run-1".to_string(),
        snapshot_id: base.snapshot_id,
        applied_state_seq: first.to_state_seq,
    });

    assert!(
        core.retained_patches_after(base.snapshot_id, base.state_seq)
            .is_none()
    );
    assert_eq!(
        core.retained_patches_after(base.snapshot_id, first.to_state_seq)
            .expect("second patch retained"),
        vec![second]
    );
}

#[test]
fn terminal_core_resume_returns_retained_patches_when_contiguous() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 4,
        rows: 2,
        patch_retention: 8,
    });
    let base = core.snapshot();
    let patch = core.apply_ops(vec![PatchOp::SetTitle("one".to_string())]);

    let messages = core
        .resume_messages(&ResumeV2 {
            terminal_run_id: Some(base.terminal_run_id.clone()),
            last_applied_state_seq: base.state_seq,
            last_snapshot_id: Some(base.snapshot_id),
            input_stream_id: "stream-1".to_string(),
            last_input_ack: 0,
        })
        .expect("resume retained patch");

    assert_eq!(messages, vec![PlainMsg::TerminalPatchV2(patch)]);
}

#[test]
fn terminal_core_resume_coalesces_contiguous_retained_patches() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 8,
        rows: 3,
        patch_retention: 8,
    });
    let base = core.snapshot();
    let first_ops = vec![PatchOp::SetTitle("one".to_string())];
    let second_ops = vec![PatchOp::PutCells {
        row: 0,
        col: 0,
        cells: text_run(0, "hi"),
    }];
    let third_ops = vec![PatchOp::SetMode {
        mode: TerminalMode::BracketedPaste,
        enabled: true,
    }];
    core.apply_ops(first_ops.clone());
    core.apply_ops(second_ops.clone());
    core.apply_ops(third_ops.clone());

    let messages = core
        .resume_messages(&ResumeV2 {
            terminal_run_id: Some(base.terminal_run_id.clone()),
            last_applied_state_seq: base.state_seq,
            last_snapshot_id: Some(base.snapshot_id),
            input_stream_id: "stream-1".to_string(),
            last_input_ack: 0,
        })
        .expect("resume coalesced patches");

    let [PlainMsg::TerminalPatchV2(patch)] = messages.as_slice() else {
        panic!("resume should coalesce contiguous retained patches into one patch");
    };
    assert_eq!(patch.from_state_seq, 1);
    assert_eq!(patch.to_state_seq, 3);
    assert_eq!(
        patch.validate_against(base.snapshot_id, base.state_seq),
        Ok(())
    );
    let expected_ops = [first_ops, second_ops, third_ops].concat();
    assert_eq!(patch.ops, expected_ops);
}

#[test]
fn terminal_core_resume_uses_snapshot_when_replay_has_more_than_128_patches() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 8,
        rows: 3,
        patch_retention: 256,
    });
    let base = core.snapshot();
    for index in 0..129 {
        core.apply_ops(vec![PatchOp::SetTitle(format!("title-{index}"))]);
    }

    let messages = core
        .resume_messages(&ResumeV2 {
            terminal_run_id: Some(base.terminal_run_id.clone()),
            last_applied_state_seq: base.state_seq,
            last_snapshot_id: Some(base.snapshot_id),
            input_stream_id: "stream-1".to_string(),
            last_input_ack: 0,
        })
        .expect("large replay should fall back to snapshot");

    assert!(matches!(
        messages.first(),
        Some(PlainMsg::TerminalSnapshotV2(snapshot)) if snapshot.reset_app_cache
    ));
}

#[test]
fn terminal_core_resume_replays_exactly_128_small_patches() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 8,
        rows: 3,
        patch_retention: 256,
    });
    let base = core.snapshot();
    for index in 0..128 {
        core.apply_ops(vec![PatchOp::SetTitle(format!("title-{index}"))]);
    }

    let messages = core
        .resume_messages(&ResumeV2 {
            terminal_run_id: Some(base.terminal_run_id.clone()),
            last_applied_state_seq: base.state_seq,
            last_snapshot_id: Some(base.snapshot_id),
            input_stream_id: "stream-1".to_string(),
            last_input_ack: 0,
        })
        .expect("replay at the patch-count limit should remain incremental");

    assert!(matches!(
        messages.as_slice(),
        [PlainMsg::TerminalPatchV2(patch)] if patch.from_state_seq == 1 && patch.to_state_seq == 128
    ));
}

#[test]
fn terminal_core_resume_uses_snapshot_when_replay_exceeds_512_kib_total() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 8,
        rows: 3,
        patch_retention: 8,
    });
    let base = core.snapshot();
    core.apply_ops(vec![PatchOp::SetTitle("a".repeat(300 * 1024))]);
    core.apply_ops(vec![PatchOp::SetTitle("b".repeat(300 * 1024))]);

    let messages = core
        .resume_messages(&ResumeV2 {
            terminal_run_id: Some(base.terminal_run_id.clone()),
            last_applied_state_seq: base.state_seq,
            last_snapshot_id: Some(base.snapshot_id),
            input_stream_id: "stream-1".to_string(),
            last_input_ack: 0,
        })
        .expect("large replay should fall back to snapshot");

    assert!(matches!(
        messages.first(),
        Some(PlainMsg::TerminalSnapshotV2(snapshot)) if snapshot.reset_app_cache
    ));
}

#[test]
fn terminal_core_resume_coalesced_patch_carries_latest_attr_table() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 16,
        rows: 4,
        patch_retention: 8,
    });
    let base = core.snapshot();
    let introduces_attr = core
        .feed_vt_bytes(b"\x1b[31mred\x1b[0m")
        .expect("visible VT output should emit a patch");
    assert!(!introduces_attr.attrs.is_empty());
    let reuses_attr = core
        .feed_vt_bytes(b" plain")
        .expect("visible VT output should emit a patch");
    assert!(reuses_attr.attrs.is_empty());

    let messages = core
        .resume_messages(&ResumeV2 {
            terminal_run_id: Some(base.terminal_run_id.clone()),
            last_applied_state_seq: base.state_seq,
            last_snapshot_id: Some(base.snapshot_id),
            input_stream_id: "stream-1".to_string(),
            last_input_ack: 0,
        })
        .expect("resume coalesced attrs patch");

    let [PlainMsg::TerminalPatchV2(patch)] = messages.as_slice() else {
        panic!("resume should coalesce retained patches that share one snapshot base");
    };
    assert_eq!(patch.from_state_seq, introduces_attr.from_state_seq);
    assert_eq!(patch.to_state_seq, reuses_attr.to_state_seq);
    assert!(
        patch
            .attrs
            .iter()
            .any(|attr| attr.fg == TerminalColor::Indexed(1)),
        "coalesced patch must carry the latest non-empty attr table"
    );
    let expected_ops = [introduces_attr.ops, reuses_attr.ops].concat();
    assert_eq!(patch.ops, expected_ops);
}

#[test]
fn terminal_core_resume_falls_back_to_snapshot_when_patch_base_is_missing() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 4,
        rows: 2,
        patch_retention: 1,
    });
    let base = core.snapshot();
    core.apply_ops(vec![PatchOp::SetTitle("one".to_string())]);
    core.apply_ops(vec![PatchOp::SetTitle("two".to_string())]);

    let messages = core
        .resume_messages(&ResumeV2 {
            terminal_run_id: Some(base.terminal_run_id.clone()),
            last_applied_state_seq: base.state_seq,
            last_snapshot_id: Some(base.snapshot_id),
            input_stream_id: "stream-1".to_string(),
            last_input_ack: 0,
        })
        .expect("fallback snapshot transaction");

    let [PlainMsg::TerminalSnapshotV2(snapshot)] = messages.as_slice() else {
        panic!("missing retained base should fall back to a reset snapshot");
    };
    assert!(snapshot.reset_app_cache);
}

#[test]
fn terminal_core_resume_falls_back_to_snapshot_when_patch_replay_exceeds_budget() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 4,
        rows: 2,
        patch_retention: 8,
    });
    let base = core.snapshot();
    let oversized_line = TerminalRow {
        line_id: 99,
        wrapped: false,
        cells: text_run(0, &"x".repeat(RELAY_SAFE_PLAIN_MSG_BYTES + 1)),
    };
    core.apply_ops(vec![PatchOp::ReplaceRow {
        row: 0,
        line: oversized_line,
    }]);

    let messages = core
        .resume_messages(&ResumeV2 {
            terminal_run_id: Some(base.terminal_run_id.clone()),
            last_applied_state_seq: base.state_seq,
            last_snapshot_id: Some(base.snapshot_id),
            input_stream_id: "stream-1".to_string(),
            last_input_ack: 0,
        })
        .expect("oversized replay fallback");

    let [PlainMsg::TerminalSnapshotV2(snapshot)] = messages.as_slice() else {
        panic!("oversized patch replay should fall back to a reset snapshot");
    };
    assert!(snapshot.reset_app_cache);
}

#[test]
fn terminal_core_resume_replays_scrollback_patches_within_twelve_screens() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 12,
        rows: 2,
        patch_retention: 8,
    });
    let base = core.snapshot();
    let rows = (0..24)
        .map(|index| TerminalRow {
            line_id: 100 + index,
            wrapped: false,
            cells: text_run(0, &format!("row-{index:02}")),
        })
        .collect();
    let patch = core.apply_ops(vec![PatchOp::AppendScrollback { rows }]);

    let messages = core
        .resume_messages(&ResumeV2 {
            terminal_run_id: Some(base.terminal_run_id.clone()),
            last_applied_state_seq: base.state_seq,
            last_snapshot_id: Some(base.snapshot_id),
            input_stream_id: "stream-1".to_string(),
            last_input_ack: 0,
        })
        .expect("resume scrollback within snapshot window");

    assert_eq!(messages, vec![PlainMsg::TerminalPatchV2(patch)]);
}

#[test]
fn terminal_core_resume_replays_scrollback_beyond_twelve_screens() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 12,
        rows: 2,
        patch_retention: 8,
    });
    let base = core.snapshot();
    let rows = (0..25)
        .map(|index| TerminalRow {
            line_id: 100 + index,
            wrapped: false,
            cells: text_run(0, &format!("row-{index:02}")),
        })
        .collect();
    let patch = core.apply_ops(vec![PatchOp::AppendScrollback { rows }]);

    let messages = core
        .resume_messages(&ResumeV2 {
            terminal_run_id: Some(base.terminal_run_id.clone()),
            last_applied_state_seq: base.state_seq,
            last_snapshot_id: Some(base.snapshot_id),
            input_stream_id: "stream-1".to_string(),
            last_input_ack: 0,
        })
        .expect("resume scrollback beyond snapshot window");

    assert_eq!(messages, vec![PlainMsg::TerminalPatchV2(patch)]);
}

#[test]
fn terminal_core_feed_vt_bytes_tracks_cells_attrs_and_modes() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 8,
        rows: 3,
        patch_retention: 8,
    });
    let base = core.snapshot();

    let patch = core
        .feed_vt_bytes(b"\x1b[31mR\x1b[0m ok\x1b[?2004h")
        .expect("visible VT output should emit a patch");

    assert_eq!(
        patch.validate_against(base.snapshot_id, base.state_seq),
        Ok(())
    );
    assert!(
        patch
            .attrs
            .iter()
            .any(|attr| attr.fg == TerminalColor::Indexed(1))
    );

    let snapshot = core.snapshot();
    assert_eq!(snapshot.state_seq, 1);
    assert_eq!(row_text(&snapshot.screen_rows[0], 8), "R ok    ");
    assert!(snapshot.modes.bracketed_paste);

    let red_attr_id = snapshot.screen_rows[0].cells[0].attr_id as usize;
    assert_eq!(snapshot.attrs[red_attr_id].fg, TerminalColor::Indexed(1));
}

#[test]
fn terminal_core_snapshot_carries_scrollback_after_screen_overflow() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 8,
        rows: 2,
        patch_retention: 8,
    });

    core.feed_vt_bytes(b"one\r\ntwo\r\nthree");

    let snapshot = core.snapshot();

    assert!(!snapshot.scrollback_window.is_empty());
    assert!(
        snapshot
            .scrollback_window
            .iter()
            .any(|row| row_text(row, 8).starts_with("one"))
    );
}

#[test]
fn terminal_core_snapshot_carries_recent_twelve_screens_of_scrollback() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 12,
        rows: 4,
        patch_retention: 8,
    });

    for index in 0..60 {
        core.feed_vt_bytes(format!("row-{index:02}\r\n").as_bytes());
    }

    let snapshot = core.snapshot();

    assert_eq!(snapshot.scrollback_window.len(), 48);
    let texts = snapshot
        .scrollback_window
        .iter()
        .map(|row| row_text(row, 12).trim_end().to_string())
        .collect::<Vec<_>>();
    assert_eq!(texts.first().map(String::as_str), Some("row-09"));
    assert_eq!(texts.last().map(String::as_str), Some("row-56"));
}

#[test]
fn terminal_core_snapshot_can_mark_app_cache_reset() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 12,
        rows: 4,
        patch_retention: 8,
    });

    for index in 0..60 {
        core.feed_vt_bytes(format!("row-{index:02}\r\n").as_bytes());
    }

    let snapshot = core.snapshot_with_reset_app_cache(true);

    assert!(snapshot.reset_app_cache);
    assert_eq!(snapshot.scrollback_window.len(), 48);
    let texts = snapshot
        .scrollback_window
        .iter()
        .map(|row| row_text(row, 12).trim_end().to_string())
        .collect::<Vec<_>>();
    assert_eq!(texts.first().map(String::as_str), Some("row-09"));
    assert_eq!(texts.last().map(String::as_str), Some("row-56"));
}

#[test]
fn terminal_core_same_size_resize_can_mark_app_cache_reset() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 12,
        rows: 4,
        patch_retention: 8,
    });
    let initial = core.snapshot();
    core.feed_vt_bytes(b"new output\r\n");

    let (ack, snapshot) = core.resize_with_reset_app_cache(
        ResizeEventV2 {
            resize_seq: 9,
            cols: 12,
            rows: 4,
            input_stream_id: "stream-1".to_string(),
            last_input_ack: 0,
        },
        true,
    );

    assert_eq!(ack.resize_seq, 9);
    assert!(snapshot.reset_app_cache);
    assert!(snapshot.snapshot_id > initial.snapshot_id);
    assert_eq!(snapshot.cols, 12);
    assert_eq!(snapshot.rows, 4);
}

#[test]
fn terminal_core_feed_vt_bytes_emits_compact_patch_under_relay_frame_budget() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 160,
        rows: 48,
        patch_retention: 8,
    });
    core.snapshot();

    let patch = core
        .feed_vt_bytes(b"\x1b[31mR\x1b[0m")
        .expect("visible VT output should emit a patch");
    let encoded_len = encoded_plain_len(PlainMsg::TerminalPatchV2(patch.clone()));

    assert!(
        patch.ops.len() < 12,
        "single-cell update should not resend the whole screen: {} ops",
        patch.ops.len()
    );
    assert!(
        encoded_len <= RELAY_SAFE_PLAIN_MSG_BYTES,
        "patch encoded to {encoded_len} bytes"
    );
}

#[test]
fn terminal_core_splits_oversized_scrollback_into_contiguous_patches() {
    let cols = 240_u16;
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols,
        rows: 2,
        patch_retention: 64,
    });
    let base = core.snapshot();
    let mut input = Vec::new();
    for index in 0..400 {
        input.extend_from_slice(format!("line-{index:04}-{}\r\n", "x".repeat(220)).as_bytes());
    }

    let patches = core.feed_vt_bytes_batch(&input);

    assert!(patches.len() > 1, "large parser drain should be fragmented");
    for (index, patch) in patches.iter().enumerate() {
        let expected_seq = base.state_seq + index as u64 + 1;
        assert_eq!(patch.from_state_seq, expected_seq);
        assert_eq!(patch.to_state_seq, expected_seq);
        let encoded_len = encoded_plain_len(PlainMsg::TerminalPatchV2(patch.clone()));
        assert!(
            encoded_len <= RELAY_SAFE_PLAIN_MSG_BYTES,
            "fragment {index} encoded to {encoded_len} bytes"
        );
    }
    assert!(patches[..patches.len() - 1].iter().all(|patch| {
        patch
            .ops
            .iter()
            .all(|op| matches!(op, PatchOp::AppendScrollback { .. }))
    }));
    assert!(patches.last().is_some_and(|patch| {
        patch
            .ops
            .iter()
            .any(|op| !matches!(op, PatchOp::AppendScrollback { .. }))
    }));

    let appended = patches
        .iter()
        .flat_map(|patch| appended_row_texts(patch, usize::from(cols)))
        .collect::<Vec<_>>();
    let expected = (0..399)
        .map(|index| format!("line-{index:04}-{}", "x".repeat(220)))
        .collect::<Vec<_>>();
    assert_eq!(appended, expected);
}

#[test]
fn terminal_core_splits_oversized_screen_diff_into_contiguous_patches() {
    let cols = 240_u16;
    let rows = 300_u16;
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols,
        rows,
        patch_retention: 64,
    });
    let base = core.snapshot();
    let mut input = Vec::new();
    for row in 1..=rows {
        input.extend_from_slice(format!("\x1b[{row};1H{}", "x".repeat(220)).as_bytes());
    }

    let patches = core.feed_vt_bytes_batch(&input);

    assert!(patches.len() > 1, "large screen diff should be fragmented");
    for (index, patch) in patches.iter().enumerate() {
        let expected_seq = base.state_seq + index as u64 + 1;
        assert_eq!(patch.from_state_seq, expected_seq);
        assert_eq!(patch.to_state_seq, expected_seq);
        assert!(
            patch
                .ops
                .iter()
                .all(|op| { !matches!(op, PatchOp::AppendScrollback { .. }) })
        );
        let encoded_len = encoded_plain_len(PlainMsg::TerminalPatchV2(patch.clone()));
        assert!(
            encoded_len <= RELAY_SAFE_PLAIN_MSG_BYTES,
            "screen fragment {index} encoded to {encoded_len} bytes"
        );
    }
    assert!(patches.last().is_some_and(|patch| {
        patch
            .ops
            .iter()
            .any(|op| matches!(op, PatchOp::SetCursor(_)))
    }));

    let snapshot = core.snapshot();
    assert_eq!(
        row_text(&snapshot.screen_rows[0], usize::from(cols)).trim_end(),
        "x".repeat(220)
    );
    assert_eq!(
        row_text(
            &snapshot.screen_rows[usize::from(rows - 1)],
            usize::from(cols)
        )
        .trim_end(),
        "x".repeat(220)
    );
}

#[test]
fn resume_replays_remaining_split_scrollback_patches() {
    let cols = 240_u16;
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols,
        rows: 2,
        patch_retention: 64,
    });
    let base = core.snapshot();
    let mut input = Vec::new();
    for index in 0..400 {
        input.extend_from_slice(format!("line-{index:04}-{}\r\n", "x".repeat(220)).as_bytes());
    }
    let patches = core.feed_vt_bytes_batch(&input);
    assert!(patches.len() > 1);
    let first = &patches[0];
    core.ack_render(RenderAckV2 {
        terminal_run_id: base.terminal_run_id.clone(),
        snapshot_id: base.snapshot_id,
        applied_state_seq: first.to_state_seq,
    });

    let messages = core
        .resume_messages(&ResumeV2 {
            terminal_run_id: Some(base.terminal_run_id),
            last_applied_state_seq: first.to_state_seq,
            last_snapshot_id: Some(base.snapshot_id),
            input_stream_id: "stream-1".to_string(),
            last_input_ack: 0,
        })
        .expect("resume split scrollback patches");

    let resumed = messages
        .iter()
        .map(|message| match message {
            PlainMsg::TerminalPatchV2(patch) => patch,
            PlainMsg::TerminalSnapshotV2(_) => {
                panic!("split replay must not fall back to snapshot")
            }
            other => panic!("unexpected resume message: {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        resumed.first().map(|patch| patch.from_state_seq),
        Some(first.to_state_seq + 1)
    );
    assert_eq!(
        resumed.last().map(|patch| patch.to_state_seq),
        patches.last().map(|patch| patch.to_state_seq)
    );
    assert!(resumed.iter().all(|patch| {
        encoded_plain_len(PlainMsg::TerminalPatchV2((*patch).clone())) <= RELAY_SAFE_PLAIN_MSG_BYTES
    }));

    let expected_rows = patches[1..]
        .iter()
        .flat_map(|patch| appended_row_texts(patch, usize::from(cols)))
        .collect::<Vec<_>>();
    let replayed_rows = resumed
        .iter()
        .flat_map(|patch| appended_row_texts(patch, usize::from(cols)))
        .collect::<Vec<_>>();
    assert_eq!(replayed_rows, expected_rows);
}

fn appended_row_texts(patch: &relaycat_protocol::TerminalPatchV2, cols: usize) -> Vec<String> {
    patch
        .ops
        .iter()
        .flat_map(|op| match op {
            PatchOp::AppendScrollback { rows } => rows
                .iter()
                .map(|row| row_text(row, cols).trim_end().to_string())
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .collect()
}

fn assert_ris_transaction_preserves_all_post_reset_rows(split_ris: bool) {
    let cols = 240_u16;
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols,
        rows: 2,
        patch_retention: 64,
    });
    core.snapshot();
    for index in 0..8 {
        core.feed_vt_bytes(format!("old-{index}\r\n").as_bytes());
    }

    let mut input = if split_ris {
        let prefix = core
            .feed_vt_bytes_transport_batch(b"\x1b")
            .expect("partial RIS prefix should be accepted");
        assert!(prefix.reset_messages.is_empty());
        assert!(prefix.patches.is_empty());
        b"c".to_vec()
    } else {
        b"\x07\x1bc".to_vec()
    };
    for index in 0..400 {
        input.extend_from_slice(format!("new-{index:04}-{}\r\n", "x".repeat(220)).as_bytes());
    }

    let batch = core
        .feed_vt_bytes_transport_batch(&input)
        .expect("RIS parser drain should produce a relay-safe transaction");
    let [
        PlainMsg::TerminalSnapshotV2(reset_snapshot),
        reset_tail @ ..,
    ] = batch.reset_messages.as_slice()
    else {
        panic!(
            "RIS transaction must start with a reset snapshot, got {:?}",
            batch.reset_messages
        );
    };
    assert!(reset_snapshot.reset_app_cache);
    assert!(reset_snapshot.scrollback_window.is_empty());

    let reset_patches = reset_tail.iter().map(|message| match message {
        PlainMsg::TerminalPatchV2(patch) => patch,
        other => panic!("unexpected reset transaction message: {other:?}"),
    });
    let patches = reset_patches
        .chain(batch.patches.iter())
        .collect::<Vec<_>>();
    let mut expected_seq = reset_snapshot.state_seq.saturating_add(1);
    for patch in &patches {
        assert_eq!(patch.base_snapshot_id, reset_snapshot.snapshot_id);
        assert_eq!(patch.from_state_seq, expected_seq);
        assert_eq!(patch.to_state_seq, expected_seq);
        expected_seq = expected_seq.saturating_add(1);
        assert!(
            patch.ops.iter().all(|op| !matches!(op, PatchOp::Bell)),
            "bell before RIS must not leak into the post-reset transaction"
        );
    }

    let appended = patches
        .iter()
        .flat_map(|patch| appended_row_texts(patch, usize::from(cols)))
        .collect::<Vec<_>>();
    let expected = (0..399)
        .map(|index| format!("new-{index:04}-{}", "x".repeat(220)))
        .collect::<Vec<_>>();
    assert_eq!(appended, expected);
    assert!(appended.iter().all(|row| !row.starts_with("old-")));

    let resumed = core
        .resume_messages(&ResumeV2 {
            terminal_run_id: Some(reset_snapshot.terminal_run_id.clone()),
            last_applied_state_seq: reset_snapshot.state_seq,
            last_snapshot_id: Some(reset_snapshot.snapshot_id),
            input_stream_id: "stream-1".to_string(),
            last_input_ack: 0,
        })
        .expect("post-RIS patches should remain resumable");
    let Some(PlainMsg::TerminalSnapshotV2(snapshot)) = resumed.first() else {
        panic!("large post-RIS resume must switch to a snapshot transaction");
    };
    assert!(snapshot.reset_app_cache);
    assert_eq!(
        row_text(&snapshot.screen_rows[0], usize::from(cols)).trim_end(),
        format!("new-0399-{}", "x".repeat(220))
    );
}

#[test]
fn terminal_core_ris_transaction_preserves_more_than_snapshot_window() {
    assert_ris_transaction_preserves_all_post_reset_rows(false);
}

#[test]
fn terminal_core_split_ris_transaction_preserves_more_than_snapshot_window() {
    assert_ris_transaction_preserves_all_post_reset_rows(true);
}

#[test]
fn terminal_core_streams_scrolled_rows_via_append_scrollback() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 8,
        rows: 2,
        patch_retention: 64,
    });
    core.snapshot();

    let mut streamed = Vec::new();
    for index in 0..12 {
        let patch = core
            .feed_vt_bytes(format!("line-{index:02}\r\n").as_bytes())
            .expect("streamed line should emit a patch");
        streamed.extend(appended_row_texts(&patch, 8));
    }

    // The most recently written line is still on screen, so only the first 11
    // lines have scrolled off into history.
    let expected: Vec<String> = (0..11).map(|index| format!("line-{index:02}")).collect();
    assert_eq!(streamed, expected);
}

#[test]
fn terminal_core_continues_streaming_scrollback_after_vt_ring_capacity() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 16,
        rows: 2,
        patch_retention: 4_096,
    });
    core.snapshot();

    let mut streamed = Vec::new();
    for index in 0..2_200 {
        let patch = core
            .feed_vt_bytes(format!("line-{index:04}\r\n").as_bytes())
            .expect("each line should change the terminal");
        streamed.extend(appended_row_texts(&patch, 16));
    }

    assert_eq!(streamed.len(), 2_199);
    assert_eq!(streamed.first().map(String::as_str), Some("line-0000"));
    assert_eq!(streamed.last().map(String::as_str), Some("line-2198"));
    assert_eq!(
        core.snapshot()
            .scrollback_window
            .last()
            .map(|row| row_text(row, 16).trim_end().to_string())
            .as_deref(),
        Some("line-2198")
    );
}

#[test]
fn terminal_core_captures_scrollback_evicted_within_one_vt_batch() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 16,
        rows: 2,
        patch_retention: 8,
    });
    core.snapshot();
    let bytes = (0..2_200)
        .map(|index| format!("line-{index:04}\r\n"))
        .collect::<String>();

    let patch = core
        .feed_vt_bytes(bytes.as_bytes())
        .expect("large output patch");
    let streamed = appended_row_texts(&patch, 16);

    assert_eq!(streamed.len(), 2_199);
    assert_eq!(streamed.first().map(String::as_str), Some("line-0000"));
    assert_eq!(streamed.last().map(String::as_str), Some("line-2198"));
    assert_eq!(
        core.snapshot()
            .scrollback_window
            .last()
            .map(|row| row_text(row, 16).trim_end().to_string())
            .as_deref(),
        Some("line-2198")
    );
}

#[test]
fn terminal_core_does_not_dedupe_identical_scrollback_rows() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 16,
        rows: 2,
        patch_retention: 8,
    });
    core.snapshot();
    let bytes = "same\r\n".repeat(2_200);

    let patch = core
        .feed_vt_bytes(bytes.as_bytes())
        .expect("large repeated output patch");
    let streamed = appended_row_texts(&patch, 16);

    assert_eq!(streamed.len(), 2_199);
    assert!(streamed.iter().all(|row| row == "same"));
    assert_eq!(
        core.snapshot()
            .scrollback_window
            .last()
            .map(|row| row_text(row, 16).trim_end().to_string())
            .as_deref(),
        Some("same")
    );
}

#[test]
fn terminal_core_reset_clears_retained_history_and_streams_only_new_rows() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 16,
        rows: 2,
        patch_retention: 8,
    });
    core.snapshot();
    for index in 0..4 {
        core.feed_vt_bytes(format!("old-{index}\r\n").as_bytes());
    }

    let patch = core
        .feed_vt_bytes(b"\x1bcnew-0\r\nnew-1\r\nnew-2\r\n")
        .expect("reset output should emit a patch");

    assert_eq!(
        appended_row_texts(&patch, 16),
        vec!["new-0".to_string(), "new-1".to_string()]
    );
    assert_eq!(
        core.snapshot()
            .scrollback_window
            .iter()
            .map(|row| row_text(row, 16).trim_end().to_string())
            .collect::<Vec<_>>(),
        vec!["new-0".to_string(), "new-1".to_string()]
    );
}

#[test]
fn terminal_core_drains_primary_scrollback_before_entering_alt_screen() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 16,
        rows: 2,
        patch_retention: 8,
    });
    core.snapshot();

    let patch = core
        .feed_vt_bytes(b"primary-0\r\nprimary-1\r\n\x1b[?1049h")
        .expect("primary output and alt-screen entry should emit a patch");

    assert_eq!(appended_row_texts(&patch, 16), vec!["primary-0"]);
    assert!(core.snapshot().modes.alt_screen);
}

#[test]
fn terminal_core_reset_clears_retained_history_before_entering_alt_screen() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 16,
        rows: 2,
        patch_retention: 8,
    });
    core.snapshot();
    for index in 0..4 {
        core.feed_vt_bytes(format!("old-{index}\r\n").as_bytes());
    }
    assert!(!core.snapshot().scrollback_window.is_empty());

    let batch = core
        .feed_vt_bytes_transport_batch(b"\x1bc\x1b[?1049h")
        .expect("reset and alt-screen entry should emit a transaction");
    assert!(batch.has_reset());
    let snapshot = core.snapshot();

    assert!(snapshot.modes.alt_screen);
    assert!(snapshot.scrollback_window.is_empty());
}

#[test]
fn terminal_core_discards_frozen_primary_scrollback_before_entering_alt_screen() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 16,
        rows: 2,
        patch_retention: 8,
    });
    core.snapshot();
    core.freeze_history();

    let enter_patch = core
        .feed_vt_bytes(b"frozen-0\r\nfrozen-1\r\n\x1b[?1049h")
        .expect("frozen output and alt-screen entry should emit a patch");
    assert!(appended_row_texts(&enter_patch, 16).is_empty());

    core.thaw_history();
    let exit_patch = core
        .feed_vt_bytes(b"\x1b[?1049l")
        .expect("alt-screen exit should emit a patch");

    assert!(appended_row_texts(&exit_patch, 16).is_empty());
    assert!(core.snapshot().scrollback_window.is_empty());
}

#[test]
fn terminal_core_streams_top_anchored_scroll_region_rows_via_append_scrollback() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 8,
        rows: 5,
        patch_retention: 64,
    });
    core.snapshot();

    core.feed_vt_bytes(b"\x1b[1;1Hone\x1b[2;1Htwo\x1b[3;1Hthree\x1b[4;1Hfour\x1b[5;1Hstatus");
    let patch = core
        .feed_vt_bytes(b"\x1b[1;4r\x1b[4;1H\r\nfive")
        .expect("scroll-region output should emit a patch");

    assert_eq!(appended_row_texts(&patch, 8), vec!["one".to_string()]);
    assert_eq!(
        core.snapshot()
            .scrollback_window
            .iter()
            .map(|row| row_text(row, 8).trim_end().to_string())
            .collect::<Vec<_>>(),
        vec!["one".to_string()]
    );
}

#[test]
fn terminal_core_ris_does_not_reintroduce_pre_reset_rows() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 8,
        rows: 5,
        patch_retention: 64,
    });
    core.snapshot();
    core.feed_vt_bytes(
        b"\x1b[1;1Hone\x1b[2;1Htwo\x1b[3;1Hthree\x1b[4;1Hfour\x1b[5;1Hstatus\x1b[1;4r",
    );

    let batch = core
        .feed_vt_bytes_transport_batch(b"\x1bctwo\x1b[2;1Hthree\x1b[3;1Hfour")
        .expect("RIS output should emit a transaction");

    assert!(batch.has_reset());
    assert!(
        batch
            .patches
            .iter()
            .flat_map(|patch| appended_row_texts(patch, 8))
            .next()
            .is_none()
    );
    assert!(core.snapshot().scrollback_window.is_empty());
}

#[test]
fn terminal_core_split_ris_does_not_reintroduce_pre_reset_rows() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 8,
        rows: 5,
        patch_retention: 64,
    });
    core.snapshot();
    core.feed_vt_bytes(
        b"\x1b[1;1Hone\x1b[2;1Htwo\x1b[3;1Hthree\x1b[4;1Hfour\x1b[5;1Hstatus\x1b[1;4r",
    );

    core.feed_vt_bytes(b"\x1b");
    let batch = core
        .feed_vt_bytes_transport_batch(b"ctwo\x1b[2;1Hthree\x1b[3;1Hfour")
        .expect("split RIS output should emit a transaction");

    assert!(batch.has_reset());
    assert!(
        batch
            .patches
            .iter()
            .flat_map(|patch| appended_row_texts(patch, 8))
            .next()
            .is_none()
    );
    assert!(core.snapshot().scrollback_window.is_empty());
}

#[test]
fn terminal_core_captures_post_ris_partial_region_scroll_in_same_batch() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 4,
        rows: 3,
        patch_retention: 8,
    });
    core.snapshot();

    let patch = core
        .feed_vt_bytes(b"\x1bc\x1b[1;2r\x1b[1;1Ha\x1b[2;1Hb\r\nc")
        .expect("post-RIS partial-region scroll should emit a patch");

    assert_eq!(appended_row_texts(&patch, 4), ["a"]);
    assert_eq!(
        core.snapshot()
            .scrollback_window
            .iter()
            .map(|row| row_text(row, 4).trim_end().to_string())
            .collect::<Vec<_>>(),
        ["a"]
    );
}

#[test]
fn terminal_core_alt_screen_scroll_region_does_not_pollute_primary_history() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 8,
        rows: 5,
        patch_retention: 8,
    });
    core.snapshot();

    let batch = core
        .feed_vt_bytes_transport_batch(b"\x1bc\x1b[?1049h\x1b[1;4ralt")
        .expect("alt-screen entry should emit a reset transaction");
    assert!(batch.has_reset());
    let exit_patch = core
        .feed_vt_bytes(b"\x1b[?1049l")
        .expect("alt-screen exit should emit a patch");

    assert!(appended_row_texts(&exit_patch, 8).is_empty());
    assert!(core.snapshot().scrollback_window.is_empty());
}

#[test]
fn terminal_core_patch_reuses_attr_table_when_unchanged() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 16,
        rows: 4,
        patch_retention: 8,
    });
    core.snapshot();

    let introduces_attr = core
        .feed_vt_bytes(b"\x1b[31mred\x1b[0m")
        .expect("visible VT output should emit a patch");
    assert!(
        introduces_attr
            .attrs
            .iter()
            .any(|attr| attr.fg == TerminalColor::Indexed(1)),
        "patch that adds a new attribute must carry the table"
    );

    let reuses_attr = core
        .feed_vt_bytes(b" plain")
        .expect("visible VT output should emit a patch");
    assert!(
        reuses_attr.attrs.is_empty(),
        "patch with no new attributes must omit the table (reuse), got {} entries",
        reuses_attr.attrs.len()
    );
}

#[test]
fn terminal_core_records_alt_screen_transcript_without_scrollback() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 12,
        rows: 3,
        patch_retention: 64,
    });
    core.snapshot();

    core.feed_vt_bytes(b"\x1b[?1049h\x1b[Hcodex");
    core.feed_vt_bytes(b"\x1b[Hcodex");

    let snapshot = core.snapshot();
    assert!(snapshot.modes.alt_screen);
    assert!(
        snapshot.scrollback_window.is_empty(),
        "alternate screen must not be terminal scrollback"
    );

    let chunk = core.transcript_chunk(&RequestTranscriptV2 {
        terminal_run_id: "run-1".to_string(),
        before_entry_id: None,
        max_entries: 10,
    });

    assert_eq!(chunk.entries.len(), 1, "identical alt frames are deduped");
    let entry = &chunk.entries[0];
    assert_eq!(entry.kind, TerminalTranscriptEntryKind::AltScreenFrame);
    assert_eq!(entry.terminal_run_id, "run-1");
    assert_eq!(row_text(&entry.rows[0], 12).trim_end(), "codex");
}

#[test]
fn terminal_core_can_record_primary_screen_frames_for_inline_tuis() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 12,
        rows: 3,
        patch_retention: 64,
    });
    core.set_record_primary_screen_frames(true);
    core.snapshot();

    core.feed_vt_bytes(b"\x1b[Hcodex");
    core.feed_vt_bytes(b"\x1b[Hcodex");
    core.feed_vt_bytes(b"\x1b[Hnext ");

    assert!(core.snapshot().scrollback_window.is_empty());
    let chunk = core.transcript_chunk(&RequestTranscriptV2 {
        terminal_run_id: "run-1".to_string(),
        before_entry_id: None,
        max_entries: 10,
    });

    assert_eq!(
        chunk.entries.len(),
        2,
        "identical primary frames are deduped"
    );
    assert_eq!(
        chunk.entries[0].kind,
        TerminalTranscriptEntryKind::ScreenFrame
    );
    assert_eq!(row_text(&chunk.entries[0].rows[0], 12).trim_end(), "codex");
    assert_eq!(row_text(&chunk.entries[1].rows[0], 12).trim_end(), "next");
}

#[test]
fn terminal_core_snapshot_trims_scrollback_under_relay_frame_budget() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 160,
        rows: 48,
        patch_retention: 8,
    });

    for index in 0..600 {
        let line = format!(
            "\x1b[{}mline-{index:04}-{}\x1b[0m\r\n",
            31 + index % 7,
            "x".repeat(120)
        );
        core.feed_vt_bytes(line.as_bytes());
    }

    let snapshot = core.snapshot();
    let encoded_len = encoded_plain_len(PlainMsg::TerminalSnapshotV2(snapshot));

    assert!(
        encoded_len <= RELAY_SAFE_PLAIN_MSG_BYTES,
        "snapshot encoded to {encoded_len} bytes"
    );
}
