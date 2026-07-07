use super::*;
use crate::i18n::CliLanguage;
use relaycat_crypto::{
    KeyPair, PairingRole, SessionKeys, pairing_token_hash, pairing_token_proof,
};

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvVarGuard {
    key: &'static str,
    original: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let original = env::var_os(key);
        // SAFETY: Tests that mutate process environment hold ENV_LOCK, and
        // relay.rs tests do not spawn concurrent env readers while held.
        unsafe {
            env::set_var(key, value);
        }
        Self { key, original }
    }

    fn remove(key: &'static str) -> Self {
        let original = env::var_os(key);
        // SAFETY: See EnvVarGuard::set.
        unsafe {
            env::remove_var(key);
        }
        Self { key, original }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // SAFETY: See EnvVarGuard::set.
        unsafe {
            if let Some(value) = &self.original {
                env::set_var(self.key, value);
            } else {
                env::remove_var(self.key);
            }
        }
    }
}

fn pty_input(filtered: LocalInputFilterResult) -> Vec<u8> {
    assert!(filtered.mirrored_output.is_empty());
    filtered.pty_input
}

fn output_both(filtered: LocalOutputFilterResult) -> Vec<u8> {
    assert_eq!(filtered.local_output, filtered.remote_output);
    assert!(filtered.pty_input.is_empty());
    filtered.local_output
}

fn terminal_row_text(row: &relaycat_protocol::TerminalRow) -> String {
    row.cells
        .iter()
        .flat_map(|run| run.cells.iter())
        .map(|cell| cell.text.as_str())
        .collect::<String>()
        .trim_end()
        .to_string()
}

#[test]
fn pty_output_coalesce_window_is_250ms() {
    assert_eq!(PTY_OUTPUT_COALESCE_WINDOW, Duration::from_millis(250));
}

#[test]
fn terminal_patch_diagnostic_line_includes_encoded_patch_size() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 20,
        rows: 4,
        patch_retention: 8,
    });
    let bytes = b"hello";
    let patch = core
        .feed_vt_bytes(bytes)
        .expect("visible output should emit a patch");
    let encoded_len = encode_plain_msg(&PlainMsg::TerminalPatchV2(patch.clone()))
        .expect("encode patch")
        .len();

    let line = terminal_patch_diagnostic_line("codex", bytes, &core, &patch);

    assert!(
        line.contains(&format!("patch_plain_bytes={encoded_len}")),
        "diagnostic line must include encoded patch byte size, got {line}"
    );
}

#[test]
fn deferred_history_thaw_keeps_delayed_resize_repaint_out_of_scrollback() {
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 20,
        rows: 4,
        patch_retention: 8,
    });
    for line in 1..=9 {
        core.feed_vt_bytes(format!("line {line}\r\n").as_bytes());
    }
    let before = core
        .snapshot()
        .scrollback_window
        .iter()
        .filter(|row| terminal_row_text(row) == "line 7")
        .count();

    let now = Instant::now();
    let mut thaw = DeferredHistoryThaw::default();
    core.freeze_history();
    let _ = core.resize(relaycat_protocol::ResizeEventV2 {
        resize_seq: 1,
        cols: 40,
        rows: 8,
        input_stream_id: "stream-1".to_string(),
        last_input_ack: 0,
    });
    thaw.request(now);

    let repaint_at = now + Duration::from_millis(10);
    thaw.observe_pty_output(repaint_at);
    assert!(
        !thaw.take_due(now + PTY_RESIZE_REPAINT_QUIET_WINDOW),
        "repaint output must extend the frozen quiet window"
    );

    for line in 7..=9 {
        core.feed_vt_bytes(format!("line {line}\r\n").as_bytes());
    }
    assert!(thaw.take_due(repaint_at + PTY_RESIZE_REPAINT_QUIET_WINDOW));
    core.thaw_history();

    let snapshot = core.snapshot();
    let after = snapshot
        .scrollback_window
        .iter()
        .filter(|row| terminal_row_text(row) == "line 7")
        .count();
    assert_eq!(
        after, before,
        "resize repaint must be consumed while history is frozen"
    );
}

#[test]
fn deferred_history_thaw_ignores_render_ack_during_resize_quiet_window() {
    let mut thaw = DeferredHistoryThaw::default();
    thaw.request(Instant::now());

    assert!(
        !thaw.take_render_ack_thaw(),
        "render ack must not end the resize quiet window early"
    );
}

#[test]
fn deferred_history_thaw_has_maximum_freeze_window() {
    let start = Instant::now();
    let mut thaw = DeferredHistoryThaw::default();
    thaw.request(start);

    thaw.observe_pty_output(start + Duration::from_millis(10));
    thaw.observe_pty_output(start + PTY_RESIZE_REPAINT_FREEZE_MAX_WINDOW);

    assert!(
        thaw.take_due(start + PTY_RESIZE_REPAINT_FREEZE_MAX_WINDOW),
        "continuous output after resize must not keep history frozen indefinitely"
    );
}

#[test]
fn remote_resize_ignores_tiny_transient_terminal_sizes() {
    assert!(remote_resize_pty_size(1, 24).is_none());
    assert!(remote_resize_pty_size(80, 1).is_none());
    assert!(remote_resize_pty_size(19, 5).is_none());
    assert!(remote_resize_pty_size(20, 4).is_none());
}

