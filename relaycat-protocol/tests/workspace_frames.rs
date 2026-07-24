use relaycat_protocol::{
    CliMetadata, PlainMsg, WorkspaceRequest, WorkspaceRequestEnvelope, decode_plain_msg,
    encode_plain_msg, plain_msg_type, plain_msg_types,
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
