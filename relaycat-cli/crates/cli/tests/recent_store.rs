use relaycat_cli::{
    command::{RelayOptions, SessionKind, TargetCommand},
    i18n::CliLanguage,
    recent_store::{RecentRecord, RecentStore, format_recent_list_for_language},
};

#[test]
fn upsert_records_relay_targets_and_updates_existing_entry() {
    let mut store = RecentStore::default();
    let target = TargetCommand {
        program: "codex".to_string(),
        args: Vec::new(),
        cwd: Some(std::path::PathBuf::from("/tmp/relaycat")),
        relay: Some(RelayOptions {
            url: "wss://relay.example.com".to_string(),
            room_id: None,
        }),
        session_kind: SessionKind::codex(),
    };

    assert!(store.upsert_target(&target, 1000));
    assert!(store.upsert_target(&target, 2000));

    assert_eq!(store.records().len(), 1);
    let record = &store.records()[0];
    assert_eq!(record.id, "codex-relaycat");
    assert_eq!(record.kind, SessionKind::codex());
    assert_eq!(record.project, std::path::Path::new("/tmp/relaycat"));
    assert_eq!(record.relay, "wss://relay.example.com");
    assert_eq!(record.use_count, 2);
    assert_eq!(record.last_used_at, 2000);
}

#[test]
fn upsert_assigns_distinct_ids_to_same_named_projects() {
    let mut store = RecentStore::default();
    let first = TargetCommand {
        program: "codex".to_string(),
        args: Vec::new(),
        cwd: Some(std::path::PathBuf::from("/work/alpha/relaycat")),
        relay: Some(RelayOptions {
            url: "wss://relay.example.com".to_string(),
            room_id: None,
        }),
        session_kind: SessionKind::codex(),
    };
    let second = TargetCommand {
        cwd: Some(std::path::PathBuf::from("/work/beta/relaycat")),
        ..first.clone()
    };

    assert!(store.upsert_target(&first, 1000));
    assert!(store.upsert_target(&second, 2000));

    assert_eq!(store.records().len(), 2);
    assert_ne!(store.records()[0].id, store.records()[1].id);
}

#[test]
fn upsert_ignores_targets_without_relay() {
    let mut store = RecentStore::default();
    let target = TargetCommand {
        program: "codex".to_string(),
        args: Vec::new(),
        cwd: Some(std::path::PathBuf::from("/tmp/relaycat")),
        relay: None,
        session_kind: SessionKind::codex(),
    };

    assert!(!store.upsert_target(&target, 1000));
    assert!(store.records().is_empty());
}

#[test]
fn resolves_numeric_selector_to_target_command() {
    let store = RecentStore::new(vec![RecentRecord {
        id: "codex-relaycat".to_string(),
        kind: SessionKind::codex(),
        program: "codex".to_string(),
        args: Vec::new(),
        project: std::path::PathBuf::from("/tmp/relaycat"),
        relay: "wss://relay.example.com".to_string(),
        last_used_at: 1000,
        use_count: 1,
    }]);

    let target = store.resolve_target("1").expect("target");

    assert_eq!(target.program, "codex");
    assert_eq!(target.session_kind, SessionKind::codex());
    assert_eq!(target.cwd, Some(std::path::PathBuf::from("/tmp/relaycat")));
    assert_eq!(
        target.relay,
        Some(RelayOptions {
            url: "wss://relay.example.com".to_string(),
            room_id: None,
        })
    );
}

#[test]
fn resolves_shell_record_with_custom_command() {
    let store = RecentStore::new(vec![RecentRecord {
        id: "shell-lab".to_string(),
        kind: SessionKind::shell(),
        program: "bash".to_string(),
        args: Vec::new(),
        project: std::path::PathBuf::from("/tmp/lab"),
        relay: "ws://127.0.0.1:8787".to_string(),
        last_used_at: 1000,
        use_count: 1,
    }]);

    let target = store.resolve_target("shell-lab").expect("target");

    assert_eq!(target.program, "bash");
    assert_eq!(target.session_kind, SessionKind::shell());
    assert_eq!(target.cwd, Some(std::path::PathBuf::from("/tmp/lab")));
}