#[test]
fn remote_resize_accepts_usable_terminal_sizes() {
    assert_eq!(
        remote_resize_pty_size(80, 24),
        Some(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
    );
}

#[test]
fn remote_resize_preserves_phone_reported_width() {
    assert_eq!(effective_remote_resize_cols(42), 42);
}

fn host(cols: u16, rows: u16) -> Option<PtySize> {
    Some(PtySize {
        cols,
        rows,
        pixel_width: 0,
        pixel_height: 0,
    })
}

#[test]
fn clamp_remote_size_caps_rows_to_shorter_host() {
    // App viewport taller than the host terminal: codex would otherwise
    // draw its input box below the host's last row and lose it. Clamp rows.
    assert_eq!(clamp_remote_size_to_host(80, 40, host(80, 20)), (80, 20));
}

#[test]
fn clamp_remote_size_caps_cols_to_narrower_host() {
    assert_eq!(clamp_remote_size_to_host(120, 24, host(80, 24)), (80, 24));
}

#[test]
fn clamp_remote_size_keeps_app_size_when_host_is_larger() {
    // Smallest client wins: a host larger than the app must not upscale the
    // PTY beyond what the app asked for.
    assert_eq!(clamp_remote_size_to_host(80, 24, host(200, 60)), (80, 24));
}

#[test]
fn clamp_remote_size_passes_through_without_host_terminal() {
    // Headless host (no controlling tty): keep the app size unchanged.
    assert_eq!(clamp_remote_size_to_host(80, 40, None), (80, 40));
}

#[test]
fn effective_remote_size_clamps_app_to_host() {
    // The child PTY is always sized to `min(app, host)`, so both the desktop and
    // the phone render the same layout: a narrower host shrinks the PTY, and a
    // wider host never stretches it past what the app asked for.
    assert_eq!(effective_remote_size(42, 20, host(30, 10)), (30, 10));
    assert_eq!(effective_remote_size(200, 60, host(120, 30)), (120, 30));
    assert_eq!(effective_remote_size(80, 24, host(200, 60)), (80, 24));
    // Host size not known yet (headless / not seeded): keep the app size.
    assert_eq!(effective_remote_size(42, 20, None), (42, 20));
}

#[test]
fn log_archive_names_match_rotated_files_only() {
    assert!(is_log_archive_name("cli-2026-07-03-134500.log.gz"));
    assert!(is_log_archive_name("cli-2026-07-03-134500.log"));
    assert!(is_log_archive_name("cli-2026-07-03-134500-1.log.gz"));
    // The live log and unrelated files are never pruned.
    assert!(!is_log_archive_name("cli.log"));
    assert!(!is_log_archive_name("pairing.json"));
    assert!(!is_log_archive_name("cli-2026.log.gz.tmp"));
}

#[test]
fn log_archive_path_is_derived_from_timestamp() {
    let dir = std::env::temp_dir().join(format!("relaycat-logtest-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = unique_archive_path(&dir, "2026-07-03 13:45:00");
    assert_eq!(
        path.file_name().unwrap().to_str().unwrap(),
        "cli-2026-07-03-134500.log"
    );
    // An existing archive for the same second gets a numeric suffix.
    fs::write(&path, b"x").unwrap();
    let next = unique_archive_path(&dir, "2026-07-03 13:45:00");
    assert_eq!(
        next.file_name().unwrap().to_str().unwrap(),
        "cli-2026-07-03-134500-1.log"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn prune_log_archives_keeps_at_most_max_and_spares_live_log() {
    let dir = std::env::temp_dir().join(format!("relaycat-prunetest-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("cli.log"), b"live").unwrap();
    for n in 1..=12 {
        fs::write(
            dir.join(format!("cli-2026-06-{n:02}-000000.log.gz")),
            b"x",
        )
        .unwrap();
    }
    prune_log_archives(&dir, u64::MAX / (24 * 60 * 60) - 1, 10);
    let archives = fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .filter(|e| is_log_archive_name(e.file_name().to_str().unwrap()))
        .count();
    assert_eq!(archives, 10);
    assert!(dir.join("cli.log").exists());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn host_clear_only_when_clamped_below_host() {
    // A host wider or taller than the clamped grid keeps stale child output in
    // the margin, so the host screen must be cleared before the resize.
    assert!(should_clear_host_for_remote_clamp(51, 37, host(120, 40)));
    assert!(should_clear_host_for_remote_clamp(120, 20, host(120, 40)));
    // Grid covers the whole host (or host unknown): nothing stale to clear.
    assert!(!should_clear_host_for_remote_clamp(120, 40, host(120, 40)));
    assert!(!should_clear_host_for_remote_clamp(51, 37, None));
}

#[test]
fn remote_model_size_mirrors_pty() {
    // The app-facing model always matches the PTY exactly so full-screen TUIs
    // keep absolute cursor positioning in sync across desktop and phone.
    assert_eq!(remote_model_size((30, 10)), (30, 10));
    assert_eq!(remote_model_size((120, 30)), (120, 30));
}

#[test]
fn pty_work_mode_keeps_remote_size_across_app_disconnects() {
    let mut mode = PtyWorkMode::new();

    assert_eq!(mode.current(), PtyWorkModeKind::Remote);
    assert!(mode.observe_remote_size((42, 24)));
    assert!(!mode.observe_app_disconnected());
    assert!(!mode.observe_remote_size((90, 30)));
    assert_eq!(mode.current(), PtyWorkModeKind::Remote);
}

#[test]
fn pty_work_mode_accepts_one_remote_size_after_local_mode() {
    let mut mode = PtyWorkMode::new();

    assert!(mode.observe_remote_size((42, 24)));
    assert!(mode.enter_local_mode());
    assert_eq!(mode.current(), PtyWorkModeKind::Local);
    assert!(mode.enter_remote_mode());
    assert_eq!(mode.current(), PtyWorkModeKind::Remote);
    assert!(!mode.observe_remote_size((80, 30)));
    assert_eq!(mode.remote_size(), Some((42, 24)));
}

#[test]
fn pty_work_mode_waits_for_app_size_when_remote_mode_has_no_cached_size() {
    let mut mode = PtyWorkMode::new();

    assert!(mode.enter_local_mode());
    assert!(mode.enter_remote_mode());
    assert!(mode.observe_remote_size((80, 30)));
    assert_eq!(mode.remote_size(), Some((80, 30)));
}

#[test]
fn pty_work_mode_ignores_app_resize_while_local() {
    let mut mode = PtyWorkMode::new();

    assert!(pty_work_mode_observe_app_resize(&mut mode, (42, 24)));
    assert!(mode.enter_local_mode());

    assert!(!pty_work_mode_observe_app_resize(&mut mode, (80, 30)));
    assert_eq!(mode.current(), PtyWorkModeKind::Local);
    assert_eq!(mode.remote_size(), Some((42, 24)));
}

#[test]
fn process_exit_msg_uses_real_exit_code() {
    assert_eq!(
        process_exit_msg(&ExitStatus::with_exit_code(17)),
        PlainMsg::ProcessExit { code: Some(17) }
    );
}

#[test]
fn relay_output_budget_rejects_plain_messages_that_could_exceed_relay_frame_limit() {
    assert_eq!(RELAY_SAFE_PLAIN_MSG_BYTES, 960 * 1024);
    assert!(plain_msg_fits_relay_budget(&PlainMsg::ProcessExit { code: Some(0) }).unwrap());
    assert!(
        !plain_msg_fits_relay_budget(&PlainMsg::InputEventV2(
            relaycat_protocol::InputEventV2 {
                input_stream_id: "stream-1".to_string(),
                input_seq: 1,
                bytes: vec![b'x'; RELAY_SAFE_PLAIN_MSG_BYTES + 1],
            }
        ))
        .unwrap()
    );
}

#[test]
fn relay_input_action_echoes_heartbeat_without_pty_write() {
    assert_eq!(
        relay_input_action(PlainMsg::Heartbeat),
        RelayInputAction::EchoHeartbeat
    );
}

#[test]
fn relay_input_action_routes_v2_control_messages() {
    let render_ack = relaycat_protocol::RenderAckV2 {
        terminal_run_id: "run-1".to_string(),
        snapshot_id: 7,
        applied_state_seq: 44,
    };
    assert_eq!(
        relay_input_action(PlainMsg::RenderAckV2(render_ack.clone())),
        RelayInputAction::RenderAckV2(render_ack)
    );

    let request = relaycat_protocol::RequestSnapshotV2 {
        terminal_run_id: Some("run-1".to_string()),
        reason: relaycat_protocol::SnapshotRequestReason::SeqGap,
        cols: 80,
        rows: 24,
    };
    assert_eq!(
        relay_input_action(PlainMsg::RequestSnapshotV2(request.clone())),
        RelayInputAction::RequestSnapshotV2(request)
    );

    let transcript = relaycat_protocol::RequestTranscriptV2 {
        terminal_run_id: "run-1".to_string(),
        before_entry_id: None,
        max_entries: 20,
    };
    assert_eq!(
        relay_input_action(PlainMsg::RequestTranscriptV2(transcript.clone())),
        RelayInputAction::RequestTranscriptV2(transcript)
    );

    let resize = relaycat_protocol::ResizeEventV2 {
        resize_seq: 3,
        cols: 120,
        rows: 40,
        input_stream_id: "stream-1".to_string(),
        last_input_ack: 7,
    };
    assert_eq!(
        relay_input_action(PlainMsg::ResizeEventV2(resize.clone())),
        RelayInputAction::ResizeEventV2(resize)
    );
}

#[test]
fn terminal_snapshot_request_coalescer_suppresses_duplicate_pending_request() {
    let input_coalescer = TerminalSnapshotRequestCoalescer::default();
    let output_coalescer = input_coalescer.clone();
    let request = relaycat_protocol::RequestSnapshotV2 {
        terminal_run_id: Some("run-1".to_string()),
        reason: relaycat_protocol::SnapshotRequestReason::SeqGap,
        cols: 80,
        rows: 24,
    };

    assert!(input_coalescer.observe(&request));
    assert!(!input_coalescer.observe(&request));
    output_coalescer.mark_request_finished();
    assert!(input_coalescer.observe(&request));
}

#[test]
fn relay_input_action_routes_v2_input_events_for_dedupe() {
    assert_eq!(
        relay_input_action(PlainMsg::InputEventV2(relaycat_protocol::InputEventV2 {
            input_stream_id: "stream-1".to_string(),
            input_seq: 9,
            bytes: b"hi".to_vec(),
        })),
        RelayInputAction::InputEventV2 {
            input_stream_id: "stream-1".to_string(),
            input_seq: 9,
            bytes: b"hi".to_vec(),
        }
    );
}

#[test]
fn local_input_filter_drops_focus_events() {
    let mut filter = LocalInputFilter::default();

    assert_eq!(pty_input(filter.filter(b"a\x1b[I\x1b[Ob")), b"ab");
}

#[test]
fn local_input_filter_intercepts_work_mode_toggle_shortcut() {
    let mut filter = LocalInputFilter::default();

    let filtered = filter.filter(b"a\x07b");

    assert_eq!(filtered.pty_input, b"ab");
    assert_eq!(filtered.toggle_work_mode_count, 1);
}

#[test]
fn terminal_chrome_renders_work_mode_button() {
    let remote = terminal_chrome_line(PtyWorkModeKind::Remote, 80);
    let local = terminal_chrome_line(PtyWorkModeKind::Local, 80);

    assert_eq!(remote, "relaycat 【Remote/App on】Ctrl+G to toggle");
    assert_eq!(local, "relaycat 【Local/App off】Ctrl+G to toggle");
}

#[test]
fn terminal_chrome_line_can_include_project_name_and_session_kind() {
    assert_eq!(
        terminal_chrome_line_for_language(
            PtyWorkModeKind::Remote,
            80,
            &TerminalChromeTitleContext::new(Some("relaycat"), Some("codex")),
            CliLanguage::En
        ),
        "codex·relaycat 【Remote/App on】Ctrl+G to toggle"
    );
}

#[test]
fn terminal_chrome_line_deduplicates_project_name_and_session_kind() {
    assert_eq!(
        terminal_chrome_line_for_language(
            PtyWorkModeKind::Remote,
            80,
            &TerminalChromeTitleContext::new(Some("relaycat"), Some("relaycat")),
            CliLanguage::En
        ),
        "relaycat 【Remote/App on】Ctrl+G to toggle"
    );
}

#[test]
fn terminal_chrome_line_omits_project_separator_when_project_name_is_missing() {
    assert_eq!(
        terminal_chrome_line_for_language(
            PtyWorkModeKind::Remote,
            80,
            &TerminalChromeTitleContext::new(None, Some("codex")),
            CliLanguage::En
        ),
        "codex 【Remote/App on】Ctrl+G to toggle"
    );
}

#[test]
fn terminal_chrome_renders_chinese_work_mode_button() {
    let remote = terminal_chrome_line_for_language(
        PtyWorkModeKind::Remote,
        80,
        &TerminalChromeTitleContext::fallback(),
        CliLanguage::ZhHans,
    );
    let local = terminal_chrome_line_for_language(
        PtyWorkModeKind::Local,
        80,
        &TerminalChromeTitleContext::fallback(),
        CliLanguage::ZhHans,
    );

    assert_eq!(remote, "relaycat 【远程/App开】按 Ctrl+G 切换");
    assert_eq!(local, "relaycat 【本地/App关】按 Ctrl+G 切换");
}

#[test]
fn terminal_chrome_filters_control_characters_from_title_parts() {
    let line = terminal_chrome_line_for_language(
        PtyWorkModeKind::Remote,
        80,
        &TerminalChromeTitleContext::new(Some("bad\u{7}\u{1b}[2J"), Some("codex")),
        CliLanguage::En,
    );
    let sequence = terminal_chrome_sequence(
        PtyWorkModeKind::Remote,
        PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        },
        &TerminalChromeTitleContext::new(Some("bad\u{7}\u{1b}[2J"), Some("codex")),
    );

    assert_eq!(line, "codex·bad[2J 【Remote/App on】Ctrl+G to toggle");
    assert_eq!(sequence[0..4], *b"\x1b]2;");
    assert_eq!(*sequence.last().expect("terminator"), 0x07);
    assert!(
        !sequence[4..sequence.len() - 1]
            .iter()
            .any(|byte| matches!(*byte, 0x00..=0x1f | 0x7f)),
        "OSC title payload must not contain terminal control bytes"
    );
}

#[test]
fn initial_relay_failure_message_says_target_was_not_started() {
    let target = TargetCommand {
        program: "codex".to_string(),
        args: Vec::new(),
        cwd: None,
        relay: None,
        session_kind: SessionKind::codex(),
    };

    assert_eq!(
        initial_relay_unavailable_message(&target),
        "initial relay connection failed; codex was not started. Start or check the relay, then run relaycat again"
    );
}

#[test]
fn pairing_png_status_messages_can_render_chinese() {
    assert_eq!(
        secure_pairing_header_message(CliLanguage::ZhHans),
        "RelayCat 配对"
    );
    assert_eq!(
        pairing_url_message(
            "relaycat://pair?relay=wss%3A%2F%2Frelaycat.example",
            CliLanguage::ZhHans
        ),
        "配对链接:\n  relaycat://pair?relay=wss%3A%2F%2Frelaycat.example"
    );
    assert_eq!(
        pairing_qr_png_written_message(Path::new("/tmp/pairing.png"), CliLanguage::ZhHans),
        "二维码 PNG:\n  /tmp/pairing.png"
    );
    assert_eq!(
        relaycat_logs_message(Path::new("/tmp/cli.log"), CliLanguage::ZhHans),
        "日志:\n  /tmp/cli.log"
    );
}

#[test]
fn terminal_chrome_line_does_not_fill_last_column() {
    let line = terminal_chrome_line(PtyWorkModeKind::Local, 80);

    assert!(line.len() < 80);
}

#[test]
fn terminal_chrome_sequence_clears_old_status_without_autowrap() {
    let sequence = terminal_chrome_sequence(
        PtyWorkModeKind::Local,
        PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        },
        &TerminalChromeTitleContext::fallback(),
    );

    let expected = "\x1b]2;relaycat 【Local/App off】Ctrl+G to toggle\x07".as_bytes();
    assert!(
        sequence
            .windows(expected.len())
            .any(|window| window == expected)
    );
}

#[test]
fn terminal_chrome_sequence_uses_title_without_scroll_region() {
    let sequence = terminal_chrome_sequence(
        PtyWorkModeKind::Local,
        PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        },
        &TerminalChromeTitleContext::fallback(),
    );

    assert!(
        !sequence
            .windows(b"\x1b[2;24r".len())
            .any(|window| window == b"\x1b[2;24r")
    );
    assert!(
        sequence
            .windows(b"Local/App off".len())
            .any(|window| window == b"Local/App off")
    );
}

#[test]
fn child_terminal_env_removes_codex_theme_hints() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _theme = EnvVarGuard::set("RELAYCAT_TERMINAL_THEME", "dark");
    let _colorfgbg = EnvVarGuard::set("COLORFGBG", "0;15");
    let mut command = CommandBuilder::new("codex");

    assert_eq!(
        command
            .get_env("RELAYCAT_TERMINAL_THEME")
            .and_then(|value| value.to_str()),
        Some("dark")
    );
    assert_eq!(
        command
            .get_env("COLORFGBG")
            .and_then(|value| value.to_str()),
        Some("0;15")
    );

    configure_child_terminal_env(&mut command, &SessionKind::codex());

    assert!(command.get_env("RELAYCAT_TERMINAL_THEME").is_none());
    assert!(command.get_env("COLORFGBG").is_none());
}

#[test]
fn terminal_control_can_request_local_mode_transport_disconnect() {
    assert_eq!(
        TerminalV2Control::EnterLocalMode,
        TerminalV2Control::EnterLocalMode
    );
}

#[test]
fn local_content_pty_size_uses_full_host_size() {
    assert_eq!(
        local_content_pty_size(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        }),
        PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        }
    );
}

#[test]
fn local_input_filter_drops_terminal_query_responses() {
    let mut filter = LocalInputFilter::default();

    // CPR responses (\x1b[46;1R) and DSR responses (\x1b[0n) now pass
    // through to PTY so the shell receives real terminal status.
    // DA and color query responses are still dropped.
    assert_eq!(
        pty_input(filter.filter(
            b"a\x1b[46;1R\x1b[0n\x1b[?1;2c\x1b]10;rgb:0000/0000/0000\x07\x1b]11;rgb:ffff/ffff/ffff\x07b"
        )),
        b"a\x1b[46;1R\x1b[0nb"
    );
}

#[test]
fn local_input_filter_passes_decxcpr_response() {
    let mut filter = LocalInputFilter::default();

    // DECXCPR response (\x1b[?46;1R) passes through to PTY
    assert_eq!(
        pty_input(filter.filter(b"a\x1b[?46;1Rb")),
        b"a\x1b[?46;1Rb"
    );
}

#[test]
fn local_input_filter_handles_split_terminal_query_responses() {
    let mut filter = LocalInputFilter::default();

    assert_eq!(pty_input(filter.filter(b"a\x1b[46;")), b"a");
    // CPR response passes through to PTY now
    assert_eq!(
        pty_input(filter.filter(b"1R\x1b]10;rgb:0000")),
        b"\x1b[46;1R"
    );
    assert_eq!(pty_input(filter.filter(b"/0000/0000\x07b")), b"b");
}

#[test]
fn local_input_filter_drops_common_terminal_string_responses() {
    let mut filter = LocalInputFilter::default();

    assert_eq!(
        pty_input(filter.filter(b"a\x1b]11;rgb:ffff/ffff/ffff\x1b\\\x1bP1$r0 q\x1b\\b")),
        b"ab"
    );
}

#[test]
fn local_input_filter_drops_window_and_mode_reports() {
    let mut filter = LocalInputFilter::default();

    assert_eq!(
        pty_input(filter.filter(b"a\x1b[8;40;120t\x1b[?2026;1$yb")),
        b"ab"
    );
}

#[test]
fn local_input_filter_handles_split_focus_events_without_dropping_arrows() {
    let mut filter = LocalInputFilter::default();

    assert_eq!(pty_input(filter.filter(b"a\x1b[")), b"a");
    assert_eq!(pty_input(filter.filter(b"Ib\x1b[")), b"b");
    assert_eq!(pty_input(filter.filter(b"Ac")), b"\x1b[Ac");
}

#[test]
fn local_output_filter_strips_focus_reporting_mode() {
    let mut filter = LocalOutputFilter::default();

    assert_eq!(
        output_both(filter.filter(b"a\x1b[?1004h\x1b[?1004lb")),
        b"ab"
    );
}

#[test]
fn local_output_filter_splits_combined_dec_private_mode_with_1004() {
    let mut filter = LocalOutputFilter::default();

    // Combined \x1b[?1049;1004l should pass \x1b[?1049l to both outputs
    // (alt screen exit) while stripping only mode 1004.
    let filtered = filter.filter(b"a\x1b[?1049;1004lb");
    assert_eq!(filtered.local_output, b"a\x1b[?1049lb");
    assert_eq!(filtered.remote_output, b"a\x1b[?1049lb");

    // Combined \x1b[?2004;1004h should pass \x1b[?2004h
    let filtered = filter.filter(b"\x1b[?2004;1004h");
    assert_eq!(filtered.local_output, b"\x1b[?2004h");
    assert_eq!(filtered.remote_output, b"\x1b[?2004h");

    // Three modes combined: \x1b[?25;1004;7h → pass \x1b[?25;7h
    let filtered = filter.filter(b"\x1b[?25;1004;7h");
    assert_eq!(filtered.local_output, b"\x1b[?25;7h");
    assert_eq!(filtered.remote_output, b"\x1b[?25;7h");

    // Only 1004: nothing passes through
    let filtered = filter.filter(b"x\x1b[?1004hy");
    assert_eq!(filtered.local_output, b"xy");
    assert_eq!(filtered.remote_output, b"xy");
}

#[test]
fn local_output_filter_passes_cpr_to_local_only() {
    let mut filter = LocalOutputFilter::default();

    // \x1b[6n should reach local_output (host terminal) but not remote_output
    let filtered = filter.filter(b"a\x1b[6nb");
    assert_eq!(filtered.local_output, b"a\x1b[6nb");
    assert_eq!(filtered.remote_output, b"ab");
    assert_eq!(filtered.pty_input, b"");
}

#[test]
fn local_output_filter_passes_decxcpr_to_local_only() {
    let mut filter = LocalOutputFilter::default();

    // \x1b[?6n (DECXCPR) should reach local_output only, like \x1b[6n
    let filtered = filter.filter(b"a\x1b[?6nb");
    assert_eq!(filtered.local_output, b"a\x1b[?6nb");
    assert_eq!(filtered.remote_output, b"ab");
    assert_eq!(filtered.pty_input, b"");
}

#[test]
fn local_output_filter_answers_cpr_directly_when_configured() {
    let mut filter = LocalOutputFilter {
        answer_cursor_position_query: true,
        ..LocalOutputFilter::default()
    };

    // With direct answering (GUI relay child on Windows) the query is consumed
    // and a report is fed back into the PTY instead of out to the host.
    let filtered = filter.filter(b"a\x1b[6nb");
    assert_eq!(filtered.local_output, b"ab");
    assert_eq!(filtered.remote_output, b"ab");
    assert_eq!(filtered.pty_input, b"\x1b[1;1R");

    let filtered = filter.filter(b"a\x1b[?6nb");
    assert_eq!(filtered.local_output, b"ab");
    assert_eq!(filtered.remote_output, b"ab");
    assert_eq!(filtered.pty_input, b"\x1b[?1;1R");
}

#[test]
fn local_output_filter_forces_primary_screen_for_codex_remote_only() {
    let mut filter = LocalOutputFilter {
        strip_alternate_screen_from_remote: true,
        ..LocalOutputFilter::default()
    };

    // Host keeps the alt-screen switch so codex renders in the real
    // terminal's native alternate screen; the app-facing stream is forced
    // into the primary screen so codex history stays in managed scrollback.
    let filtered = filter.filter(b"a\x1b[?1049hdraw\x1b[?1049lb");
    assert_eq!(filtered.local_output, b"a\x1b[?1049hdraw\x1b[?1049lb");
    assert_eq!(filtered.remote_output, b"adrawb");
}

#[test]
fn local_output_filter_splits_combined_alt_screen_modes_in_codex() {
    let mut filter = LocalOutputFilter {
        strip_alternate_screen_from_remote: true,
        ..LocalOutputFilter::default()
    };

    // Combined \x1b[?1049;2004l: local keeps the full sequence; remote
    // strips 1049 but keeps \x1b[?2004l.
    let filtered = filter.filter(b"a\x1b[?1049;2004lb");
    assert_eq!(filtered.local_output, b"a\x1b[?1049;2004lb");
    assert_eq!(filtered.remote_output, b"a\x1b[?2004lb");

    // Only alt-screen mode: local keeps it, remote drops it entirely.
    let filtered = filter.filter(b"x\x1b[?1049hy");
    assert_eq!(filtered.local_output, b"x\x1b[?1049hy");
    assert_eq!(filtered.remote_output, b"xy");

    // Combined \x1b[?1049;1004l: host-filtered mode 1004 is dropped from
    // both streams. Local keeps the alt-screen switch (\x1b[?1049l); remote
    // drops both alt-screen and 1004, leaving nothing.
    let filtered = filter.filter(b"a\x1b[?1049;1004lb");
    assert_eq!(filtered.local_output, b"a\x1b[?1049lb");
    assert_eq!(filtered.remote_output, b"ab");
}

#[test]
fn host_scroll_region_expands_bottom_anchored_region_to_host_height() {
    // Full-screen app on a 24-row PTY hosted in a 50-row terminal: the
    // region is expanded to the host's last row so later output is not
    // trapped above row 24.
    assert_eq!(host_scroll_region_sequence(b"\x1b[1;24r", 24, 50), b"\x1b[1;50r");
    // Top margin is preserved.
    assert_eq!(host_scroll_region_sequence(b"\x1b[3;24r", 24, 50), b"\x1b[3;50r");
    // Omitted bottom margin defaults to the last row -> bottom-anchored.
    assert_eq!(host_scroll_region_sequence(b"\x1b[1r", 24, 50), b"\x1b[1;50r");
    assert_eq!(host_scroll_region_sequence(b"\x1b[1;r", 24, 50), b"\x1b[1;50r");
}

#[test]
fn host_scroll_region_leaves_partial_and_safe_regions_untouched() {
    // Status-line app reserving the last row (bottom < pty_rows): untouched.
    assert_eq!(host_scroll_region_sequence(b"\x1b[1;23r", 24, 50), b"\x1b[1;23r");
    // Bare reset already means full screen on the host.
    assert_eq!(host_scroll_region_sequence(b"\x1b[r", 24, 50), b"\x1b[r");
    // Host not taller than the PTY (or sizes unknown): never rewrite.
    assert_eq!(host_scroll_region_sequence(b"\x1b[1;24r", 24, 24), b"\x1b[1;24r");
    assert_eq!(host_scroll_region_sequence(b"\x1b[1;24r", 0, 50), b"\x1b[1;24r");
    // DEC private \x1b[?...r (XTRESTORE) is not a DECSTBM and is untouched.
    assert!(!is_set_scroll_region_sequence(b"\x1b[?1049r"));
}

#[test]
fn local_output_filter_expands_host_scroll_region_remote_unchanged() {
    // Mirrors the `top` repro: PTY clamped to 24 rows under a 50-row host.
    let mut filter = LocalOutputFilter {
        pty_rows: 24,
        host_rows: 50,
        ..LocalOutputFilter::default()
    };

    let filtered = filter.filter(b"\x1b[1;24rprompt");
    // Host stream gets the region expanded to its full height; the app's
    // grid is exactly 24 rows so it keeps the original region.
    assert_eq!(filtered.local_output, b"\x1b[1;50rprompt");
    assert_eq!(filtered.remote_output, b"\x1b[1;24rprompt");
}

#[test]
fn local_output_filter_leaves_scroll_region_alone_without_host_size() {
    // Default filter has unknown sizes (0); both streams pass through.
    let mut filter = LocalOutputFilter::default();
    let filtered = filter.filter(b"\x1b[1;24rprompt");
    assert_eq!(filtered.local_output, b"\x1b[1;24rprompt");
    assert_eq!(filtered.remote_output, b"\x1b[1;24rprompt");
}

#[test]
fn local_output_filter_buffers_split_alternate_screen_sequences() {
    let mut filter = LocalOutputFilter {
        strip_alternate_screen_from_remote: true,
        ..LocalOutputFilter::default()
    };

    // The alt-screen sequence is split across two reads. Once reassembled,
    // local keeps it and only remote strips it.
    let filtered = filter.filter(b"a\x1b[?104");
    assert_eq!(filtered.local_output, b"a");
    assert_eq!(filtered.remote_output, b"a");
    let filtered = filter.filter(b"9hbc");
    assert_eq!(filtered.local_output, b"\x1b[?1049hbc");
    assert_eq!(filtered.remote_output, b"bc");
}

#[test]
fn terminal_output_control_diagnostics_detects_scrolling_controls() {
    assert_eq!(
        terminal_output_control_diagnostics(b"\x1b[2;20r\x1b[3S"),
        TerminalOutputControlDiagnostics {
            csi_scroll_region: true,
            csi_scroll_up_or_down: true,
            alternate_screen: false,
        }
    );
}

#[test]
fn terminal_output_control_diagnostics_detects_alternate_screen_controls() {
    assert_eq!(
        terminal_output_control_diagnostics(b"\x1b[?1049hdraw\x1b[?1049l"),
        TerminalOutputControlDiagnostics {
            csi_scroll_region: false,
            csi_scroll_up_or_down: false,
            alternate_screen: true,
        }
    );
}

#[test]
fn terminal_input_control_name_detects_page_keys() {
    assert_eq!(terminal_input_control_name(b"\x1b[5~"), "page_up");
    assert_eq!(terminal_input_control_name(b"\x1b[6~"), "page_down");
    assert_eq!(terminal_input_control_name(b"x"), "other");
}

#[test]
fn local_output_filter_strips_terminal_queries_that_trigger_stdin_responses() {
    let mut filter = LocalOutputFilter::default();

    // \x1b[6n and \x1b[?6n pass to local_output only (host terminal
    // responds with real cursor position).  Other queries (\x1b[c,
    // \x1b[>c, CSI t) are still stripped from both.
    let filtered = filter.filter(b"a\x1b[6n\x1b[?6n\x1b[c\x1b[>c\x1b[18tb");
    assert_eq!(filtered.local_output, b"a\x1b[6n\x1b[?6nb");
    assert_eq!(filtered.remote_output, b"ab");
    assert_eq!(filtered.pty_input, b"");
}

#[test]
fn local_output_filter_answers_color_queries_deterministically() {
    let mut filter =
        LocalOutputFilter::new_with_color_query_policy(default_terminal_palette(), true);

    let filtered = filter.filter(b"a\x1b]10;?\x07\x1b]4;8;?\x07\x1bP+q544e\x1b\\b");
    assert_eq!(filtered.local_output, b"ab");
    assert_eq!(filtered.remote_output, b"ab");
    assert_eq!(
        filtered.pty_input,
        b"\x1b]10;rgb:0000/0000/0000\x07\x1b]4;8;rgb:7676/7676/7676\x07"
    );
}

#[test]
fn local_output_filter_does_not_answer_color_queries_from_fallback_palette() {
    let mut filter = LocalOutputFilter::default();

    let filtered = filter.filter(b"a\x1b]10;?\x07\x1b]11;?\x07b");

    assert_eq!(filtered.local_output, b"ab");
    assert_eq!(filtered.remote_output, b"ab");
    assert!(filtered.pty_input.is_empty());
}

#[test]
fn default_terminal_palette_is_light_not_dark() {
    let palette = default_terminal_palette();

    assert_eq!(palette.default_fg, TerminalColor::Rgb { r: 0, g: 0, b: 0 });
    assert_eq!(
        palette.default_bg,
        TerminalColor::Rgb {
            r: 255,
            g: 255,
            b: 255,
        }
    );
    assert_eq!(palette.cursor, TerminalColor::Rgb { r: 0, g: 0, b: 0 });
}

#[test]
fn explicit_terminal_theme_does_not_answer_child_color_queries() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _theme = EnvVarGuard::set("RELAYCAT_TERMINAL_THEME", "dark");

    let local_palette = local_terminal_palette();

    assert_eq!(
        local_palette.palette.default_bg,
        dark_terminal_palette().default_bg
    );
    assert!(!local_palette.answers_color_queries());
}

#[test]
fn colorfgbg_palette_does_not_answer_child_color_queries() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _theme = EnvVarGuard::remove("RELAYCAT_TERMINAL_THEME");
    let _colorfgbg = EnvVarGuard::set("COLORFGBG", "0;15");

    let local_palette = local_terminal_palette();

    assert_eq!(local_palette.palette.default_bg, xterm_indexed_color(15));
    assert!(!local_palette.answers_color_queries());
}

