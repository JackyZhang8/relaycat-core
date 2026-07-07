use relaycat_protocol::{
    CellAttr, CellRun, CliMetadata, CliStatus, CursorState, CursorStyle, Direction, HelloAckV2,
    HelloV2, InputAckV2, InputDecision, InputDedupe, InputEventV2, OuterFrame, PaletteState,
    PatchOp, PatchRejectReason, PlainMsg, ProtocolCapabilityV2, RenderAckV2, RequestSnapshotV2,
    RequestTranscriptV2, ResizeAckV2, ResizeEventV2, ResumeV2, Role, SnapshotRequestReason,
    TerminalCell, TerminalColor, TerminalModes, TerminalPatchV2, TerminalRow, TerminalSnapshotV2,
    TerminalTranscriptEntryKind, TerminalTranscriptEntryV2, TranscriptChunkV2, decode_frame,
    decode_plain_msg, encode_frame, encode_plain_msg, plain_msg_encoded_len, plain_msg_type,
    plain_msg_types, terminal_patch_v2_encoded_len, terminal_snapshot_v2_encoded_len,
};

#[test]
fn outer_data_frame_round_trips_through_messagepack() {
    let frame = OuterFrame::Data {
        room_id: "room-1".to_string(),
        direction: Direction::CliToApp,
        seq: 42,
        nonce: [7; 12],
        ciphertext: vec![1, 2, 3, 4],
    };

    let encoded = encode_frame(&frame).expect("encode frame");
    let decoded = decode_frame(&encoded).expect("decode frame");

    assert_eq!(decoded, frame);
}

#[test]
fn join_frame_carries_optional_pairing_proof() {
    let frame = OuterFrame::Join {
        room_id: "room-1".to_string(),
        role: Role::App,
        device_pubkey: [3; 32],
        pairing_token_proof: Some([9; 32]),
        relay_admission: None,

        connection_salt: None,
    };

    let encoded = encode_frame(&frame).expect("encode frame");
    let decoded = decode_frame(&encoded).expect("decode frame");

    assert_eq!(decoded, frame);
}

#[test]
fn cli_status_round_trips() {
    let msg = PlainMsg::CliStatus(CliStatus {
        cpu_percent_x10: 125,
        memory_bytes: 4096,
        rx_bytes_per_sec: 0,
        tx_bytes_per_sec: 0,
        process_name: "codex".to_string(),
        collected_at_unix_ms: 123456,
    });

    assert_eq!(
        decode_plain_msg(&encode_plain_msg(&msg).unwrap()).unwrap(),
        msg
    );
}

#[test]
fn cli_metadata_round_trips() {
    let msg = PlainMsg::CliMetadata(CliMetadata {
        project_path: "/Users/apple/rustdev/relaycat".to_string(),
    });

    assert_eq!(
        decode_plain_msg(&encode_plain_msg(&msg).unwrap()).unwrap(),
        msg
    );
    assert_eq!(plain_msg_type(&msg), b"cli_metadata");
    assert!(plain_msg_types().contains(&b"cli_metadata".as_slice()));
}

#[test]
fn hello_v2_round_trips_with_capabilities() {
    let msg = PlainMsg::HelloV2(HelloV2 {
        protocol_versions: vec![2],
        capabilities: vec![
            ProtocolCapabilityV2::TerminalState,
            ProtocolCapabilityV2::SnapshotRecovery,
            ProtocolCapabilityV2::ExactlyOnceInput,
        ],
    });

    assert_eq!(
        decode_plain_msg(&encode_plain_msg(&msg).unwrap()).unwrap(),
        msg
    );
    assert_eq!(plain_msg_type(&msg), b"hello_v2");
}

#[test]
fn hello_ack_v2_round_trips_with_selected_version() {
    let msg = PlainMsg::HelloAckV2(HelloAckV2 {
        selected_protocol_version: 2,
        capabilities: vec![
            ProtocolCapabilityV2::TerminalState,
            ProtocolCapabilityV2::SnapshotRecovery,
        ],
    });

    assert_eq!(
        decode_plain_msg(&encode_plain_msg(&msg).unwrap()).unwrap(),
        msg
    );
    assert_eq!(plain_msg_type(&msg), b"hello_ack_v2");
}

