use relaycat_protocol::{
    AppJoinIntent, CellAttr, CursorState, CursorStyle, Direction, OuterFrame, PaletteState,
    PatchOp, PlainMsg, ResumeAcceptMode, ResumeAcceptedV2, ResumeV2, Role, TerminalColor,
    TerminalModes, TerminalPatchV2, TerminalSnapshotV2, TerminalTranscriptEntryKind,
    TerminalTranscriptEntryV2, TerminalTranscriptFrameFragmentV2, TranscriptChunkV2, decode_frame,
    decode_plain_msg, encode_frame, encode_plain_msg,
};
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};

#[test]
fn sync_protocol_rejects_duplicate_map_fields() {
    for wire in [
        rmp_serde::to_vec_named(&DuplicateResumeRunId).expect("encode duplicate resume field"),
        rmp_serde::to_vec_named(&DuplicateProcessExitCode).expect("encode duplicate exit code"),
        rmp_serde::to_vec_named(&DuplicateSnapshotStateSeq)
            .expect("encode duplicate snapshot sequence"),
    ] {
        assert!(
            decode_plain_msg(&wire).is_err(),
            "duplicate protocol field must be rejected: {wire:02x?}"
        );
    }

    let join =
        rmp_serde::to_vec_named(&DuplicateJoinIntent).expect("encode duplicate app join intent");
    assert!(decode_frame(&join).is_err());
}

#[test]
fn sync_protocol_rejects_multiple_top_level_message_kinds() {
    let wire = rmp_serde::to_vec_named(&MultiplePlainKinds).expect("encode ambiguous message");
    assert!(decode_plain_msg(&wire).is_err());
}

#[test]
fn invalid_transcript_fragment_coordinates_are_rejected() {
    for (fragment_index, fragment_count) in [(0, 0), (2, 2), (u32::MAX, 2)] {
        let msg = transcript_message(Some(TerminalTranscriptFrameFragmentV2 {
            frame_id: 9,
            fragment_index,
            fragment_count,
        }));
        let wire = encode_plain_msg(&msg).expect("encode invalid fragment fixture");

        assert!(
            decode_plain_msg(&wire).is_err(),
            "fragment index={fragment_index} count={fragment_count} must be rejected"
        );
    }
}

#[test]
fn every_proper_outer_data_prefix_is_rejected() {
    let frame = OuterFrame::Data {
        room_id: "room-truncation".to_string(),
        direction: Direction::CliToApp,
        seq: u64::MAX,
        nonce: [0xa5; 12],
        ciphertext: vec![0xde, 0xad, 0xbe, 0xef, 0, 0xff],
    };
    let wire = encode_frame(&frame).expect("encode outer data");

    assert_every_proper_prefix_fails(&wire, decode_frame);
    assert_eq!(
        decode_frame(&wire).expect("decode complete outer data"),
        frame
    );
}

#[test]
fn every_proper_snapshot_patch_and_transcript_prefix_is_rejected() {
    let messages = [
        PlainMsg::TerminalSnapshotV2(snapshot()),
        PlainMsg::TerminalPatchV2(TerminalPatchV2 {
            terminal_run_id: "run-truncation".to_string(),
            base_snapshot_id: 7,
            from_state_seq: 12,
            to_state_seq: 13,
            attrs: vec![CellAttr::default()],
            attrs_base_len: Some(1),
            ops: vec![PatchOp::SetTitle("truncated-🚀".to_string()), PatchOp::Bell],
        }),
        transcript_message(Some(TerminalTranscriptFrameFragmentV2 {
            frame_id: 9,
            fragment_index: 0,
            fragment_count: 2,
        })),
    ];

    for message in messages {
        let wire = encode_plain_msg(&message).expect("encode plain message");
        assert_every_proper_prefix_fails(&wire, decode_plain_msg);
        assert_eq!(
            decode_plain_msg(&wire).expect("decode complete plain message"),
            message
        );
    }
}

#[test]
fn resume_accepted_modes_and_sequence_boundaries_round_trip() {
    for (mode, target_state_seq) in [
        (ResumeAcceptMode::UpToDate, 0),
        (ResumeAcceptMode::ReplayingPatches, 42),
        (ResumeAcceptMode::SendingSnapshot, u64::MAX),
    ] {
        let message = PlainMsg::ResumeAcceptedV2(ResumeAcceptedV2 {
            mode,
            target_state_seq,
        });
        let wire = encode_plain_msg(&message).expect("encode resume accepted");
        assert_eq!(
            decode_plain_msg(&wire).expect("decode resume accepted"),
            message
        );
    }
}

fn assert_every_proper_prefix_fails<T>(
    wire: &[u8],
    decode: impl Fn(&[u8]) -> relaycat_protocol::Result<T>,
) {
    assert!(!wire.is_empty());
    for length in 0..wire.len() {
        assert!(
            decode(&wire[..length]).is_err(),
            "proper prefix {length}/{} decoded successfully",
            wire.len()
        );
    }
}

fn snapshot() -> TerminalSnapshotV2 {
    TerminalSnapshotV2 {
        terminal_run_id: "run-truncation".to_string(),
        snapshot_id: 7,
        state_seq: 11,
        cols: 4,
        rows: 2,
        title: "truncation".to_string(),
        cursor: CursorState {
            row: 0,
            col: 0,
            visible: true,
            style: CursorStyle::Block,
        },
        modes: TerminalModes {
            alt_screen: false,
            bracketed_paste: true,
            application_cursor: false,
        },
        palette: palette(),
        attrs: vec![CellAttr::default()],
        reset_app_cache: false,
        scrollback_window: vec![],
        screen_rows: vec![],
    }
}