#[test]
fn codex_child_color_queries_use_light_palette_when_host_palette_untrusted() {
    let local_palette = LocalTerminalPalette::new(default_terminal_palette(), None);

    let child_palette =
        child_color_query_palette(&local_palette, &SessionKind::codex()).expect("palette");

    assert_eq!(
        local_palette.palette.default_bg,
        default_terminal_palette().default_bg
    );
    assert_eq!(
        child_palette.default_bg,
        light_terminal_palette().default_bg
    );
}

#[test]
fn shell_child_color_queries_do_not_use_untrusted_fallback_palette() {
    let local_palette = LocalTerminalPalette::new(default_terminal_palette(), None);

    assert!(child_color_query_palette(&local_palette, &SessionKind::shell()).is_none());
}

#[test]
fn local_output_filter_answers_color_queries_from_terminal_palette() {
    let mut filter = LocalOutputFilter::new_with_color_query_policy(
        PaletteState {
            default_fg: TerminalColor::Rgb { r: 1, g: 2, b: 3 },
            default_bg: TerminalColor::Rgb {
                r: 250,
                g: 251,
                b: 252,
            },
            cursor: TerminalColor::Rgb { r: 7, g: 8, b: 9 },
            ansi: (0..16)
                .map(|index| {
                    if index == 8 {
                        TerminalColor::Rgb {
                            r: 80,
                            g: 81,
                            b: 82,
                        }
                    } else {
                        TerminalColor::Indexed(index)
                    }
                })
                .collect(),
        },
        true,
    );

    let filtered = filter.filter(b"a\x1b]10;?\x07\x1b]11;?\x07\x1b]12;?\x07\x1b]4;8;?\x07b");

    assert_eq!(filtered.local_output, b"ab");
    assert_eq!(filtered.remote_output, b"ab");
    assert_eq!(
        filtered.pty_input,
        b"\x1b]10;rgb:0101/0202/0303\x07\x1b]11;rgb:fafa/fbfb/fcfc\x07\x1b]12;rgb:0707/0808/0909\x07\x1b]4;8;rgb:5050/5151/5252\x07"
    );
}