#[test]
fn resume_v2_round_trips_with_optional_terminal_state() {
    let msg = PlainMsg::ResumeV2(ResumeV2 {
        terminal_run_id: Some("run-1".to_string()),
        last_applied_state_seq: 55,
        last_snapshot_id: Some(8),
        input_stream_id: "stream-1".to_string(),
        last_input_ack: 21,
    });

    assert_eq!(
        decode_plain_msg(&encode_plain_msg(&msg).unwrap()).unwrap(),
        msg
    );
    assert_eq!(plain_msg_type(&msg), b"resume_v2");
}

#[test]
fn terminal_snapshot_v2_round_trips() {
    let msg = PlainMsg::TerminalSnapshotV2(TerminalSnapshotV2 {
        terminal_run_id: "run-1".to_string(),
        snapshot_id: 9,
        state_seq: 44,
        cols: 80,
        rows: 24,
        title: "codex".to_string(),
        cursor: CursorState {
            row: 3,
            col: 4,
            visible: true,
            style: CursorStyle::Block,
        },
        modes: TerminalModes {
            alt_screen: false,
            bracketed_paste: true,
            application_cursor: false,
        },
        palette: PaletteState {
            default_fg: TerminalColor::Rgb {
                r: 238,
                g: 238,
                b: 238,
            },
            default_bg: TerminalColor::Rgb {
                r: 17,
                g: 17,
                b: 17,
            },
            cursor: TerminalColor::Indexed(15),
            ansi: vec![TerminalColor::Indexed(0), TerminalColor::Indexed(1)],
        },
        attrs: vec![CellAttr::default()],
        reset_app_cache: true,
        scrollback_window: vec![TerminalRow {
            line_id: 1,
            wrapped: false,
            cells: vec![CellRun {
                attr_id: 0,
                cells: vec![TerminalCell {
                    text: "h".to_string(),
                    width: 1,
                }],
            }],
        }],
        screen_rows: vec![],
    });

    assert_eq!(
        decode_plain_msg(&encode_plain_msg(&msg).unwrap()).unwrap(),
        msg
    );
}

#[test]
fn terminal_patch_v2_rejects_non_contiguous_sequence() {
    let patch = TerminalPatchV2 {
        terminal_run_id: "run-1".to_string(),
        base_snapshot_id: 7,
        from_state_seq: 12,
        to_state_seq: 13,
        attrs: vec![CellAttr::default()],
        attrs_base_len: None,
        ops: vec![PatchOp::SetTitle("ready".to_string())],
    };

    assert_eq!(
        patch.validate_against(7, 10),
        Err(PatchRejectReason::SequenceGap)
    );
}

#[test]
fn terminal_patch_v2_accepts_matching_base_and_next_sequence() {
    let patch = TerminalPatchV2 {
        terminal_run_id: "run-1".to_string(),
        base_snapshot_id: 7,
        from_state_seq: 11,
        to_state_seq: 12,
        attrs: vec![CellAttr::default()],
        attrs_base_len: None,
        ops: vec![PatchOp::Bell],
    };

    assert_eq!(patch.validate_against(7, 10), Ok(()));
}

#[test]
fn terminal_patch_v2_carries_attr_table_for_cell_attr_ids() {
    let red = CellAttr {
        fg: TerminalColor::Indexed(1),
        ..CellAttr::default()
    };
    let msg = PlainMsg::TerminalPatchV2(TerminalPatchV2 {
        terminal_run_id: "run-1".to_string(),
        base_snapshot_id: 7,
        from_state_seq: 11,
        to_state_seq: 11,
        attrs: vec![CellAttr::default(), red],
        attrs_base_len: None,
        ops: vec![PatchOp::PutCells {
            row: 0,
            col: 0,
            cells: vec![CellRun {
                attr_id: 1,
                cells: vec![TerminalCell {
                    text: "R".to_string(),
                    width: 1,
                }],
            }],
        }],
    });

    assert_eq!(
        decode_plain_msg(&encode_plain_msg(&msg).unwrap()).unwrap(),
        msg
    );
}

