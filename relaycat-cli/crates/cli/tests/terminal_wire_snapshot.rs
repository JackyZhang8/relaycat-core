use relaycat_cli::secure::SecureSession;
use relaycat_cli::terminal_core::{TerminalCore, TerminalCoreConfig};
use relaycat_crypto::{KeyPair, SessionKeys};
use relaycat_protocol::{
    Direction, MAX_OUTER_FRAME_BYTES, PatchOp, PlainMsg, RenderAckV2, RequestTranscriptV2,
    ResizeEventV2, ResumeV2, TerminalPatchV2, TerminalRow, encode_plain_msg,
};
use std::fmt::Write as _;

const RELAY_SAFE_PLAIN_MSG_BYTES: usize = 960 * 1024;
const MAX_TERMINAL_CELLS: usize = 262_144;
const MAX_TITLE_BYTES: usize = 4 * 1024;
const MAX_CELL_TEXT_BYTES: usize = 64;

fn populated_large_screen() -> TerminalCore {
    let cols = 260_u16;
    let rows = 300_u16;
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "large-screen-run".to_string(),
        cols,
        rows,
        patch_retention: 64,
    });
    let mut input = Vec::new();
    for row in 1..=rows {
        input.extend_from_slice(format!("\x1b[{row};1H").as_bytes());
        input.extend(std::iter::repeat_n(b'x', 240));
    }
    core.feed_vt_bytes_batch(&input);
    core
}

fn test_keys() -> SessionKeys {
    let cli = KeyPair::from_private_bytes([11; 32]);
    let app = KeyPair::from_private_bytes([12; 32]);
    SessionKeys::derive_for_cli(
        b"room",
        cli.private(),
        app.public(),
        cli.public(),
        app.public(),
        &[13; 32],
        &[14; 32],
        &[15; 32],
    )
}

fn apply_reconstruction(messages: &[PlainMsg]) -> (Vec<TerminalRow>, Vec<TerminalRow>, u64) {
    let PlainMsg::TerminalSnapshotV2(snapshot) = &messages[0] else {
        panic!("snapshot transaction must start with a snapshot");
    };
    let mut scrollback = snapshot.scrollback_window.clone();
    let mut screen = snapshot.screen_rows.clone();
    let mut state_seq = snapshot.state_seq;

    for message in &messages[1..] {
        let PlainMsg::TerminalPatchV2(patch) = message else {
            panic!("snapshot transaction may only contain reconstruction patches");
        };
        assert_eq!(patch.base_snapshot_id, snapshot.snapshot_id);
        assert_eq!(patch.from_state_seq, state_seq + 1);
        for op in &patch.ops {
            match op {
                PatchOp::AppendScrollback { rows } => scrollback.extend(rows.iter().cloned()),
                PatchOp::ReplaceRow { row, line } => {
                    screen[usize::from(*row)] = line.clone();
                }
                other => panic!("unexpected reconstruction op: {other:?}"),
            }
        }
        state_seq = patch.to_state_seq;
    }

    (scrollback, screen, state_seq)
}

#[test]
fn oversized_snapshot_is_fragmented_into_recoverable_messages() {
    let mut core = populated_large_screen();

    let messages = core
        .snapshot_messages(false)
        .expect("large snapshot should be planned as bounded messages");

    assert!(
        messages.len() > 1,
        "large screen should require reconstruction patches"
    );
    assert!(messages.iter().all(|message| {
        encode_plain_msg(message).is_ok_and(|bytes| bytes.len() <= RELAY_SAFE_PLAIN_MSG_BYTES)
    }));

    let (scrollback, screen, final_state_seq) = apply_reconstruction(&messages);
    let expected = core.snapshot();
    assert_eq!(scrollback, expected.scrollback_window);
    assert_eq!(screen, expected.screen_rows);
    assert_eq!(
        final_state_seq,
        messages
            .last()
            .and_then(|message| match message {
                PlainMsg::TerminalPatchV2(patch) => Some(patch.to_state_seq),
                _ => None,
            })
            .expect("large transaction must end with a patch")
    );
}

fn transaction_parts(
    messages: &[PlainMsg],
) -> (
    &relaycat_protocol::TerminalSnapshotV2,
    Vec<&TerminalPatchV2>,
) {
    let PlainMsg::TerminalSnapshotV2(snapshot) = &messages[0] else {
        panic!("snapshot transaction must start with a snapshot");
    };
    let patches = messages[1..]
        .iter()
        .map(|message| match message {
            PlainMsg::TerminalPatchV2(patch) => patch,
            other => panic!("unexpected snapshot transaction message: {other:?}"),
        })
        .collect();
    (snapshot, patches)
}