#[test]
fn local_output_filter_handles_split_focus_reporting_mode() {
    let mut filter = LocalOutputFilter::default();

    assert_eq!(output_both(filter.filter(b"a\x1b[?")), b"a");
    assert_eq!(output_both(filter.filter(b"1004hbc")), b"bc");
    assert_eq!(output_both(filter.filter(b"\x1b[?25h")), b"\x1b[?25h");
}

#[test]
fn local_output_filter_does_not_rewrite_clear_screen_for_chrome() {
    let mut filter = LocalOutputFilter::default();

    let filtered = filter.filter(b"\x1b[2J");

    assert_eq!(filtered.local_output, b"\x1b[2J");
    assert_eq!(filtered.remote_output, b"\x1b[2J");
}

#[test]
fn local_output_filter_does_not_shift_home_for_chrome() {
    let mut filter = LocalOutputFilter::default();

    let filtered = filter.filter(b"\x1b[H\x1b[3;4H");

    assert_eq!(filtered.local_output, b"\x1b[H\x1b[3;4H");
    assert_eq!(filtered.remote_output, b"\x1b[H\x1b[3;4H");
}

#[test]
fn local_output_filter_does_not_rewrite_scroll_region_for_chrome() {
    let mut filter = LocalOutputFilter::default();

    assert_eq!(filter.filter(b"\x1b[r").local_output, b"\x1b[r");
    assert_eq!(filter.filter(b"\x1b[1;23r").local_output, b"\x1b[1;23r");
}

