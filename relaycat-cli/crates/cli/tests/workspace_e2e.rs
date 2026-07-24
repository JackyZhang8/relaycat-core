use relaycat_cli::secure::{SecureSession, decode_secure_data};
use relaycat_crypto::{KeyPair, SessionKeys};
use relaycat_protocol::{
    CursorState, CursorStyle, Direction, GitDiffTarget, MAX_OUTER_FRAME_BYTES, PatchOp, PlainMsg, TerminalPatchV2,
    WorkspaceRequest, WorkspaceRequestEnvelope,
};
use relaycat_workspace::{ProjectRoot, WorkspaceService};
use std::{fs, path::PathBuf, process::Command, sync::atomic::{AtomicU64, Ordering}};

static NEXT_REPO: AtomicU64 = AtomicU64::new(1);

struct Repo { root: PathBuf }
impl Repo {
    fn new() -> Self {
        let id = NEXT_REPO.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("relaycat-workspace-e2e-{}-{id}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "--quiet"]);
        git(&root, &["config", "user.name", "RelayCat Test"]);
        git(&root, &["config", "user.email", "relaycat@example.test"]);
        fs::write(root.join("large.txt"), patterned_text(700 * 1024, b'a')).unwrap();
        git(&root, &["add", "--", "large.txt"]);
        git(&root, &["commit", "--quiet", "-m", "initial"]);
        fs::write(root.join("large.txt"), patterned_text(700 * 1024, b'k')).unwrap();
        Self { root }
    }
}
impl Drop for Repo { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.root); } }

#[test]
fn large_workspace_diff_and_following_terminal_patch_fit_encrypted_relay_frames() {
    let repo = Repo::new();
    let root = ProjectRoot::open(&repo.root).unwrap();
    let project_id = root.project_id().to_string();
    let service = WorkspaceService::new(root, 3);
    let response = service.execute(WorkspaceRequestEnvelope {
        request_id: "large-diff".into(),
        project_id,
        deadline_unix_ms: u64::MAX,
        idempotency_key: None,
        operation: WorkspaceRequest::GitDiff {
            target: GitDiffTarget::WorkingTree,
            cursor: None,
            max_bytes: 700 * 1024,
        },
    });
    assert!(response.result.is_ok());

    let keys = test_keys();
    let mut session = SecureSession::new("room-1", keys.clone());
    let workspace_wire = session
        .encode_wire(Direction::CliToApp, PlainMsg::WorkspaceResponse(response.clone()), MAX_OUTER_FRAME_BYTES)
        .expect("large workspace response fits relay frame");
    let patch = PlainMsg::TerminalPatchV2(TerminalPatchV2 {
        terminal_run_id: "run-1".into(),
        base_snapshot_id: 1,
        from_state_seq: 1,
        to_state_seq: 2,
        attrs: Vec::new(),
        attrs_base_len: None,
        ops: vec![PatchOp::SetCursor(CursorState { row: 0, col: 1, visible: true, style: CursorStyle::Block })],
    });
    let patch_wire = session
        .encode_wire(Direction::CliToApp, patch.clone(), MAX_OUTER_FRAME_BYTES)
        .expect("terminal patch remains sendable after workspace response");

    assert!(workspace_wire.len() <= MAX_OUTER_FRAME_BYTES);
    assert!(patch_wire.len() <= MAX_OUTER_FRAME_BYTES);
    assert_eq!(decode_secure_data(&relaycat_protocol::decode_frame(&workspace_wire).unwrap(), &keys).unwrap(), Some(PlainMsg::WorkspaceResponse(response)));
    assert_eq!(decode_secure_data(&relaycat_protocol::decode_frame(&patch_wire).unwrap(), &keys).unwrap(), Some(patch));
}

fn patterned_text(len: usize, start: u8) -> Vec<u8> {
    (0..len).map(|index| if index % 79 == 78 { b'\n' } else { start + (index % 10) as u8 }).collect()
}

fn git(root: &PathBuf, args: &[&str]) {
    assert!(Command::new("git").args(args).current_dir(root).status().unwrap().success());
}

fn test_keys() -> SessionKeys {
    let cli = KeyPair::from_private_bytes([11; 32]);
    let app = KeyPair::from_private_bytes([12; 32]);
    SessionKeys::derive_for_cli(
        b"room-1", cli.private(), app.public(), cli.public(), app.public(), &[13; 32], &[14; 32], &[15; 32],
    )
}