#[test]
fn repairs_duplicate_legacy_ids_when_store_is_constructed() {
    let store = RecentStore::new(vec![
        RecentRecord {
            id: "codex-relaycat".to_string(),
            kind: SessionKind::codex(),
            program: "codex".to_string(),
            args: Vec::new(),
            project: std::path::PathBuf::from("/work/a/relaycat"),
            relay: "wss://relay-a.example.com".to_string(),
            last_used_at: 2000,
            use_count: 1,
        },
        RecentRecord {
            id: "codex-relaycat".to_string(),
            kind: SessionKind::codex(),
            program: "codex".to_string(),
            args: Vec::new(),
            project: std::path::PathBuf::from("/work/b/relaycat"),
            relay: "wss://relay-b.example.com".to_string(),
            last_used_at: 1000,
            use_count: 1,
        },
    ]);

    assert_ne!(store.records()[0].id, store.records()[1].id);
    assert_eq!(store.records()[0].id, "codex-relaycat");
    assert!(store.records()[1].id.starts_with("codex-relaycat-"));
}

#[test]
fn custom_tool_recent_record_preserves_program_and_args() {
    let mut store = RecentStore::default();
    let target = TargetCommand {
        program: "gemini".to_string(),
        args: vec!["--model".to_string(), "flash".to_string()],
        cwd: Some(std::path::PathBuf::from("/tmp/relaycat")),
        relay: Some(RelayOptions {
            url: "wss://relay.example.com".to_string(),
            room_id: None,
        }),
        session_kind: SessionKind::new("gemini").expect("custom kind"),
    };

    assert!(store.upsert_target(&target, 1000));
    let resolved = store.resolve_target("1").expect("target");

    assert_eq!(resolved.program, "gemini");
    assert_eq!(resolved.args, ["--model", "flash"]);
    assert_eq!(resolved.session_kind.as_str(), "gemini");
}

#[test]
fn forget_removes_numeric_selector() {
    let mut store = RecentStore::new(vec![
        RecentRecord {
            id: "codex-relaycat".to_string(),
            kind: SessionKind::codex(),
            program: "codex".to_string(),
            args: Vec::new(),
            project: std::path::PathBuf::from("/tmp/relaycat"),
            relay: "wss://relay.example.com".to_string(),
            last_used_at: 1000,
            use_count: 1,
        },
        RecentRecord {
            id: "claude-app".to_string(),
            kind: SessionKind::claude(),
            program: "claude".to_string(),
            args: Vec::new(),
            project: std::path::PathBuf::from("/tmp/app"),
            relay: "wss://relay.example.com".to_string(),
            last_used_at: 900,
            use_count: 1,
        },
    ]);

    let removed = store.forget("1").expect("removed");

    assert_eq!(removed.id, "codex-relaycat");
    assert_eq!(store.records().len(), 1);
    assert_eq!(store.records()[0].id, "claude-app");
}

#[test]
fn save_and_load_round_trips_records_with_escaped_paths() {
    let temp = tempfile_dir("relaycat-recent-store");
    let path = temp.join("recent.json");
    let store = RecentStore::new(vec![RecentRecord {
        id: "shell-lab".to_string(),
        kind: SessionKind::shell(),
        program: "pwsh".to_string(),
        args: Vec::new(),
        project: std::path::PathBuf::from("/tmp/relay cat/quoted\"project"),
        relay: "wss://relay.example.com/ws?name=a\\b".to_string(),
        last_used_at: 1000,
        use_count: 3,
    }]);

    store.save(&path).expect("save");
    let loaded = RecentStore::load(&path).expect("load");

    assert_eq!(loaded, store);
}

#[test]
fn formats_recent_list_in_chinese() {
    let store = RecentStore::new(vec![RecentRecord {
        id: "codex-relaycat".to_string(),
        kind: SessionKind::codex(),
        program: "codex".to_string(),
        args: Vec::new(),
        project: std::path::PathBuf::from("/tmp/relaycat"),
        relay: "wss://relay.example.com".to_string(),
        last_used_at: 1000,
        use_count: 2,
    }]);

    let formatted = format_recent_list_for_language(&store, CliLanguage::ZhHans);

    assert!(formatted.contains("最近的 relaycat 会话："));
    assert!(formatted.contains("使用 2x"));
    assert!(formatted.contains("运行:"));
}

#[test]
fn formats_empty_recent_list_in_chinese() {
    let formatted = format_recent_list_for_language(&RecentStore::default(), CliLanguage::ZhHans);

    assert_eq!(formatted, "没有最近的 relaycat 会话。\n");
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