#[cfg(unix)]
#[test]
fn raw_terminal_mode_disables_local_echo_and_line_editing() {
    // SAFETY: The test only uses this value as an in-memory termios bitfield.
    let mut original = unsafe { MaybeUninit::<libc::termios>::zeroed().assume_init() };
    original.c_lflag = libc::ECHO | libc::ICANON | libc::IEXTEN | libc::ISIG;
    original.c_iflag = libc::ICRNL | libc::IXON;
    original.c_oflag = libc::OPOST;

    let raw = raw_terminal_mode_from(original);

    assert_eq!(raw.c_lflag & libc::ECHO, 0);
    assert_eq!(raw.c_lflag & libc::ICANON, 0);
    assert_eq!(raw.c_lflag & libc::IEXTEN, 0);
    assert_eq!(raw.c_lflag & libc::ISIG, 0);
    assert_eq!(raw.c_iflag & libc::ICRNL, 0);
    assert_eq!(raw.c_iflag & libc::IXON, 0);
    assert_eq!(raw.c_oflag & libc::OPOST, 0);
    assert_eq!(raw.c_cc[libc::VMIN], 1);
    assert_eq!(raw.c_cc[libc::VTIME], 0);
}

#[test]
fn local_interrupt_policy_exits_on_second_ctrl_c_within_window() {
    let now = Instant::now();

    assert_eq!(
        local_interrupt_action(None, now),
        LocalInterruptAction::Forward
    );
    assert_eq!(
        local_interrupt_action(Some(now), now + LOCAL_INTERRUPT_EXIT_WINDOW),
        LocalInterruptAction::Exit
    );
    assert_eq!(
        local_interrupt_action(
            Some(now),
            now + LOCAL_INTERRUPT_EXIT_WINDOW + Duration::from_millis(1)
        ),
        LocalInterruptAction::Forward
    );
}

#[test]
fn claude_local_interrupt_exits_on_first_ctrl_c() {
    let now = Instant::now();

    assert_eq!(
        local_interrupt_action_for_session(SessionKind::claude(), None, now),
        LocalInterruptAction::Exit
    );
    assert_eq!(
        local_interrupt_action_for_session(SessionKind::codex(), None, now),
        LocalInterruptAction::Forward
    );
}

#[test]
fn secure_pairing_wait_reconnects_on_transport_loss_before_peer_joined() {
    let closed: Option<Result<Message, tokio_tungstenite::tungstenite::Error>> = None;
    assert_eq!(
        secure_pairing_transport_action(&closed),
        SecurePairingTransportAction::Reconnect
    );

    let reset = Some(Err(tokio_tungstenite::tungstenite::Error::Protocol(
        tokio_tungstenite::tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
    )));
    assert_eq!(
        secure_pairing_transport_action(&reset),
        SecurePairingTransportAction::Reconnect
    );
}

