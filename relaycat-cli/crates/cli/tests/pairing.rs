use relaycat_cli::{
    command::SessionKind,
    pairing::{
        PairingMaterial, generate_pairing_material, pairing_url, parse_pairing_url,
        render_pairing_qr, render_pairing_qr_with_white_background, write_pairing_qr_png,
    },
};

#[test]
fn pairing_url_carries_relay_room_public_key_and_token() {
    let material = PairingMaterial {
        relay_url: "wss://relay.example.com/ws".to_string(),
        room_id: "room-1".to_string(),
        cli_public_key: [1; 32],
        pairing_token: vec![2; 16],
        session_kind: SessionKind::codex(),
    };

    let url = pairing_url(&material);

    assert_eq!(
        url,
        "relaycat://pair?relay=wss%3A%2F%2Frelay.example.com%2Fws&room=room-1&pubkey=AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE&token=AgICAgICAgICAgICAgICAg&kind=codex"
    );
}

#[test]
fn rendered_pairing_qr_contains_terminal_blocks() {
    let material = PairingMaterial {
        relay_url: "wss://relay.example.com/ws".to_string(),
        room_id: "room-1".to_string(),
        cli_public_key: [3; 32],
        pairing_token: vec![4; 16],
        session_kind: SessionKind::shell(),
    };

    let rendered = render_pairing_qr(&material).expect("render qr");

    #[cfg(not(windows))]
    assert!(rendered.contains("\x1b["));
    #[cfg(windows)]
    assert!(rendered.contains("██"));
    assert!(rendered.lines().count() > 8);
}

#[test]
fn rendered_pairing_qr_can_use_ansi_white_background() {
    let material = PairingMaterial {
        relay_url: "wss://relay.example.com/ws".to_string(),
        room_id: "room-1".to_string(),
        cli_public_key: [3; 32],
        pairing_token: vec![4; 16],
        session_kind: SessionKind::shell(),
    };

    let rendered = render_pairing_qr_with_white_background(&material).expect("render qr");

    assert!(rendered.contains("\x1b["));
    assert!(rendered.contains("\x1b[47m"));
    assert!(rendered.contains("\x1b[0m"));
    assert!(!rendered.contains('▄'));
    assert!(!rendered.contains('█'));
    assert!(rendered.lines().count() > 8);
}

#[test]
fn rendered_pairing_qr_uses_compact_module_width() {
    let material = PairingMaterial {
        relay_url: "wss://relay.example.com/ws".to_string(),
        room_id: "room-1".to_string(),
        cli_public_key: [3; 32],
        pairing_token: vec![4; 16],
        session_kind: SessionKind::shell(),
    };

    let rendered = render_pairing_qr(&material).expect("render qr");
    let visible = strip_ansi_escape_sequences(&rendered);
    let max_width = visible.lines().map(str::len).max().unwrap_or_default();

    assert!(
        max_width <= 160,
        "QR width should stay compact, got {max_width}"
    );
}

fn strip_ansi_escape_sequences(text: &str) -> String {
    let mut stripped = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        stripped.push(ch);
    }
    stripped
}

#[test]
fn writes_pairing_qr_png_file() {
    let material = PairingMaterial {
        relay_url: "wss://relay.example.com/ws".to_string(),
        room_id: "room-1".to_string(),
        cli_public_key: [3; 32],
        pairing_token: vec![4; 16],
        session_kind: SessionKind::shell(),
    };
    let path = std::env::temp_dir().join(format!(
        "relaycat-pairing-{}-{}.png",
        std::process::id(),
        unique_suffix()
    ));

    write_pairing_qr_png(&material, &path).expect("write png");
    let bytes = std::fs::read(&path).expect("read png");
    let _ = std::fs::remove_file(&path);

    assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert!(bytes.len() > 1024);
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time")
        .as_nanos()
}

