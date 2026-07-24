use relaycat_protocol::{
    GitSummary, PlainMsg, WorkspaceEvent, WorkspaceEventEnvelope, WorkspaceRequest,
    WorkspaceRequestEnvelope, WorkspaceResponse, WorkspaceResponseEnvelope, encode_plain_msg,
};

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

fn print_fixture(name: &str, message: PlainMsg) {
    let encoded = encode_plain_msg(&message).expect("encode fixture");
    println!("{name}={}", hex(&encoded));
}

fn main() {
    print_fixture(
        "workspace_request.msgpack.hex",
        PlainMsg::WorkspaceRequest(WorkspaceRequestEnvelope {
            request_id: "req-1".to_string(),
            project_id: "project-1".to_string(),
            deadline_unix_ms: 1_900_000_000_000,
            idempotency_key: None,
            operation: WorkspaceRequest::ListDirectory {
                path: "src".to_string(),
                offset: 0,
                limit: 100,
            },
        }),
    );
    print_fixture(
        "workspace_response.msgpack.hex",
        PlainMsg::WorkspaceResponse(WorkspaceResponseEnvelope {
            request_id: "req-1".to_string(),
            project_id: "project-1".to_string(),
            result: Ok(WorkspaceResponse::GitSummary(GitSummary {
                branch: "main".to_string(),
                upstream: Some("origin/main".to_string()),
                ahead: 2,
                behind: 1,
                staged_count: 1,
                unstaged_count: 2,
                untracked_count: 3,
                fingerprint: "abc123".to_string(),
            })),
        }),
    );
    print_fixture(
        "workspace_event.msgpack.hex",
        PlainMsg::WorkspaceEvent(WorkspaceEventEnvelope {
            project_id: "project-1".to_string(),
            event_seq: 8,
            event: WorkspaceEvent::ShellOutput {
                shell_id: "shell-1".to_string(),
                output_seq: 7,
                bytes: vec![0, 27, 91, 65, 255],
            },
        }),
    );
}