#[test]
fn secure_pairing_wait_continues_on_live_transport_message() {
    let message = Some(Ok(Message::Binary(Vec::new().into())));

    assert_eq!(
        secure_pairing_transport_action(&message),
        SecurePairingTransportAction::Continue
    );
}

#[test]
fn pairing_control_keys_route_escape_and_ctrl_c_for_launcher() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    assert_eq!(
        pairing_control_action_for_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        Some(PairingControlAction::BackOneLevel)
    );
    assert_eq!(
        pairing_control_action_for_key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        )),
        Some(PairingControlAction::BackToTools)
    );
    assert_eq!(
        pairing_control_action_for_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE)),
        None
    );
}

#[test]
fn secure_peer_rejoin_resets_transport_sequences() {
    let room_id = "room-1";
    let token = vec![9; 16];
    let cli_keypair = KeyPair::from_private_bytes([1; 32]);
    let first_app = KeyPair::from_private_bytes([2; 32]);
    let second_app = KeyPair::from_private_bytes([3; 32]);
    let first_keys = SessionKeys::derive_for_cli(
        room_id.as_bytes(),
        cli_keypair.private(),
        first_app.public(),
        cli_keypair.public(),
        first_app.public(),
        &pairing_token_hash(&token),
        &[5; 32],
        &[6; 32],
    );
    let handshake = CliSecureHandshake::new(room_id, cli_keypair, token.clone());
    let output_session = Arc::new(Mutex::new(SecureSession::new(room_id, first_keys.clone())));
    let input_session = Arc::new(Mutex::new(SecureSession::new(room_id, first_keys.clone())));
    let current_keys = Arc::new(Mutex::new(first_keys));

    let _ = output_session
        .lock()
        .expect("output lock")
        .encode(Direction::CliToApp, PlainMsg::Heartbeat)
        .expect("advance output seq");

    let second_app_salt = [70; 32];
    let second_proof = pairing_token_proof(
        &token,
        room_id.as_bytes(),
        PairingRole::App,
        &second_app.public(),
        &second_app_salt,
    );
    let did_reset = accept_secure_peer_joined_and_reset_sessions(
        &handshake,
        &OuterFrame::PeerJoined {
            role: Role::App,
            device_pubkey: second_app.public(),
            pairing_token_proof: Some(second_proof),
            connection_salt: Some(second_app_salt),
        },
        room_id,
        &output_session,
        &input_session,
        &current_keys,
    )
    .expect("accept peer rejoin");

    assert_eq!(did_reset, SecurePeerJoined::SessionReset);
    let reset_frame = output_session
        .lock()
        .expect("output lock")
        .encode(Direction::CliToApp, PlainMsg::Heartbeat)
        .expect("encode after reset");
    assert!(matches!(reset_frame, OuterFrame::Data { seq: 1, .. }));

    let second_keys = SessionKeys::derive_for_cli(
        room_id.as_bytes(),
        handshake.cli_private(),
        second_app.public(),
        handshake.cli_public(),
        second_app.public(),
        &pairing_token_hash(&token),
        &handshake.connection_salt(),
        &second_app_salt,
    );
    assert_eq!(
        SecureSession::new(room_id, second_keys)
            .decode(&reset_frame)
            .expect("second app can decode reset frame"),
        Some(PlainMsg::Heartbeat)
    );
}

#[test]
fn secure_peer_joined_with_unchanged_keys_preserves_sessions() {
    // A CLI transport reconnect echoes the still-present app's PeerJoined back
    // with the same salts. Re-deriving yields the current keys, so the secure
    // sessions (and their sequence counters) must be kept: resetting would
    // re-encrypt from seq 1 under the same key (nonce reuse) and the app,
    // which kept its counters, would reject the replayed sequences.
    let room_id = "room-1";
    let token = vec![9; 16];
    let cli_keypair = KeyPair::from_private_bytes([1; 32]);
    let app = KeyPair::from_private_bytes([2; 32]);
    let app_salt = [70; 32];
    let handshake = CliSecureHandshake::new(room_id, cli_keypair, token.clone());
    let current = SessionKeys::derive_for_cli(
        room_id.as_bytes(),
        handshake.cli_private(),
        app.public(),
        handshake.cli_public(),
        app.public(),
        &pairing_token_hash(&token),
        &handshake.connection_salt(),
        &app_salt,
    );
    let output_session = Arc::new(Mutex::new(SecureSession::new(room_id, current.clone())));
    let input_session = Arc::new(Mutex::new(SecureSession::new(room_id, current.clone())));
    let current_keys = Arc::new(Mutex::new(current));

    let _ = output_session
        .lock()
        .expect("output lock")
        .encode(Direction::CliToApp, PlainMsg::Heartbeat)
        .expect("advance output seq");

    let proof = pairing_token_proof(
        &token,
        room_id.as_bytes(),
        PairingRole::App,
        &app.public(),
        &app_salt,
    );
    let result = accept_secure_peer_joined_and_reset_sessions(
        &handshake,
        &OuterFrame::PeerJoined {
            role: Role::App,
            device_pubkey: app.public(),
            pairing_token_proof: Some(proof),
            connection_salt: Some(app_salt),
        },
        room_id,
        &output_session,
        &input_session,
        &current_keys,
    )
    .expect("accept echoed peer joined");

    assert_eq!(result, SecurePeerJoined::SessionPreserved);
    let next_frame = output_session
        .lock()
        .expect("output lock")
        .encode(Direction::CliToApp, PlainMsg::Heartbeat)
        .expect("encode after preserved rejoin");
    assert!(matches!(next_frame, OuterFrame::Data { seq: 2, .. }));
}

#[test]
fn secure_transport_reconnect_preserves_sequences_with_current_app_keys() {
    let room_id = "room-1";
    let token = vec![9; 16];
    let cli_keypair = KeyPair::from_private_bytes([1; 32]);
    let first_app = KeyPair::from_private_bytes([2; 32]);
    let second_app = KeyPair::from_private_bytes([3; 32]);
    let first_keys = SessionKeys::derive_for_cli(
        room_id.as_bytes(),
        cli_keypair.private(),
        first_app.public(),
        cli_keypair.public(),
        first_app.public(),
        &pairing_token_hash(&token),
        &[5; 32],
        &[6; 32],
    );
    let handshake = CliSecureHandshake::new(room_id, cli_keypair, token.clone());
    let output_session = Arc::new(Mutex::new(SecureSession::new(room_id, first_keys.clone())));
    let input_session = Arc::new(Mutex::new(SecureSession::new(room_id, first_keys.clone())));
    let current_keys = Arc::new(Mutex::new(first_keys));

    let second_app_salt = [71; 32];
    let second_proof = pairing_token_proof(
        &token,
        room_id.as_bytes(),
        PairingRole::App,
        &second_app.public(),
        &second_app_salt,
    );
    accept_secure_peer_joined_and_reset_sessions(
        &handshake,
        &OuterFrame::PeerJoined {
            role: Role::App,
            device_pubkey: second_app.public(),
            pairing_token_proof: Some(second_proof),
            connection_salt: Some(second_app_salt),
        },
        room_id,
        &output_session,
        &input_session,
        &current_keys,
    )
    .expect("accept second app");

    let second_keys = current_keys.lock().expect("keys lock").clone();
    let mut app_session = SecureSession::new(room_id, second_keys.clone());
    let app_frame = app_session
        .encode(
            Direction::AppToCli,
            PlainMsg::InputEventV2(InputEventV2 {
                input_stream_id: "second-app-process".to_string(),
                input_seq: 1,
                bytes: b"x".to_vec(),
            }),
        )
        .expect("app encodes first input before transport reconnect");

    let _ = input_session
        .lock()
        .expect("input lock")
        .decode(&app_frame)
        .expect("cli decodes first input from second app");
    let app_connected = AtomicBool::new(false);
    let resume_gate = Arc::new(Mutex::new(AppResumeGate::default()));
    complete_transport_reconnect(&app_connected, &resume_gate, &mut || {
        reset_secure_sessions(
            room_id,
            second_keys.clone(),
            &output_session,
            &input_session,
        )
    })
    .expect("transport reconnect completes");

    let app_frame = app_session
        .encode(
            Direction::AppToCli,
            PlainMsg::InputEventV2(InputEventV2 {
                input_stream_id: "second-app-process".to_string(),
                input_seq: 2,
                bytes: b"y".to_vec(),
            }),
        )
        .expect("app encodes second input after transport reconnect");

    let expected = PlainMsg::InputEventV2(InputEventV2 {
        input_stream_id: "second-app-process".to_string(),
        input_seq: 2,
        bytes: b"y".to_vec(),
    });
    assert_eq!(
        input_session
            .lock()
            .expect("input lock")
            .decode(&app_frame)
            .expect("cli expected sequence continues to 2"),
        Some(expected)
    );
}

