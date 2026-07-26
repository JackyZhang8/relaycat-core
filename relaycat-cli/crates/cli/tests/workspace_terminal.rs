use relaycat_cli::workspace_terminal::{
    WORKSPACE_SHELL_STREAM_ID, WorkspaceTerminalAction, WorkspaceTerminalHost,
};
use relaycat_protocol::{
    RenderAckV2, ResizeEventV2, ResumeAcceptMode, ResumeV2, TerminalStreamMessageV2,
    TerminalStreamV2,
};

#[test]
fn workspace_terminal_stream_emits_snapshot_then_semantic_output() {
    let mut host = WorkspaceTerminalHost::new(80, 24, 128);

    let initial = host.initial_messages().expect("initial snapshot");
    assert_eq!(initial.len(), 1);
    assert_eq!(initial[0].stream_id, WORKSPACE_SHELL_STREAM_ID);
    assert!(matches!(
        initial[0].message,
        TerminalStreamMessageV2::Snapshot(_)
    ));

    let output = host
        .feed_output(b"workspace-marker\r\n")
        .expect("terminal output");
    assert!(!output.is_empty());
    assert!(
        output
            .iter()
            .all(|message| message.stream_id == WORKSPACE_SHELL_STREAM_ID)
    );
    assert!(
        output
            .iter()
            .any(|message| matches!(message.message, TerminalStreamMessageV2::Patch(_)))
    );
}

#[test]
fn workspace_terminal_stream_writes_contiguous_input_once() {
    let mut host = WorkspaceTerminalHost::new(80, 24, 128);
    let input = TerminalStreamV2 {
        stream_id: WORKSPACE_SHELL_STREAM_ID.to_string(),
        message: TerminalStreamMessageV2::Input {
            input_stream_id: "workspace-input-1".to_string(),
            input_seq: 1,
            bytes: b"top\r".to_vec(),
        },
    };

    let first = host.handle_message(input.clone()).expect("first input");
    assert_eq!(
        first.actions,
        vec![WorkspaceTerminalAction::Write(b"top\r".to_vec())]
    );
    assert_eq!(first.outbound.len(), 1);
    assert!(matches!(
        first.outbound[0].message,
        TerminalStreamMessageV2::InputAck(_)
    ));

    let duplicate = host.handle_message(input).expect("duplicate input");
    assert!(duplicate.actions.is_empty());
    assert_eq!(duplicate.outbound.len(), 1);
}

#[test]
fn workspace_terminal_resize_updates_semantic_dimensions_and_requests_pty_resize() {
    let mut host = WorkspaceTerminalHost::new(80, 24, 128);
    let _ = host.initial_messages().unwrap();
    let result = host
        .handle_message(TerminalStreamV2 {
            stream_id: WORKSPACE_SHELL_STREAM_ID.to_string(),
            message: TerminalStreamMessageV2::Resize(ResizeEventV2 {
                resize_seq: 3,
                cols: 100,
                rows: 32,
                input_stream_id: "workspace-input-1".to_string(),
                last_input_ack: 0,
            }),
        })
        .unwrap();

    assert_eq!(
        result.actions,
        vec![WorkspaceTerminalAction::Resize { cols: 100, rows: 32 }]
    );
    assert!(result.outbound.iter().any(|message| matches!(
        &message.message,
        TerminalStreamMessageV2::Snapshot(snapshot)
            if (snapshot.cols, snapshot.rows) == (100, 32)
    )));
}

#[test]
fn workspace_terminal_render_ack_releases_patch_and_resume_falls_back_to_snapshot() {
    let mut host = WorkspaceTerminalHost::new(80, 24, 128);
    let initial = host.initial_messages().unwrap();
    let snapshot = match &initial[0].message {
        TerminalStreamMessageV2::Snapshot(snapshot) => snapshot.clone(),
        other => panic!("unexpected initial message: {other:?}"),
    };
    let patches = host.feed_output(b"ack-marker\r\n").unwrap();
    let patch = patches
        .iter()
        .find_map(|message| match &message.message {
            TerminalStreamMessageV2::Patch(patch) => Some(patch.clone()),
            _ => None,
        })
        .expect("semantic patch");
    host.handle_message(TerminalStreamV2 {
        stream_id: WORKSPACE_SHELL_STREAM_ID.to_string(),
        message: TerminalStreamMessageV2::RenderAck(RenderAckV2 {
            terminal_run_id: patch.terminal_run_id.clone(),
            snapshot_id: patch.base_snapshot_id,
            applied_state_seq: patch.to_state_seq,
        }),
    })
    .unwrap();

    let resumed = host
        .handle_message(TerminalStreamV2 {
            stream_id: WORKSPACE_SHELL_STREAM_ID.to_string(),
            message: TerminalStreamMessageV2::Resume(ResumeV2 {
                terminal_run_id: Some(snapshot.terminal_run_id),
                last_applied_state_seq: 0,
                last_snapshot_id: Some(snapshot.snapshot_id),
                input_stream_id: "workspace-input-1".to_string(),
                last_input_ack: 0,
            }),
        })
        .unwrap();
    assert!(matches!(
        resumed.outbound.first().map(|message| &message.message),
        Some(TerminalStreamMessageV2::ResumeAccepted(accepted))
            if accepted.mode == ResumeAcceptMode::SendingSnapshot
    ));
    assert!(resumed.outbound.iter().any(|message| matches!(
        message.message,
        TerminalStreamMessageV2::Snapshot(_)
    )));
}

#[test]
fn workspace_terminal_resume_replays_retained_patch_without_touching_main_terminal() {
    let mut host = WorkspaceTerminalHost::new(80, 24, 128);
    let initial = host.initial_messages().unwrap();
    let snapshot = match &initial[0].message {
        TerminalStreamMessageV2::Snapshot(snapshot) => snapshot.clone(),
        other => panic!("unexpected initial message: {other:?}"),
    };
    let _ = host.feed_output(b"resume-marker\r\n").unwrap();

    let resumed = host
        .handle_message(TerminalStreamV2 {
            stream_id: WORKSPACE_SHELL_STREAM_ID.to_string(),
            message: TerminalStreamMessageV2::Resume(ResumeV2 {
                terminal_run_id: Some(snapshot.terminal_run_id),
                last_applied_state_seq: snapshot.state_seq,
                last_snapshot_id: Some(snapshot.snapshot_id),
                input_stream_id: "workspace-input-1".to_string(),
                last_input_ack: 0,
            }),
        })
        .unwrap();

    assert!(matches!(
        resumed.outbound.first().map(|message| &message.message),
        Some(TerminalStreamMessageV2::ResumeAccepted(accepted))
            if accepted.mode == ResumeAcceptMode::ReplayingPatches
    ));
    assert!(resumed.outbound.iter().any(|message| matches!(
        message.message,
        TerminalStreamMessageV2::Patch(_)
    )));
}