#[test]
fn generated_pairing_material_has_random_room_key_and_token() {
    let first = generate_pairing_material("wss://relay.example.com/ws");
    let second = generate_pairing_material("wss://relay.example.com/ws");

    assert_eq!(first.relay_url, "wss://relay.example.com/ws");
    assert_eq!(first.room_id.len(), 16);
    assert_eq!(base64_token_len(&first), 22);
    assert_ne!(first.room_id, second.room_id);
    assert_ne!(first.cli_public_key, [0; 32]);
    assert_eq!(first.pairing_token.len(), 16);
    assert_ne!(first.pairing_token, vec![0; 16]);
    assert_ne!(first.pairing_token, second.pairing_token);
}

#[test]
fn parses_pairing_url_back_to_material() {
    let material = PairingMaterial {
        relay_url: "wss://relay.example.com/ws".to_string(),
        room_id: "room-1".to_string(),
        cli_public_key: [1; 32],
        pairing_token: vec![2; 16],
        session_kind: SessionKind::claude(),
    };

    let parsed = parse_pairing_url(&pairing_url(&material)).expect("parse pairing url");

    assert_eq!(parsed, material);
}

#[test]
fn parses_opencode_pairing_url_back_to_material() {
    let material = PairingMaterial {
        relay_url: "wss://relay.example.com/ws".to_string(),
        room_id: "room-1".to_string(),
        cli_public_key: [1; 32],
        pairing_token: vec![2; 16],
        session_kind: SessionKind::opencode(),
    };

    let parsed = parse_pairing_url(&pairing_url(&material)).expect("parse opencode pairing url");

    assert_eq!(parsed, material);
    assert!(pairing_url(&material).ends_with("&kind=opencode"));
}

#[test]
fn custom_session_kind_round_trips_through_pairing_url() {
    let material = PairingMaterial {
        relay_url: "wss://relay.example.com/ws".to_string(),
        room_id: "room-1".to_string(),
        cli_public_key: [1; 32],
        pairing_token: vec![2; 16],
        session_kind: SessionKind::new("gemini").expect("custom kind"),
    };

    let parsed = parse_pairing_url(&pairing_url(&material)).expect("parse custom pairing url");

    assert_eq!(parsed, material);
    assert!(pairing_url(&material).ends_with("&kind=gemini"));
}

#[test]
fn rejects_invalid_custom_session_kind_in_pairing_url() {
    let err = parse_pairing_url(
        "relaycat://pair?relay=wss%3A%2F%2Frelay.example.com%2Fws&room=room-1&pubkey=AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE&token=AgICAgICAgICAgICAgICAg&kind=Bad!",
    )
    .expect_err("reject invalid kind");

    assert!(err.to_string().contains("invalid session kind"));
}

#[test]
fn parses_pairing_url_without_kind_as_shell_for_compatibility() {
    let parsed = parse_pairing_url(
        "relaycat://pair?relay=wss%3A%2F%2Frelay.example.com%2Fws&room=room-1&pubkey=AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE&token=AgICAgICAgICAgICAgICAg",
    )
    .expect("parse legacy pairing url");

    assert_eq!(parsed.session_kind, SessionKind::shell());
}

#[test]
fn rejects_pairing_url_with_wrong_scheme() {
    let err = parse_pairing_url("https://example.com/pair").expect_err("reject wrong scheme");

    assert!(err.to_string().contains("relaycat://pair"));
}

#[test]
fn rejects_pairing_url_with_bad_public_key_length() {
    let err = parse_pairing_url(
        "relaycat://pair?relay=wss%3A%2F%2Frelay.example.com%2Fws&room=room-1&pubkey=AQ&token=AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI",
    )
    .expect_err("reject bad pubkey");

    assert!(err.to_string().contains("pubkey"));
}

#[test]
fn rejects_pairing_url_with_too_short_token() {
    let err = parse_pairing_url(
        "relaycat://pair?relay=wss%3A%2F%2Frelay.example.com%2Fws&room=room-1&pubkey=AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE&token=AQ",
    )
    .expect_err("reject short token");

    assert!(err.to_string().contains("token"));
}

fn base64_token_len(material: &PairingMaterial) -> usize {
    pairing_url(material)
        .split("token=")
        .nth(1)
        .and_then(|tail| tail.split('&').next())
        .expect("token query value")
        .len()
}