#[test]
fn snapshot_transaction_resume_replays_only_remaining_patches() {
    let mut core = populated_large_screen();
    let messages = core
        .snapshot_messages(false)
        .expect("large snapshot transaction");
    let (snapshot, patches) = transaction_parts(&messages);
    let first = patches.first().expect("reconstruction patch");
    let snapshot_id = snapshot.snapshot_id;
    let terminal_run_id = snapshot.terminal_run_id.clone();
    let first_seq = first.to_state_seq;
    let final_seq = patches
        .last()
        .expect("final reconstruction patch")
        .to_state_seq;
    core.ack_render(RenderAckV2 {
        terminal_run_id: terminal_run_id.clone(),
        snapshot_id,
        applied_state_seq: first_seq,
    });

    let resumed = core
        .resume_messages(&ResumeV2 {
            terminal_run_id: Some(terminal_run_id),
            last_applied_state_seq: first_seq,
            last_snapshot_id: Some(snapshot_id),
            input_stream_id: "input-stream".to_string(),
            last_input_ack: 0,
        })
        .expect("resume reconstruction patches");

    assert!(!resumed.is_empty());
    assert!(
        resumed
            .iter()
            .all(|message| matches!(message, PlainMsg::TerminalPatchV2(_)))
    );
    let PlainMsg::TerminalPatchV2(first_resumed) = &resumed[0] else {
        unreachable!();
    };
    let PlainMsg::TerminalPatchV2(last_resumed) = resumed.last().expect("last resumed patch")
    else {
        unreachable!();
    };
    assert_eq!(first_resumed.from_state_seq, first_seq + 1);
    assert_eq!(last_resumed.to_state_seq, final_seq);
    assert!(resumed.iter().all(|message| {
        encode_plain_msg(message).is_ok_and(|bytes| bytes.len() <= RELAY_SAFE_PLAIN_MSG_BYTES)
    }));
}

#[test]
fn missing_resume_base_uses_bounded_snapshot_transaction() {
    let mut core = populated_large_screen();

    let messages = core
        .resume_messages(&ResumeV2 {
            terminal_run_id: Some("different-run".to_string()),
            last_applied_state_seq: 0,
            last_snapshot_id: Some(999),
            input_stream_id: "input-stream".to_string(),
            last_input_ack: 0,
        })
        .expect("bounded fallback snapshot transaction");

    assert!(messages.len() > 1, "oversized fallback must be fragmented");
    assert!(
        matches!(messages.first(), Some(PlainMsg::TerminalSnapshotV2(snapshot)) if snapshot.reset_app_cache)
    );
    assert!(messages.iter().all(|message| {
        encode_plain_msg(message).is_ok_and(|bytes| bytes.len() <= RELAY_SAFE_PLAIN_MSG_BYTES)
    }));
}

#[test]
fn terminal_dimensions_are_bounded_before_grid_allocation() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "bounded-grid".to_string(),
        cols: 4_096,
        rows: 65,
        patch_retention: 8,
    });

    let snapshot = core.snapshot();

    assert!(usize::from(snapshot.cols) * usize::from(snapshot.rows) <= MAX_TERMINAL_CELLS);
}

#[test]
fn terminal_title_is_truncated_on_utf8_boundary() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "bounded-title".to_string(),
        cols: 8,
        rows: 2,
        patch_retention: 8,
    });
    let mut title = String::from("\x1b]2;");
    title.push_str(&"界".repeat(2_000));
    title.push('\u{7}');

    core.feed_vt_bytes(title.as_bytes());
    let snapshot = core.snapshot();

    assert!(snapshot.title.len() <= MAX_TITLE_BYTES);
    assert!(snapshot.title.is_char_boundary(snapshot.title.len()));
}

#[test]
fn terminal_cell_text_is_bounded_on_utf8_boundary() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "bounded-cell".to_string(),
        cols: 2,
        rows: 1,
        patch_retention: 8,
    });
    let contents = format!("a{}", "\u{301}".repeat(100));

    core.feed_vt_bytes(contents.as_bytes());
    let snapshot = core.snapshot();
    let text = &snapshot.screen_rows[0].cells[0].cells[0].text;

    assert!(text.len() <= MAX_CELL_TEXT_BYTES);
    assert!(text.is_char_boundary(text.len()));
}