#[test]
fn mode_switch_reconnect_preserves_sessions_without_marking_app_connected() {
    // A Ctrl-G return to RemoteMode must NOT reset the secure sessions and must
    // NOT mark the app connected: the app was evicted and is re-joining fresh,
    // so PeerJoined(app) is the only place that may reset both sides.
    let app_connected = AtomicBool::new(false);
    let mut reset_called = false;

    preserve_sessions_on_transport_reconnect(&mut || {
        reset_called = true;
        Ok(())
    })
    .expect("mode-switch reconnect completes");

    assert!(!reset_called);
    assert!(!app_connected.load(Ordering::Acquire));
}

#[test]
fn mode_switch_reconnect_marks_cli_connected_without_app_rejoin() {
    // Re-registering at the relay on a Ctrl-G mode switch must clear the
    // "reconnecting" indicator immediately — the CLI is connected as far as
    // the relay is concerned — without marking the app connected. The app
    // reconnects on its own and is only considered connected on PeerJoined,
    // so terminal pushes stay gated until then.
    let bar = TerminalStatusBar::default();
    let app_connected = AtomicBool::new(false);
    bar.set_hint(DISCONNECT_TITLE_HINT);
    assert!(bar.hint_active.load(Ordering::Acquire));

    mark_relay_registered_after_mode_switch(&bar);

    assert!(!bar.hint_active.load(Ordering::Acquire));
    assert!(!app_connected.load(Ordering::Acquire));
}

#[test]
fn unexpected_transport_reconnect_preserves_sessions_and_marks_app_connected() {
    // An unexpected transport drop (socket failed while still in RemoteMode)
    // may leave the app in the room seeing only PeerJoined(cli), so no
    // PeerJoined(app) comes back. The CLI must mark the app connected eagerly
    // so the output task resumes, but must not reset the secure sessions.
    let app_connected = AtomicBool::new(false);
    let resume_gate = Arc::new(Mutex::new(AppResumeGate::default()));
    let mut reset_called = false;

    complete_transport_reconnect(&app_connected, &resume_gate, &mut || {
        reset_called = true;
        Ok(())
    })
    .expect("transport reconnect completes");

    assert!(!reset_called);
    assert!(app_connected.load(Ordering::Acquire));
}

#[test]
fn app_disconnect_marker_only_tracks_connection_state() {
    let app_connected = AtomicBool::new(true);

    mark_app_disconnected(&app_connected);

    assert!(!app_connected.load(Ordering::Acquire));
}

#[test]
fn app_resume_gate_blocks_terminal_state_until_resume_is_processed() {
    let mut gate = AppResumeGate::default();

    gate.mark_app_rejoined();
    assert!(!gate.can_send_terminal_state(true));

    gate.mark_resume_processed();
    assert!(gate.can_send_terminal_state(true));

    gate.mark_app_disconnected();
    assert!(!gate.can_send_terminal_state(true));
    assert!(!gate.can_send_terminal_state(false));
}

#[test]
fn strip_windows_verbatim_prefix_simplifies_disk_and_unc_paths() {
    assert_eq!(
        strip_windows_verbatim_prefix(r"\\?\C:\relaycat"),
        r"C:\relaycat"
    );
    assert_eq!(
        strip_windows_verbatim_prefix(r"\\?\UNC\server\share\proj"),
        r"\\server\share\proj"
    );
    // Already-clean Windows and POSIX paths are left untouched.
    assert_eq!(strip_windows_verbatim_prefix(r"C:\relaycat"), r"C:\relaycat");
    assert_eq!(
        strip_windows_verbatim_prefix("/home/user/relaycat"),
        "/home/user/relaycat"
    );
}

#[test]
fn cli_metadata_can_send_before_terminal_resume() {
    assert!(can_send_without_terminal_resume(&PlainMsg::CliMetadata(
        CliMetadata {
            project_path: "/work/project".to_string(),
        },
    )));
    assert!(!can_send_without_terminal_resume(
        &PlainMsg::TerminalPatchV2(relaycat_protocol::TerminalPatchV2 {
            terminal_run_id: "run-1".to_string(),
            base_snapshot_id: 1,
            from_state_seq: 2,
            to_state_seq: 2,
            attrs: Vec::new(),
            attrs_base_len: None,
            ops: Vec::new(),
        }),
    ));
}

#[test]
fn hello_ack_can_send_before_terminal_resume() {
    // The capability handshake reply must not be gated by the resume window.
    assert!(can_send_without_terminal_resume(&PlainMsg::HelloAckV2(
        HelloAckV2 {
            selected_protocol_version: 2,
            capabilities: vec![ProtocolCapabilityV2::Compression],
        },
    )));
}

#[test]
fn negotiate_capabilities_intersects_with_peer() {
    let ack = hello_ack_for(&HelloV2 {
        protocol_versions: vec![1, 2],
        capabilities: vec![
            ProtocolCapabilityV2::TerminalState,
            ProtocolCapabilityV2::Compression,
        ],
    });
    assert_eq!(ack.selected_protocol_version, 2);
    assert!(ack.capabilities.contains(&ProtocolCapabilityV2::Compression));
    assert!(ack.capabilities.contains(&ProtocolCapabilityV2::TerminalState));
    // A capability the peer did not advertise is not negotiated.
    assert!(!ack.capabilities.contains(&ProtocolCapabilityV2::CliMetadata));
}

#[test]
fn negotiate_capabilities_empty_peer_yields_no_compression() {
    // A legacy app that sends no capabilities (or never sends Hello at all)
    // must end up with compression disabled, keeping the link uncompressed.
    let ack = hello_ack_for(&HelloV2 {
        protocol_versions: vec![],
        capabilities: vec![],
    });
    assert_eq!(ack.selected_protocol_version, TERMINAL_STATE_PROTOCOL_V2);
    assert!(ack.capabilities.is_empty());
}

#[test]
fn cli_metadata_uses_target_project_directory() {
    let target = TargetCommand {
        program: "sh".to_string(),
        args: Vec::new(),
        cwd: Some(PathBuf::from("/tmp/relaycat-metadata-project")),
        relay: None,
        session_kind: SessionKind::shell(),
    };

    assert_eq!(
        cli_metadata_for_target(&target).unwrap(),
        CliMetadata {
            project_path: "/tmp/relaycat-metadata-project".to_string(),
        },
    );
}

#[test]
fn relay_transport_reconnect_policy_backs_off_to_cap() {
    assert_eq!(
        RelayTransportReconnectPolicy::retry_delay(1),
        Duration::from_secs(1)
    );
    assert_eq!(
        RelayTransportReconnectPolicy::retry_delay(2),
        Duration::from_secs(2)
    );
    assert_eq!(
        RelayTransportReconnectPolicy::retry_delay(5),
        Duration::from_secs(16)
    );
    assert_eq!(
        RelayTransportReconnectPolicy::retry_delay(10),
        RelayTransportReconnectPolicy::MAX_DELAY
    );
}

#[cfg(any(unix, windows))]
#[test]
fn process_usage_totals_relaycat_process_and_target_subtree() {
    let rows = vec![
        ProcessUsageRow {
            pid: 100,
            parent_pid: 1,
            cpu_percent_x10: 12,
            rss_bytes: 10_000,
        },
        ProcessUsageRow {
            pid: 200,
            parent_pid: 100,
            cpu_percent_x10: 23,
            rss_bytes: 20_000,
        },
        ProcessUsageRow {
            pid: 201,
            parent_pid: 200,
            cpu_percent_x10: 5,
            rss_bytes: 30_000,
        },
        ProcessUsageRow {
            pid: 202,
            parent_pid: 100,
            cpu_percent_x10: 99,
            rss_bytes: 99_000,
        },
        ProcessUsageRow {
            pid: 300,
            parent_pid: 1,
            cpu_percent_x10: 88,
            rss_bytes: 88_000,
        },
    ];

    assert_eq!(
        total_process_usage(100, Some(200), &rows),
        Some((40, 60_000))
    );
}

#[cfg(unix)]
#[test]
fn parse_process_usage_table_reads_pid_parent_cpu_and_rss() {
    let table = "\
  100     1   1.2  10
  200   100   2.3  20
";

    assert_eq!(
        parse_process_usage_table(table),
        vec![
            ProcessUsageRow {
                pid: 100,
                parent_pid: 1,
                cpu_percent_x10: 12,
                rss_bytes: 10 * 1024,
            },
            ProcessUsageRow {
                pid: 200,
                parent_pid: 100,
                cpu_percent_x10: 23,
                rss_bytes: 20 * 1024,
            },
        ]
    );
}

#[test]
fn relaycat_log_line_uses_timestamp_level_and_message() {
    assert_eq!(
        format_relaycat_log_line(
            "2026-05-20 08:45:30",
            "WARN",
            "relay reconnect attempt 1 failed"
        ),
        "[2026-05-20 08:45:30] WARN relaycat: relay reconnect attempt 1 failed"
    );
}

#[test]
fn extract_latest_osc_title_picks_up_osc_0_1_2_sequences() {
    assert_eq!(
        extract_latest_osc_title(b"prefix\x1b]2;hello\x07tail"),
        Some(b"\x1b]2;hello\x07".to_vec())
    );
    assert_eq!(
        extract_latest_osc_title(b"\x1b]0;icon-and-window\x1b\\"),
        Some(b"\x1b]0;icon-and-window\x1b\\".to_vec())
    );
    assert_eq!(
        extract_latest_osc_title(b"\x1b]1;icon\x07"),
        Some(b"\x1b]1;icon\x07".to_vec())
    );
}