fn palette() -> PaletteState {
    PaletteState {
        default_fg: TerminalColor::Default,
        default_bg: TerminalColor::Default,
        cursor: TerminalColor::Default,
        ansi: vec![],
    }
}

fn transcript_message(frame_fragment: Option<TerminalTranscriptFrameFragmentV2>) -> PlainMsg {
    PlainMsg::TranscriptChunkV2(TranscriptChunkV2 {
        terminal_run_id: "run-truncation".to_string(),
        before_entry_id: Some(42),
        entries: vec![TerminalTranscriptEntryV2 {
            entry_id: 41,
            terminal_run_id: "run-truncation".to_string(),
            state_seq: 13,
            kind: TerminalTranscriptEntryKind::ScreenFrame,
            cols: 4,
            rows: vec![],
            captured_at_unix_ms: u64::MAX,
            frame_fragment,
        }],
        attrs: vec![CellAttr::default()],
        has_more: true,
    })
}

struct DuplicateResumeRunId;

impl Serialize for DuplicateResumeRunId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry("resume_v2", &DuplicateResumeBody)?;
        map.end()
    }
}

struct DuplicateResumeBody;

impl Serialize for DuplicateResumeBody {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(6))?;
        map.serialize_entry("terminal_run_id", "run-a")?;
        map.serialize_entry("terminal_run_id", "run-b")?;
        map.serialize_entry("last_applied_state_seq", &1_u64)?;
        map.serialize_entry("last_snapshot_id", &Some(1_u64))?;
        map.serialize_entry("input_stream_id", "stream-1")?;
        map.serialize_entry("last_input_ack", &0_u64)?;
        map.end()
    }
}

struct DuplicateProcessExitCode;

impl Serialize for DuplicateProcessExitCode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry("process_exit", &DuplicateProcessExitBody)?;
        map.end()
    }
}

struct DuplicateProcessExitBody;

impl Serialize for DuplicateProcessExitBody {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("code", &Option::<i32>::None)?;
        map.serialize_entry("code", &Some(9_i32))?;
        map.end()
    }
}

struct DuplicateSnapshotStateSeq;

impl Serialize for DuplicateSnapshotStateSeq {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry("terminal_snapshot_v2", &DuplicateSnapshotBody)?;
        map.end()
    }
}

struct DuplicateSnapshotBody;

impl Serialize for DuplicateSnapshotBody {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let cursor = CursorState {
            row: 0,
            col: 0,
            visible: true,
            style: CursorStyle::Block,
        };
        let modes = TerminalModes {
            alt_screen: false,
            bracketed_paste: false,
            application_cursor: false,
        };
        let mut map = serializer.serialize_map(Some(14))?;
        map.serialize_entry("terminal_run_id", "run-1")?;
        map.serialize_entry("snapshot_id", &1_u64)?;
        map.serialize_entry("state_seq", &1_u64)?;
        map.serialize_entry("state_seq", &2_u64)?;
        map.serialize_entry("cols", &4_u16)?;
        map.serialize_entry("rows", &2_u16)?;
        map.serialize_entry("title", "duplicate")?;
        map.serialize_entry("cursor", &cursor)?;
        map.serialize_entry("modes", &modes)?;
        map.serialize_entry("palette", &palette())?;
        map.serialize_entry("attrs", &Vec::<CellAttr>::new())?;
        map.serialize_entry("reset_app_cache", &false)?;
        map.serialize_entry(
            "scrollback_window",
            &Vec::<relaycat_protocol::TerminalRow>::new(),
        )?;
        map.serialize_entry("screen_rows", &Vec::<relaycat_protocol::TerminalRow>::new())?;
        map.end()
    }
}

struct DuplicateJoinIntent;

impl Serialize for DuplicateJoinIntent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry("join", &DuplicateJoinBody)?;
        map.end()
    }
}

struct DuplicateJoinBody;

impl Serialize for DuplicateJoinBody {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(9))?;
        map.serialize_entry("room_id", "room-1")?;
        map.serialize_entry("role", &Role::App)?;
        map.serialize_entry("device_pubkey", &[1_u8; 32])?;
        map.serialize_entry("pairing_token_proof", &Option::<[u8; 32]>::None)?;
        map.serialize_entry("relay_admission", &Option::<[u8; 32]>::None)?;
        map.serialize_entry("connection_salt", &Option::<[u8; 32]>::None)?;
        map.serialize_entry("supports_join_accepted", &true)?;
        map.serialize_entry("app_join_intent", &AppJoinIntent::Resume)?;
        map.serialize_entry("app_join_intent", &AppJoinIntent::Takeover)?;
        map.end()
    }
}

struct MultiplePlainKinds;

impl Serialize for MultiplePlainKinds {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let resume = ResumeV2 {
            terminal_run_id: None,
            last_applied_state_seq: 0,
            last_snapshot_id: None,
            input_stream_id: "stream-1".to_string(),
            last_input_ack: 0,
        };
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("resume_v2", &resume)?;
        map.serialize_entry("process_exit", &DuplicateProcessExitBody)?;
        map.end()
    }
}