#[test]
fn large_resize_reply_uses_bounded_snapshot_transaction() {
    let mut core = populated_large_screen();

    let messages = core
        .resize_messages(
            ResizeEventV2 {
                resize_seq: 7,
                cols: u16::MAX,
                rows: u16::MAX,
                input_stream_id: "input-stream".to_string(),
                last_input_ack: 0,
            },
            false,
        )
        .expect("bounded resize transaction");

    assert!(matches!(messages.first(), Some(PlainMsg::ResizeAckV2(ack)) if ack.resize_seq == 7));
    assert!(matches!(
        messages.get(1),
        Some(PlainMsg::TerminalSnapshotV2(snapshot))
            if (snapshot.cols, snapshot.rows) == (4_096, 64)
    ));
    assert!(messages[1..].iter().all(|message| {
        encode_plain_msg(message).is_ok_and(|bytes| bytes.len() <= RELAY_SAFE_PLAIN_MSG_BYTES)
    }));
}

#[test]
fn snapshot_transaction_fits_final_outer_wire_with_maximum_room_id() {
    let mut core = populated_large_screen();
    let messages = core
        .snapshot_messages(false)
        .expect("large snapshot transaction");
    let mut secure = SecureSession::new("r".repeat(256), test_keys());

    for (index, message) in messages.into_iter().enumerate() {
        let wire = secure
            .encode_wire(Direction::CliToApp, message, MAX_OUTER_FRAME_BYTES)
            .unwrap_or_else(|error| panic!("message {index} did not fit final wire: {error}"));
        assert!(wire.len() <= MAX_OUTER_FRAME_BYTES);
    }
}

#[test]
fn pathological_screen_transcript_pages_reassemble_the_complete_frame() {
    let cols = 4_096_u16;
    let rows = 64_u16;
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "pathological-transcript".to_string(),
        cols,
        rows,
        patch_retention: 128,
    });
    core.feed_vt_bytes_transport_batch(b"\x1b[?1049h")
        .expect("enter alternate screen");

    let mut input = String::new();
    for row in 1..=rows {
        write!(input, "\x1b[{row};1H").expect("write cursor position");
        for col in 0..cols {
            input.push_str(if col % 2 == 0 {
                "\x1b[31mX"
            } else {
                "\x1b[32mX"
            });
        }
    }
    core.feed_vt_bytes_transport_batch(input.as_bytes())
        .expect("record pathological alternate-screen frame");
    let expected_rows = core.snapshot().screen_rows;
    let mut before_entry_id = None;
    let mut fragments_newest_first = Vec::new();
    loop {
        let chunk = core.transcript_chunk(&RequestTranscriptV2 {
            terminal_run_id: "pathological-transcript".to_string(),
            before_entry_id,
            max_entries: 1,
        });
        assert_eq!(chunk.entries.len(), 1);
        let encoded = encode_plain_msg(&PlainMsg::TranscriptChunkV2(chunk.clone()))
            .expect("encode transcript chunk");
        assert!(
            encoded.len() <= RELAY_SAFE_PLAIN_MSG_BYTES,
            "transcript page encoded to {} bytes",
            encoded.len()
        );

        let has_more = chunk.has_more;
        let next_before_entry_id = chunk.entries.first().map(|entry| entry.entry_id);
        if let (Some(previous), Some(next)) = (before_entry_id, next_before_entry_id) {
            assert!(
                next < previous,
                "transcript paging cursor must move backward"
            );
        }
        before_entry_id = next_before_entry_id;
        fragments_newest_first.extend(chunk.entries);
        assert!(
            fragments_newest_first.len() <= usize::from(rows),
            "transcript paging did not terminate within one page per row"
        );
        if !has_more {
            break;
        }
    }

    fragments_newest_first.reverse();
    assert!(
        fragments_newest_first.len() > 1,
        "pathological screen frame must span transcript pages"
    );
    let fragment_count =
        u32::try_from(fragments_newest_first.len()).expect("fragment count fits protocol");
    let frame_id = fragments_newest_first[0]
        .frame_fragment
        .as_ref()
        .expect("first fragment metadata")
        .frame_id;
    for (fragment_index, entry) in fragments_newest_first.iter().enumerate() {
        let fragment = entry
            .frame_fragment
            .as_ref()
            .expect("every oversized-frame entry has fragment metadata");
        assert_eq!(entry.entry_id, frame_id + fragment_index as u64);
        assert_eq!(fragment.frame_id, frame_id);
        assert_eq!(fragment.fragment_index, fragment_index as u32);
        assert_eq!(fragment.fragment_count, fragment_count);
    }
    let reconstructed_rows = fragments_newest_first
        .iter()
        .flat_map(|entry| entry.rows.iter().cloned())
        .collect::<Vec<_>>();
    assert_eq!(reconstructed_rows, expected_rows);
}
