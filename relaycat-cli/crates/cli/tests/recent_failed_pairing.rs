use assert_cmd::Command;
use relaycat_cli::{
    command::SessionKind,
    pairing_store::{load_session, session_file_path},
    relay::generate_pairing_material_for_run,
};

#[test]
fn failed_initial_relay_connection_does_not_create_recent_record() {
    let config_home = tempfile_dir("relaycat-failed-recent");
    let project = tempfile_dir("relaycat-failed-recent-project");

    Command::cargo_bin("relaycat")
        .expect("relaycat binary")
        .args([
            "codex",
            "--project",
            project.to_str().expect("utf-8 project path"),
            "--relay",
            "not-a-websocket-url",
        ])
        .env("XDG_CONFIG_HOME", &config_home)
        .assert()
        .failure();

    assert!(
        !config_home.join("relaycat/recent.json").exists(),
        "failed initial relay connection must not be stored as a recent session"
    );
}

#[test]
fn failed_initial_relay_connection_does_not_replace_latest_pairing_session() {
    let config_home = tempfile_dir("relaycat-failed-session-config");
    let project = tempfile_dir("relaycat-failed-session-project");
    let kind = SessionKind::codex();
    let (_, original_material) =
        generate_pairing_material_for_run(&project, "wss://relay.example.com", &kind)
            .expect("seed pairing session");

    Command::cargo_bin("relaycat")
        .expect("relaycat binary")
        .args([
            "codex",
            "--project",
            project.to_str().expect("utf-8 project path"),
            "--relay",
            "not-a-websocket-url",
        ])
        .env("XDG_CONFIG_HOME", &config_home)
        .assert()
        .failure();

    let stored = load_session(&session_file_path(&project, &kind))
        .expect("load session")
        .expect("session present");
    assert_eq!(
        stored.room_id, original_material.room_id,
        "failed initial relay connection must not replace the latest pairing session"
    );
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