#[test]
fn terminal_patch_v2_round_trips_incremental_attr_tail() {
    let red = CellAttr {
        fg: TerminalColor::Indexed(1),
        ..CellAttr::default()
    };
    let msg = PlainMsg::TerminalPatchV2(TerminalPatchV2 {
        terminal_run_id: "run-1".to_string(),
        base_snapshot_id: 7,
        from_state_seq: 11,
        to_state_seq: 11,
        // Only the appended tail, keyed by the base index it is appended after.
        attrs: vec![red],
        attrs_base_len: Some(1),
        ops: vec![PatchOp::Bell],
    });

    let decoded = decode_plain_msg(&encode_plain_msg(&msg).unwrap()).unwrap();
    assert_eq!(decoded, msg);
    let PlainMsg::TerminalPatchV2(patch) = decoded else {
        panic!("expected terminal patch");
    };
    assert_eq!(patch.attrs_base_len, Some(1));
}

#[test]
fn terminal_patch_v2_omits_attrs_base_len_for_legacy_table() {
    // A legacy (whole-table) patch must not emit the `attrs_base_len` key so the
    // wire stays byte-identical for peers that never negotiated incremental
    // attrs.
    let legacy = TerminalPatchV2 {
        terminal_run_id: "run-1".to_string(),
        base_snapshot_id: 7,
        from_state_seq: 11,
        to_state_seq: 11,
        attrs: vec![CellAttr::default()],
        attrs_base_len: None,
        ops: vec![PatchOp::Bell],
    };
    let encoded = encode_plain_msg(&PlainMsg::TerminalPatchV2(legacy)).unwrap();
    let needle = b"attrs_base_len";
    assert!(
        !encoded.windows(needle.len()).any(|w| w == needle),
        "legacy patch should not serialize attrs_base_len"
    );
}

#[test]
fn terminal_patch_v2_rejects_missing_base_snapshot() {
    let patch = TerminalPatchV2 {
        terminal_run_id: "run-1".to_string(),
        base_snapshot_id: 8,
        from_state_seq: 11,
        to_state_seq: 12,
        attrs: vec![CellAttr::default()],
        attrs_base_len: None,
        ops: vec![PatchOp::Bell],
    };

    assert_eq!(
        patch.validate_against(7, 10),
        Err(PatchRejectReason::MissingBase)
    );
}

#[test]
fn terminal_patch_v2_rejects_empty_patch_range() {
    let patch = TerminalPatchV2 {
        terminal_run_id: "run-1".to_string(),
        base_snapshot_id: 7,
        from_state_seq: 11,
        to_state_seq: 10,
        attrs: vec![CellAttr::default()],
        attrs_base_len: None,
        ops: vec![PatchOp::Bell],
    };

    assert_eq!(
        patch.validate_against(7, 10),
        Err(PatchRejectReason::EmptyPatchRange)
    );
}

#[test]
fn v2_control_messages_round_trip() {
    let messages = vec![
        PlainMsg::RenderAckV2(RenderAckV2 {
            terminal_run_id: "run-1".to_string(),
            snapshot_id: 8,
            applied_state_seq: 55,
        }),
        PlainMsg::RequestSnapshotV2(RequestSnapshotV2 {
            terminal_run_id: Some("run-1".to_string()),
            reason: SnapshotRequestReason::SeqGap,
            cols: 100,
            rows: 30,
        }),
        PlainMsg::InputEventV2(InputEventV2 {
            input_stream_id: "stream-1".to_string(),
            input_seq: 9,
            bytes: b"hello".to_vec(),
        }),
        PlainMsg::InputAckV2(InputAckV2 {
            highest_contiguous_input_seq: 9,
        }),
        PlainMsg::ResizeEventV2(ResizeEventV2 {
            resize_seq: 4,
            cols: 120,
            rows: 40,
            input_stream_id: "stream-1".to_string(),
            last_input_ack: 9,
        }),
        PlainMsg::ResizeAckV2(ResizeAckV2 { resize_seq: 4 }),
    ];

    for msg in messages {
        assert_eq!(
            decode_plain_msg(&encode_plain_msg(&msg).unwrap()).unwrap(),
            msg
        );
    }
}

