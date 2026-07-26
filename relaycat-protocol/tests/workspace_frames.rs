use relaycat_protocol::{
    ArchiveEntry, CliMetadata, FilePreview, PlainMsg, WorkspaceEvent, WorkspaceEventEnvelope,
    WorkspaceRequest, WorkspaceRequestEnvelope, WorkspaceResponse, WorkspaceResponseEnvelope,
    decode_plain_msg, encode_plain_msg, plain_msg_type, plain_msg_types,
};

#[test]
fn workspace_request_round_trips() {
    let msg = PlainMsg::WorkspaceRequest(WorkspaceRequestEnvelope {
        request_id: "req-1".to_string(),
        project_id: "project-1".to_string(),
        deadline_unix_ms: 1_900_000_000_000,
        idempotency_key: None,
        operation: WorkspaceRequest::ListDirectory {
            path: "src".to_string(),
            offset: 0,
            limit: 100,
        },
    });

    let encoded = encode_plain_msg(&msg).expect("encode workspace request");
    assert_eq!(decode_plain_msg(&encoded).expect("decode workspace request"), msg);
    assert_eq!(plain_msg_type(&msg), b"workspace_request");
    assert!(plain_msg_types().contains(&b"workspace_response".as_slice()));
    assert!(plain_msg_types().contains(&b"workspace_event".as_slice()));
}

#[test]
fn archive_file_preview_round_trips_with_partial_listing() {
    let msg = PlainMsg::WorkspaceResponse(WorkspaceResponseEnvelope {
        request_id: "req-archive".to_string(),
        project_id: "project-1".to_string(),
        result: Ok(WorkspaceResponse::File(FilePreview::Archive {
            path: "release.zip".to_string(),
            format: "zip".to_string(),
            entries: vec![
                ArchiveEntry {
                    path: "dist/".to_string(),
                    is_directory: true,
                    size: 0,
                    modified_unix_seconds: None,
                },
                ArchiveEntry {
                    path: "dist/app.js".to_string(),
                    is_directory: false,
                    size: 12_345,
                    modified_unix_seconds: Some(1_700_000_000),
                },
            ],
            has_more: true,
        })),
    });

    let encoded = encode_plain_msg(&msg).expect("encode archive preview");
    assert_eq!(decode_plain_msg(&encoded).expect("decode archive preview"), msg);
}

#[test]
fn shell_output_matches_checked_in_fixture() {
    let msg = PlainMsg::WorkspaceEvent(WorkspaceEventEnvelope {
        project_id: "project-1".to_string(),
        event_seq: 8,
        event: WorkspaceEvent::ShellOutput {
            shell_id: "shell-1".to_string(),
            output_seq: 7,
            bytes: vec![0, 27, 91, 65, 255],
        },
    });
    let expected = decode_hex(include_str!("fixtures/workspace_event.msgpack.hex"));
    assert_eq!(encode_plain_msg(&msg).expect("encode event"), expected);
    assert_eq!(decode_plain_msg(&expected).expect("decode event"), msg);
}

fn decode_hex(value: &str) -> Vec<u8> {
    let digits: Vec<u8> = value.bytes().filter(|byte| !byte.is_ascii_whitespace()).collect();
    assert_eq!(digits.len() % 2, 0, "fixture hex must have complete bytes");
    digits
        .chunks_exact(2)
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16).expect("hex digit");
            let low = (pair[1] as char).to_digit(16).expect("hex digit");
            ((high << 4) | low) as u8
        })
        .collect()
}

#[test]
fn cli_metadata_project_id_is_optional_for_legacy_frames() {
    let bytes = rmp_serde::to_vec_named(&PlainMsgFixture::CliMetadata(LegacyCliMetadata {
        project_path: "/tmp/project".to_string(),
    }))
    .expect("encode legacy metadata");

    assert_eq!(
        decode_plain_msg(&bytes).expect("decode legacy metadata"),
        PlainMsg::CliMetadata(CliMetadata {
            project_path: "/tmp/project".to_string(),
            project_id: None,
        })
    );
}

#[derive(serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum PlainMsgFixture {
    CliMetadata(LegacyCliMetadata),
}

#[derive(serde::Serialize)]
struct LegacyCliMetadata {
    project_path: String,
}
