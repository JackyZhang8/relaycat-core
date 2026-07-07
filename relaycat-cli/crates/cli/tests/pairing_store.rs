use relaycat_cli::{
    command::SessionKind,
    pairing::PairingMaterial,
    pairing_store::{
        StoredPairingSession, cleanup_expired_pairing_qr_pngs, ensure_gitignore,
        latest_pairing_qr_png_path, load_session, pairing_qr_png_path, save_session,
        session_file_path, write_pairing_qr_pngs,
    },
};

#[test]
fn saves_and_loads_kind_scoped_pairing_session() {
    let temp = tempfile_dir("relaycat-pairing-store");
    let path = session_file_path(&temp, &SessionKind::codex());
    let material = PairingMaterial {
        relay_url: "ws://127.0.0.1:8787".to_string(),
        room_id: "abcdefghijklmnop".to_string(),
        cli_public_key: [1; 32],
        pairing_token: vec![2; 16],
        session_kind: SessionKind::codex(),
    };
    let session = StoredPairingSession::new(material.clone(), [3; 32]);

    save_session(&path, &session).expect("save session");
    let loaded = load_session(&path)
        .expect("load session")
        .expect("stored session");

    assert_eq!(loaded.material(), material);
    assert_eq!(loaded.cli_private_key, [3; 32]);
    assert!(!loaded.is_expired_at(session.created_at_unix + 86_400 - 1));
    assert_eq!(
        path.file_name().and_then(|s| s.to_str()),
        Some("session_codex.json")
    );
}

#[test]
fn pairing_session_expires_after_twenty_four_hours() {
    let material = PairingMaterial {
        relay_url: "ws://127.0.0.1:8787".to_string(),
        room_id: "abcdefghijklmnop".to_string(),
        cli_public_key: [1; 32],
        pairing_token: vec![2; 16],
        session_kind: SessionKind::shell(),
    };
    let mut session = StoredPairingSession::new(material, [3; 32]);
    session.created_at_unix = 1_000;

    assert!(!session.is_expired_at(1_000 + 86_400 - 1));
    assert!(session.is_expired_at(1_000 + 86_400));
}

#[test]
fn legacy_pairing_session_without_created_at_is_not_loaded() {
    let temp = tempfile_dir("relaycat-legacy-pairing-store");
    let path = session_file_path(&temp, &SessionKind::shell());
    std::fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
    std::fs::write(
        &path,
        concat!(
            "{\n",
            "  \"version\": 1,\n",
            "  \"relay\": \"ws://127.0.0.1:8787\",\n",
            "  \"room\": \"abcdefghijklmnop\",\n",
            "  \"kind\": \"shell\",\n",
            "  \"cli_private_key\": \"AwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwM\",\n",
            "  \"cli_public_key\": \"AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE\",\n",
            "  \"token\": \"AgICAgICAgICAgICAgICAg\"\n",
            "}\n"
        ),
    )
    .expect("write legacy session");

    let loaded = load_session(&path).expect("load legacy session");

    assert!(loaded.is_none());
}

#[test]
fn ensure_gitignore_adds_relaycat_directory_once() {
    let temp = tempfile_dir("relaycat-gitignore");

    ensure_gitignore(&temp).expect("add gitignore");
    ensure_gitignore(&temp).expect("add gitignore once");

    let text = std::fs::read_to_string(temp.join(".gitignore")).expect("read gitignore");
    assert_eq!(text.lines().filter(|line| *line == ".relaycat/").count(), 1);
}

#[test]
fn opencode_session_uses_kind_scoped_file_name() {
    let temp = tempfile_dir("relaycat-opencode-pairing-store");
    let path = session_file_path(&temp, &SessionKind::opencode());

    assert_eq!(
        path.file_name().and_then(|s| s.to_str()),
        Some("session_opencode.json")
    );
}

#[test]
fn pairing_qr_png_path_is_kind_and_room_scoped() {
    let temp = tempfile_dir("relaycat-pairing-qr-path");
    let material = PairingMaterial {
        relay_url: "ws://127.0.0.1:8787".to_string(),
        room_id: "room-1".to_string(),
        cli_public_key: [1; 32],
        pairing_token: vec![2; 16],
        session_kind: SessionKind::codex(),
    };

    let path = pairing_qr_png_path(&temp, &material);
    let latest = latest_pairing_qr_png_path(&temp, &SessionKind::codex());

    assert_eq!(
        path.strip_prefix(&temp).expect("path under temp"),
        std::path::Path::new(".relaycat/pairing/codex-room-1.png")
    );
    assert_eq!(
        latest.strip_prefix(&temp).expect("path under temp"),
        std::path::Path::new(".relaycat/pairing/latest-codex.png")
    );
}

#[test]
fn cleanup_expired_pairing_qr_pngs_keeps_only_valid_pngs() {
    let temp = tempfile_dir("relaycat-pairing-qr-cleanup");
    let dir = temp.join(".relaycat/pairing");
    std::fs::create_dir_all(&dir).expect("create pairing dir");
    let expired = dir.join("codex-expired.png");
    let valid = dir.join("codex-valid.png");
    let unrelated = dir.join("note.txt");
    std::fs::write(&expired, b"old").expect("write expired");
    std::fs::write(&valid, b"new").expect("write valid");
    std::fs::write(&unrelated, b"old").expect("write unrelated");

    set_mtime_unix(&expired, 1_000);
    set_mtime_unix(&valid, 1_000 + 86_400);
    set_mtime_unix(&unrelated, 1_000);

    cleanup_expired_pairing_qr_pngs(&temp, 1_000 + 86_400).expect("cleanup");

    assert!(!expired.exists());
    assert!(valid.exists());
    assert!(unrelated.exists());
}

#[test]
fn write_pairing_qr_pngs_writes_unique_file_and_latest_copy() {
    let temp = tempfile_dir("relaycat-pairing-qr-write");
    let material = PairingMaterial {
        relay_url: "ws://127.0.0.1:8787".to_string(),
        room_id: "room-1".to_string(),
        cli_public_key: [1; 32],
        pairing_token: vec![2; 16],
        session_kind: SessionKind::codex(),
    };

    let path = write_pairing_qr_pngs(&temp, &material).expect("write qr pngs");
    let latest = latest_pairing_qr_png_path(&temp, &SessionKind::codex());

    assert_eq!(path, pairing_qr_png_path(&temp, &material));
    assert!(path.exists());
    assert!(latest.exists());
    assert!(!temp.join(".relaycat/pairing.png").exists());
    assert!(
        std::fs::read(&path)
            .expect("read png")
            .starts_with(b"\x89PNG\r\n\x1a\n")
    );
    assert_eq!(
        std::fs::read(&latest).expect("read latest"),
        std::fs::read(&path).expect("read unique")
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

#[cfg(unix)]
fn set_mtime_unix(path: &std::path::Path, unix: i64) {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes()).expect("cstring path");
    let times = [
        libc::timespec {
            tv_sec: unix,
            tv_nsec: 0,
        },
        libc::timespec {
            tv_sec: unix,
            tv_nsec: 0,
        },
    ];
    let rc = unsafe { libc::utimensat(libc::AT_FDCWD, c_path.as_ptr(), times.as_ptr(), 0) };
    assert_eq!(rc, 0, "utimensat failed");
}

#[cfg(not(unix))]
fn set_mtime_unix(_path: &std::path::Path, _unix: i64) {
    // This test crate runs in CI on Unix-like hosts. Keep a fallback so the
    // file still compiles on non-Unix targets.
}