#[test]
fn transcript_v2_messages_round_trip() {
    let request = PlainMsg::RequestTranscriptV2(RequestTranscriptV2 {
        terminal_run_id: "run-1".to_string(),
        before_entry_id: Some(42),
        max_entries: 10,
    });
    let chunk = PlainMsg::TranscriptChunkV2(TranscriptChunkV2 {
        terminal_run_id: "run-1".to_string(),
        before_entry_id: Some(42),
        entries: vec![TerminalTranscriptEntryV2 {
            entry_id: 41,
            terminal_run_id: "run-1".to_string(),
            state_seq: 9,
            kind: TerminalTranscriptEntryKind::AltScreenFrame,
            cols: 4,
            rows: vec![TerminalRow {
                line_id: 1,
                wrapped: false,
                cells: vec![CellRun {
                    attr_id: 0,
                    cells: vec![TerminalCell {
                        text: "A".to_string(),
                        width: 1,
                    }],
                }],
            }],
            captured_at_unix_ms: 1234,
        }],
        attrs: vec![CellAttr::default()],
        has_more: false,
    });

    for msg in [request, chunk] {
        assert_eq!(
            decode_plain_msg(&encode_plain_msg(&msg).unwrap()).unwrap(),
            msg
        );
    }
}

#[test]
fn plain_msg_types_lists_v2_handshake_and_state_labels() {
    let labels = plain_msg_types();

    for expected in [
        b"hello_v2".as_slice(),
        b"hello_ack_v2".as_slice(),
        b"resume_v2".as_slice(),
        b"terminal_snapshot_v2".as_slice(),
        b"terminal_patch_v2".as_slice(),
        b"render_ack_v2".as_slice(),
        b"request_snapshot_v2".as_slice(),
        b"request_transcript_v2".as_slice(),
        b"transcript_chunk_v2".as_slice(),
        b"input_event_v2".as_slice(),
        b"input_ack_v2".as_slice(),
        b"resize_event_v2".as_slice(),
        b"resize_ack_v2".as_slice(),
    ] {
        assert!(labels.contains(&expected));
    }
}

#[test]
fn encoded_len_helpers_match_full_encoding() {
    // The no-allocation length helpers must agree byte-for-byte with the real
    // encode_plain_msg, including the externally-tagged enum wrapper and the
    // skip_serializing_if on attrs_base_len.
    for attrs_base_len in [None, Some(3)] {
        let patch = TerminalPatchV2 {
            terminal_run_id: "run-1".to_string(),
            base_snapshot_id: 7,
            from_state_seq: 11,
            to_state_seq: 12,
            attrs: vec![CellAttr::default()],
            attrs_base_len,
            ops: vec![PatchOp::SetTitle("ready".to_string()), PatchOp::Bell],
        };
        let expected = encode_plain_msg(&PlainMsg::TerminalPatchV2(patch.clone()))
            .unwrap()
            .len();
        assert_eq!(terminal_patch_v2_encoded_len(&patch), expected);
        assert_eq!(
            plain_msg_encoded_len(&PlainMsg::TerminalPatchV2(patch.clone())),
            expected
        );
    }

    let snapshot = TerminalSnapshotV2 {
        terminal_run_id: "run-1".to_string(),
        snapshot_id: 9,
        state_seq: 44,
        cols: 80,
        rows: 24,
        title: "codex".to_string(),
        cursor: CursorState {
            row: 3,
            col: 4,
            visible: true,
            style: CursorStyle::Block,
        },
        modes: TerminalModes {
            alt_screen: false,
            bracketed_paste: true,
            application_cursor: false,
        },
        palette: PaletteState {
            default_fg: TerminalColor::Indexed(7),
            default_bg: TerminalColor::Indexed(0),
            cursor: TerminalColor::Indexed(15),
            ansi: vec![TerminalColor::Indexed(0), TerminalColor::Indexed(1)],
        },
        attrs: vec![CellAttr::default()],
        reset_app_cache: true,
        scrollback_window: vec![TerminalRow {
            line_id: 1,
            wrapped: false,
            cells: vec![CellRun {
                attr_id: 0,
                cells: vec![TerminalCell {
                    text: "h".to_string(),
                    width: 1,
                }],
            }],
        }],
        screen_rows: vec![],
    };
    let expected = encode_plain_msg(&PlainMsg::TerminalSnapshotV2(snapshot.clone()))
        .unwrap()
        .len();
    assert_eq!(terminal_snapshot_v2_encoded_len(&snapshot), expected);
    assert_eq!(
        plain_msg_encoded_len(&PlainMsg::TerminalSnapshotV2(snapshot)),
        expected
    );
}

