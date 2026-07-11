use relaycat_cli::command::SessionKind;
use relaycat_cli::pairing_store::{load_session, session_file_path};
use relaycat_cli::relay::generate_pairing_material_for_run;

#[test]
fn generates_distinct_material_for_each_run_with_same_relay_and_kind() {
    let dir = tempfile_dir("relaycat-pairing-material-distinct");
    let kind = SessionKind::codex();

    let (kp1, mat1) =
        generate_pairing_material_for_run(&dir, "wss://relay.example.com", &kind).expect("first");
    let (kp2, mat2) =
        generate_pairing_material_for_run(&dir, "wss://relay.example.com", &kind).expect("second");

    assert_ne!(mat1.room_id, mat2.room_id);
    assert_ne!(mat1.cli_public_key, mat2.cli_public_key);
    assert_ne!(mat1.pairing_token, mat2.pairing_token);
    assert_ne!(kp1.public(), kp2.public());

    // The latest session is still persisted so `qr` can re-display it later.
    let stored = load_session(&session_file_path(&dir, &kind))
        .expect("load")
        .expect("session present");
    assert_eq!(stored.room_id, mat2.room_id);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn regenerates_material_when_relay_changes() {
    let dir = tempfile_dir("relaycat-pairing-material-relay-change");
    let kind = SessionKind::shell();

    let (_, mat1) =
        generate_pairing_material_for_run(&dir, "wss://relay-a.example.com", &kind).expect("first");
    let (_, mat2) = generate_pairing_material_for_run(&dir, "wss://relay-b.example.com", &kind)
        .expect("second");

    assert_ne!(mat1.room_id, mat2.room_id);
    assert_eq!(mat2.relay_url, "wss://relay-b.example.com");

    std::fs::remove_dir_all(&dir).ok();
}

fn tempfile_dir(prefix: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "{}-{}-{}",
        prefix,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).expect("create temp dir");
    path
}