#[test]
fn extract_latest_osc_title_returns_last_when_multiple() {
    assert_eq!(
        extract_latest_osc_title(b"\x1b]2;first\x07middle\x1b]2;second\x07"),
        Some(b"\x1b]2;second\x07".to_vec())
    );
}

#[test]
fn extract_latest_osc_title_ignores_non_title_osc() {
    assert_eq!(
        extract_latest_osc_title(b"\x1b]10;rgb:0000/0000/0000\x07"),
        None
    );
    assert_eq!(extract_latest_osc_title(b"no-osc-here"), None);
}

#[test]
fn terminal_status_bar_observes_latest_child_title() {
    let bar = TerminalStatusBar::default();
    bar.observe_child_output(b"\x1b]2;one\x07");
    bar.observe_child_output(b"plain text without title");
    bar.observe_child_output(b"\x1b]2;two\x07\x1b]0;three\x07");

    assert_eq!(
        bar.last_child_title.lock().unwrap().as_deref(),
        Some(b"\x1b]0;three\x07".as_ref())
    );
}

#[test]
fn local_output_filter_suppresses_child_title_locally_but_keeps_remote_semantics() {
    let mut filter = LocalOutputFilter::default();

    let filtered = filter.filter(b"before\x1b]2;child-title\x07after");

    assert_eq!(filtered.local_output, b"beforeafter");
    assert_eq!(filtered.remote_output, b"before\x1b]2;child-title\x07after");
}

#[test]
fn terminal_status_bar_suppresses_mode_title_while_hint_active() {
    let bar = TerminalStatusBar::default();
    let size = PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    };

    assert!(
        bar.mode_title_sequence(
            PtyWorkModeKind::Remote,
            size,
            &TerminalChromeTitleContext::fallback()
        )
        .is_some()
    );
    bar.hint_active.store(true, Ordering::Release);
    assert_eq!(
        bar.mode_title_sequence(
            PtyWorkModeKind::Remote,
            size,
            &TerminalChromeTitleContext::fallback()
        ),
        None
    );
}

#[test]
fn terminal_chrome_render_size_uses_fallback_when_current_size_unknown() {
    let fallback = PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    };
    let current = PtySize {
        rows: 30,
        cols: 100,
        pixel_width: 0,
        pixel_height: 0,
    };

    assert_eq!(terminal_chrome_render_size(None, fallback), fallback);
    assert_eq!(
        terminal_chrome_render_size(Some(current), fallback),
        current
    );
}

#[test]
fn terminal_status_bar_can_clear_hint_for_forced_local_mode_title() {
    let bar = TerminalStatusBar::default();
    let size = PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    };

    bar.hint_active.store(true, Ordering::Release);
    assert_eq!(
        bar.mode_title_sequence(
            PtyWorkModeKind::Local,
            size,
            &TerminalChromeTitleContext::fallback()
        ),
        None
    );

    bar.clear_hint_for_mode_title();

    assert!(
        bar.mode_title_sequence(
            PtyWorkModeKind::Local,
            size,
            &TerminalChromeTitleContext::fallback()
        )
        .is_some()
    );
}

#[test]
fn join_pty_output_thread_returns_when_thread_finishes() {
    let handle = thread::spawn(|| Ok(()));
    assert!(join_pty_output_thread_within(
        handle,
        Duration::from_secs(5)
    ));
}

#[test]
fn join_pty_output_thread_detaches_when_thread_is_stuck() {
    // A reader thread stuck in a blocking read must not pin the caller: the
    // join detaches after the grace period so the terminal can be restored.
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let handle = thread::spawn(move || {
        let _ = rx.recv();
        Ok(())
    });
    let start = Instant::now();
    assert!(!join_pty_output_thread_within(
        handle,
        Duration::from_millis(80)
    ));
    assert!(start.elapsed() < Duration::from_secs(2));
    drop(tx);
}

#[cfg(unix)]
#[test]
fn kill_child_process_group_signals_the_whole_group() {
    use std::os::unix::process::CommandExt;
    use std::process::Command;

    // Spawn a session leader (its pid == its process-group id) running a
    // long sleep. Killing the group must reach it even though we never
    // signal the pid directly through the child handle.
    let mut child = unsafe {
        Command::new("sleep")
            .arg("30")
            .pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            })
            .spawn()
            .expect("spawn sleep")
    };

    kill_child_process_group(Some(child.id()));

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait().expect("try_wait") {
            Some(_) => break,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                panic!("process group was not killed within the deadline");
            }
            None => thread::sleep(Duration::from_millis(20)),
        }
    }
}

#[test]
fn extension_needs_cmd_wrapper_matches_batch_shims_only() {
    for ext in ["cmd", "CMD", "Cmd", "bat", "BAT"] {
        assert!(
            super::extension_needs_cmd_wrapper(ext),
            "{ext} should be wrapped"
        );
    }
    for ext in ["exe", "EXE", "com", "ps1", "py", ""] {
        assert!(
            !super::extension_needs_cmd_wrapper(ext),
            "{ext} should not be wrapped"
        );
    }
}

#[test]
fn windows_cmd_wrapped_routes_through_comspec() {
    let (program, args) = super::windows_cmd_wrapped(
        "C:\\Windows\\System32\\cmd.exe",
        "codex",
        &["--no-alt-screen".to_string(), "--cd".to_string()],
    );
    assert_eq!(program, "C:\\Windows\\System32\\cmd.exe");
    assert_eq!(args, vec!["/d", "/c", "codex", "--no-alt-screen", "--cd"]);
}

#[test]
fn windows_cmd_wrapped_with_no_extra_args() {
    let (program, args) = super::windows_cmd_wrapped("cmd.exe", "opencode", &[]);
    assert_eq!(program, "cmd.exe");
    assert_eq!(args, vec!["/d", "/c", "opencode"]);
}

#[test]
fn mouse_report_modes_track_dec_private_mode_changes() {
    let mut modes = MouseReportModes::default();
    assert_eq!(modes.wheel_encoding(), WheelEncoding::Disabled);

    // Combined tracking + SGR enable, as emitted by SGR-capable TUIs.
    modes.observe_output(b"draw\x1b[?1002;1006h more");
    assert_eq!(modes.wheel_encoding(), WheelEncoding::Sgr);

    // Dropping SGR falls back to legacy X10 while tracking stays on.
    modes.observe_output(b"\x1b[?1006l");
    assert_eq!(modes.wheel_encoding(), WheelEncoding::X10);

    // urxvt encoding.
    modes.observe_output(b"\x1b[?1015h");
    assert_eq!(modes.wheel_encoding(), WheelEncoding::Urxvt);

    // Disabling tracking disables wheel forwarding entirely.
    modes.observe_output(b"\x1b[?1002l");
    assert_eq!(modes.wheel_encoding(), WheelEncoding::Disabled);
}

#[test]
fn mouse_report_modes_handle_chunk_split_sequences() {
    let mut modes = MouseReportModes::default();
    modes.observe_output(b"text\x1b[?10");
    modes.observe_output(b"03;1006h");
    assert_eq!(modes.wheel_encoding(), WheelEncoding::Sgr);
}

#[test]
fn wheel_rewrite_passes_sgr_through_and_reencodes_x10() {
    let input = b"\x1b[<64;40;12M\x1b[<65;40;12M";

    let sgr = rewrite_app_wheel_reports(input, WheelEncoding::Sgr);
    assert_eq!(sgr.bytes, input.to_vec());
    assert_eq!(sgr.reports, 2);

    let x10 = rewrite_app_wheel_reports(input, WheelEncoding::X10);
    assert_eq!(
        x10.bytes,
        [b"\x1b[M".as_slice(), &[32 + 64, 32 + 40, 32 + 12], b"\x1b[M", &[32 + 65, 32 + 40, 32 + 12]].concat()
    );
    assert_eq!(x10.reports, 2);

    let urxvt = rewrite_app_wheel_reports(input, WheelEncoding::Urxvt);
    assert_eq!(urxvt.bytes, b"\x1b[96;40;12M\x1b[97;40;12M".to_vec());

    // Tracking off: wheel reports are dropped, other input is untouched.
    let disabled = rewrite_app_wheel_reports(b"a\x1b[<64;1;1Mb", WheelEncoding::Disabled);
    assert_eq!(disabled.bytes, b"ab".to_vec());
    assert_eq!(disabled.reports, 1);
}

#[test]
fn wheel_rewrite_ignores_non_wheel_sgr_reports() {
    // Left-button press (button 0) is not a wheel report and must pass through.
    let input = b"\x1b[<0;10;5M";
    let result = rewrite_app_wheel_reports(input, WheelEncoding::X10);
    assert_eq!(result.bytes, input.to_vec());
    assert_eq!(result.reports, 0);
}