#[test]
fn plain_msg_types_are_unique_and_complete() {
    // Reordering the labels for decrypt-attempt performance must never drop or
    // duplicate one: a missing label means that message type can never be
    // decoded, a duplicate just wastes a decrypt attempt.
    let labels = plain_msg_types();
    let mut sorted: Vec<&[u8]> = labels.to_vec();
    sorted.sort_unstable();
    let unique_len = sorted.len();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        unique_len,
        "duplicate label in plain_msg_types"
    );
    assert_eq!(
        labels.len(),
        17,
        "label count drifted from PlainMsg variants"
    );
}

#[test]
fn plain_msg_types_do_not_expose_legacy_terminal_labels() {
    let labels = plain_msg_types();

    for removed in [
        [b"terminal_".as_slice(), b"output".as_slice()].concat(),
        [b"terminal_".as_slice(), b"replay".as_slice()].concat(),
        [b"terminal_".as_slice(), b"input".as_slice()].concat(),
        [b"resize".as_slice()].concat(),
        [b"request_".as_slice(), b"replay".as_slice()].concat(),
        [b"request_".as_slice(), b"history_v2".as_slice()].concat(),
        [b"history_".as_slice(), b"chunk_v2".as_slice()].concat(),
    ] {
        assert!(
            !labels.contains(&removed.as_slice()),
            "unexpected legacy label {removed:?}"
        );
    }
}

#[test]
fn input_dedupe_accepts_next_sequence_once() {
    let mut dedupe = InputDedupe::default();

    assert_eq!(dedupe.observe("stream-1", 1), InputDecision::Accept);
    assert_eq!(dedupe.observe("stream-1", 1), InputDecision::Duplicate);
    assert_eq!(dedupe.highest_contiguous_input_seq(), 1);
}

#[test]
fn input_dedupe_accepts_gap_and_fast_forwards_watermark() {
    let mut dedupe = InputDedupe::default();

    assert_eq!(dedupe.observe("stream-1", 2), InputDecision::Gap);
    assert_eq!(dedupe.highest_contiguous_input_seq(), 2);
    assert_eq!(dedupe.observe("stream-1", 1), InputDecision::Duplicate);
    assert_eq!(dedupe.observe("stream-1", 3), InputDecision::Accept);
    assert_eq!(dedupe.highest_contiguous_input_seq(), 3);
}

#[test]
fn input_dedupe_can_restore_from_app_ack_watermark_before_next_input() {
    let mut dedupe = InputDedupe::default();

    dedupe.synchronize_ack("stream-1", 4);

    assert_eq!(dedupe.highest_contiguous_input_seq(), 4);
    assert_eq!(dedupe.observe("stream-1", 5), InputDecision::Accept);
    assert_eq!(dedupe.highest_contiguous_input_seq(), 5);
}

#[test]
fn input_dedupe_resets_when_input_stream_changes() {
    let mut dedupe = InputDedupe::default();

    dedupe.synchronize_ack("old-app-process", 8);

    assert_eq!(dedupe.observe("new-app-process", 1), InputDecision::Accept);
    assert_eq!(dedupe.highest_contiguous_input_seq(), 1);
}
