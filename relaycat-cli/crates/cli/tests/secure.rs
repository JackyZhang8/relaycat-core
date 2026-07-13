use relaycat_cli::secure::{
    AppSecureHandshake, CliSecureHandshake, SecureSession, decode_secure_data, encode_secure_data,
    secure_join_frame,
};
use relaycat_cli::{command::SessionKind, pairing::PairingMaterial};
use relaycat_crypto::{
    Direction as CryptoDirection, KeyPair, PairingRole, SessionKeys, pairing_token_hash,
    pairing_token_proof,
};
use relaycat_protocol::{
    Direction, MAX_OUTER_FRAME_BYTES, OuterFrame, PlainMsg, Role, decode_frame,
};

#[test]
fn secure_data_frame_round_trips_plain_msg() {
    let keys = test_keys();
    let msg = PlainMsg::InputEventV2(relaycat_protocol::InputEventV2 {
        input_stream_id: "stream-1".to_string(),
        input_seq: 1,
        bytes: b"pwd\r".to_vec(),
    });

    let frame = encode_secure_data("room-1", Direction::AppToCli, 7, &keys, msg.clone(), false)
        .expect("encode secure data");
    let decoded = decode_secure_data(&frame, &keys).expect("decode secure data");

    assert_eq!(decoded, Some(msg));
}

#[test]
fn secure_data_frame_round_trips_compressed_payload() {
    let keys = test_keys();
    // A large, highly compressible snapshot-like payload.
    let msg = PlainMsg::InputEventV2(relaycat_protocol::InputEventV2 {
        input_stream_id: "stream-1".to_string(),
        input_seq: 1,
        bytes: vec![b' '; 4096],
    });

    let compressed = encode_secure_data("room-1", Direction::CliToApp, 7, &keys, msg.clone(), true)
        .expect("encode compressed secure data");
    let plain = encode_secure_data("room-1", Direction::CliToApp, 7, &keys, msg.clone(), false)
        .expect("encode plain secure data");

    // Compression must actually shrink the wire frame for this payload...
    let compressed_len = match &compressed {
        OuterFrame::Data { ciphertext, .. } => ciphertext.len(),
        _ => panic!("expected data frame"),
    };
    let plain_len = match &plain {
        OuterFrame::Data { ciphertext, .. } => ciphertext.len(),
        _ => panic!("expected data frame"),
    };
    assert!(compressed_len < plain_len);

    // ...and still decode back to the original message (decode auto-detects framing).
    let decoded = decode_secure_data(&compressed, &keys).expect("decode compressed secure data");
    assert_eq!(decoded, Some(msg));
}

#[test]
fn secure_session_encode_wire_rejects_oversized_before_consuming_sequence() {
    let mut session = SecureSession::new("room-1", test_keys());
    let oversized = PlainMsg::InputEventV2(relaycat_protocol::InputEventV2 {
        input_stream_id: "stream-1".to_string(),
        input_seq: 1,
        bytes: vec![0xff; 1024],
    });

    let error = session
        .encode_wire(Direction::CliToApp, oversized, 128)
        .expect_err("oversized frame should be rejected");
    assert!(error.to_string().contains("exceeds"));

    let accepted = session
        .encode_wire(
            Direction::CliToApp,
            PlainMsg::Heartbeat,
            MAX_OUTER_FRAME_BYTES,
        )
        .expect("encode accepted frame");
    let frame = decode_frame(&accepted).expect("decode accepted frame");
    assert!(matches!(frame, OuterFrame::Data { seq: 1, .. }));
}

#[test]
fn secure_session_encode_wire_accepts_near_limit_binary_payload() {
    let mut session = SecureSession::new("r".repeat(256), test_keys());
    let msg = PlainMsg::InputEventV2(relaycat_protocol::InputEventV2 {
        input_stream_id: "stream-1".to_string(),
        input_seq: u64::MAX,
        bytes: vec![0xff; 960 * 1024],
    });

    let encoded = session
        .encode_wire(Direction::CliToApp, msg, MAX_OUTER_FRAME_BYTES)
        .expect("near-limit frame should fit");

    assert!(encoded.len() <= MAX_OUTER_FRAME_BYTES);
}

#[test]
fn secure_data_frame_rejects_tampered_sequence() {
    let keys = test_keys();
    let mut frame = encode_secure_data(
        "room-1",
        Direction::CliToApp,
        9,
        &keys,
        PlainMsg::Heartbeat,
        false,
    )
    .expect("encode secure data");

    if let OuterFrame::Data { seq, .. } = &mut frame {
        *seq += 1;
    }

    assert!(decode_secure_data(&frame, &keys).is_err());
}

#[test]
fn secure_data_frame_rejects_wrong_direction() {
    let keys = test_keys();
    let mut frame = encode_secure_data(
        "room-1",
        Direction::CliToApp,
        9,
        &keys,
        PlainMsg::Heartbeat,
        false,
    )
    .expect("encode secure data");

    if let OuterFrame::Data { direction, .. } = &mut frame {
        *direction = Direction::AppToCli;
    }

    assert!(decode_secure_data(&frame, &keys).is_err());
}

fn test_keys() -> SessionKeys {
    test_keys_with_salts(&[14; 32], &[15; 32])
}

fn test_keys_with_salts(cli_salt: &[u8; 32], app_salt: &[u8; 32]) -> SessionKeys {
    let cli = KeyPair::from_private_bytes([11; 32]);
    let app = KeyPair::from_private_bytes([12; 32]);
    SessionKeys::derive_for_cli(
        b"room-1",
        cli.private(),
        app.public(),
        cli.public(),
        app.public(),
        &[13; 32],
        cli_salt,
        app_salt,
    )
}

#[test]
fn protocol_direction_maps_to_crypto_direction() {
    assert_eq!(
        relaycat_cli::secure::crypto_direction(Direction::CliToApp),
        CryptoDirection::CliToApp
    );
    assert_eq!(
        relaycat_cli::secure::crypto_direction(Direction::AppToCli),
        CryptoDirection::AppToCli
    );
}

#[test]
fn cli_secure_handshake_rejects_missing_connection_salt() {
    let cli = KeyPair::from_private_bytes([21; 32]);
    let app = KeyPair::from_private_bytes([22; 32]);
    let token = vec![23; 16];
    // A proof computed without binding a salt (or a relay that stripped the
    // salt) must be rejected rather than falling back to an unsalted key.
    let proof = pairing_token_proof(&token, b"room-1", PairingRole::App, &app.public(), &[0; 32]);
    let handshake = CliSecureHandshake::new("room-1", cli, token.clone());

    let err = handshake
        .accept_peer_joined(&OuterFrame::PeerJoined {
            role: Role::App,
            device_pubkey: app.public(),
            pairing_token_proof: Some(proof),
            connection_salt: None,
        })
        .expect_err("missing salt rejected");

    assert!(err.to_string().contains("missing connection salt"));
}

#[test]
fn cli_secure_handshake_mixes_connection_salts_when_app_provides_one() {
    let cli = KeyPair::from_private_bytes([24; 32]);
    let app = KeyPair::from_private_bytes([25; 32]);
    let token = vec![26; 16];
    let app_salt = [77_u8; 32];
    let proof = pairing_token_proof(
        &token,
        b"room-1",
        PairingRole::App,
        &app.public(),
        &app_salt,
    );
    let handshake = CliSecureHandshake::new("room-1", cli, token.clone());

    let keys = handshake
        .accept_peer_joined(&OuterFrame::PeerJoined {
            role: Role::App,
            device_pubkey: app.public(),
            pairing_token_proof: Some(proof),
            connection_salt: Some(app_salt),
        })
        .expect("accept peer")
        .expect("peer joined");

    let expected = SessionKeys::derive_for_cli(
        b"room-1",
        handshake.cli_private(),
        app.public(),
        handshake.cli_public(),
        app.public(),
        &pairing_token_hash(&token),
        &handshake.connection_salt(),
        &app_salt,
    );
    assert_eq!(keys, expected);
}

#[test]
fn cli_secure_handshake_rejects_proof_bound_to_tampered_salt() {
    let cli = KeyPair::from_private_bytes([31; 32]);
    let app = KeyPair::from_private_bytes([32; 32]);
    let token = vec![33; 16];
    // The app proved its real salt, but a malicious relay swapped the salt it
    // forwards. The proof no longer matches the forwarded salt → reject.
    let real_salt = [44_u8; 32];
    let proof = pairing_token_proof(
        &token,
        b"room-1",
        PairingRole::App,
        &app.public(),
        &real_salt,
    );
    let handshake = CliSecureHandshake::new("room-1", cli, token.clone());

    let err = handshake
        .accept_peer_joined(&OuterFrame::PeerJoined {
            role: Role::App,
            device_pubkey: app.public(),
            pairing_token_proof: Some(proof),
            connection_salt: Some([99; 32]),
        })
        .expect_err("tampered salt rejected");

    assert!(err.to_string().contains("invalid pairing token proof"));
}

#[test]
fn cli_secure_handshake_ignores_non_peer_joined_frame() {
    let cli = KeyPair::from_private_bytes([41; 32]);
    let handshake = CliSecureHandshake::new("room-1", cli, [42; 32]);

    let result = handshake
        .accept_peer_joined(&OuterFrame::Ping)
        .expect("ignore non peer joined");

    assert_eq!(result, None);
}

#[test]
fn secure_join_frame_binds_cli_proof_to_its_connection_salt() {
    let cli = KeyPair::from_private_bytes([51; 32]);
    let handshake = CliSecureHandshake::new("room-1", cli, [52; 32]);

    let frame = secure_join_frame(&handshake);
    let OuterFrame::Join {
        room_id,
        role,
        device_pubkey,
        pairing_token_proof,
        relay_admission,
        connection_salt,
        supports_join_accepted: _,
        ..
    } = frame
    else {
        panic!("expected join frame");
    };
    assert_eq!(room_id, "room-1");
    assert_eq!(role, Role::Cli);
    assert_eq!(device_pubkey, handshake.cli_public());
    // The stable CLI salt must be present and recorded as the handshake's current salt.
    let salt = handshake.connection_salt();
    assert_eq!(connection_salt, Some(salt));
    // The CLI now proves token possession and binds the proof to that salt.
    assert_eq!(
        pairing_token_proof,
        Some(pairing_token_proof_for(
            &[52; 32],
            "room-1",
            &handshake.cli_public(),
            &salt
        ))
    );
    assert_eq!(
        relay_admission,
        Some(relaycat_crypto::relay_admission(&[52; 32], b"room-1"))
    );
}

#[test]
fn secure_join_frame_reuses_cli_salt_across_transport_reconnects() {
    let cli = KeyPair::from_private_bytes([53; 32]);
    let handshake = CliSecureHandshake::new("room-1", cli, [54; 32]);

    let first = secure_join_frame(&handshake);
    let first_salt = join_proof_and_salt(&first).1.expect("first salt");
    let second = secure_join_frame(&handshake);
    let second_salt = join_proof_and_salt(&second).1.expect("second salt");

    assert_eq!(second_salt, first_salt);
    assert_eq!(handshake.connection_salt(), first_salt);
}

fn pairing_token_proof_for(
    token: &[u8],
    room: &str,
    pubkey: &[u8; 32],
    salt: &[u8; 32],
) -> [u8; 32] {
    pairing_token_proof(token, room.as_bytes(), PairingRole::Cli, pubkey, salt)
}

#[test]
fn app_secure_handshake_builds_join_frame_cli_accepts_and_keys_match() {
    let cli = KeyPair::from_private_bytes([61; 32]);
    let material = PairingMaterial {
        relay_url: "wss://relay.example.com/ws".to_string(),
        room_id: "room-1".to_string(),
        cli_public_key: cli.public(),
        pairing_token: vec![62; 16],
        session_kind: SessionKind::shell(),
    };
    let cli_handshake = CliSecureHandshake::new("room-1", cli, material.pairing_token.clone());
    let app_handshake =
        AppSecureHandshake::new(material.clone(), KeyPair::from_private_bytes([63; 32]));

    // Both sides send their Join first, then each accepts the other's forwarded
    // PeerJoined. The app rotates a fresh salt for its connection; the CLI
    // advertises its stable salt so transport reconnects can preserve seq.
    let cli_join = secure_join_frame(&cli_handshake);
    let app_join = app_handshake.join_frame();
    let (cli_proof, cli_salt) = join_proof_and_salt(&cli_join);
    let (app_proof, app_salt) = join_proof_and_salt(&app_join);
    assert!(cli_salt.is_some(), "cli join must carry a connection salt");
    assert!(app_salt.is_some(), "app join must carry a connection salt");

    let cli_keys = cli_handshake
        .accept_peer_joined(&OuterFrame::PeerJoined {
            role: Role::App,
            device_pubkey: app_handshake.app_public(),
            pairing_token_proof: app_proof,
            connection_salt: app_salt,
        })
        .expect("cli accept")
        .expect("cli keys");
    // The app verifies the CLI's salt-bound proof from PeerJoined and derives
    // keys from the (authenticated) CLI salt + its own salt; both sides agree.
    let app_keys = app_handshake
        .accept_peer_joined(&OuterFrame::PeerJoined {
            role: Role::Cli,
            device_pubkey: cli_handshake.cli_public(),
            pairing_token_proof: cli_proof,
            connection_salt: cli_salt,
        })
        .expect("app accept")
        .expect("app keys");

    assert_eq!(cli_keys, app_keys);
}

#[test]
fn cli_rederives_keys_matching_app_after_transport_reconnect_with_stable_salt() {
    let cli = KeyPair::from_private_bytes([71; 32]);
    let material = PairingMaterial {
        relay_url: "wss://relay.example.com/ws".to_string(),
        room_id: "room-1".to_string(),
        cli_public_key: cli.public(),
        pairing_token: vec![72; 16],
        session_kind: SessionKind::shell(),
    };
    let cli_handshake = CliSecureHandshake::new("room-1", cli, material.pairing_token.clone());
    let app_handshake =
        AppSecureHandshake::new(material.clone(), KeyPair::from_private_bytes([73; 32]));

    // No app has joined yet: nothing to rederive from.
    let _ = secure_join_frame(&cli_handshake);
    assert!(cli_handshake.rederive_session_keys().is_none());

    let app_join = app_handshake.join_frame();
    let (app_proof, app_salt) = join_proof_and_salt(&app_join);
    cli_handshake
        .accept_peer_joined(&OuterFrame::PeerJoined {
            role: Role::App,
            device_pubkey: app_handshake.app_public(),
            pairing_token_proof: app_proof,
            connection_salt: app_salt,
        })
        .expect("cli accept")
        .expect("cli keys");

    let initial_cli_salt = cli_handshake.connection_salt();

    // The CLI's transport drops and it rejoins with the same salt. The app
    // stays in the room, sees PeerJoined(cli, same salt), and must derive the
    // same keys without either side resetting sequence counters.
    let cli_rejoin = secure_join_frame(&cli_handshake);
    let (cli_proof, cli_salt) = join_proof_and_salt(&cli_rejoin);
    assert_eq!(cli_salt, Some(initial_cli_salt));
    let app_keys = app_handshake
        .accept_peer_joined(&OuterFrame::PeerJoined {
            role: Role::Cli,
            device_pubkey: cli_handshake.cli_public(),
            pairing_token_proof: cli_proof,
            connection_salt: cli_salt,
        })
        .expect("app accept")
        .expect("app keys");
    let cli_keys = cli_handshake
        .rederive_session_keys()
        .expect("rederived keys");

    assert_eq!(cli_keys, app_keys);
}

fn join_proof_and_salt(frame: &OuterFrame) -> (Option<[u8; 32]>, Option<[u8; 32]>) {
    match frame {
        OuterFrame::Join {
            pairing_token_proof,
            connection_salt,
            ..
        } => (*pairing_token_proof, *connection_salt),
        _ => unreachable!("expected join frame"),
    }
}

#[test]
fn secure_session_assigns_monotonic_transport_sequence_per_direction() {
    let keys = test_keys();
    let mut session = SecureSession::new("room-1", keys);

    let first = session
        .encode(Direction::CliToApp, PlainMsg::Heartbeat)
        .expect("first encode");
    let second = session
        .encode(Direction::CliToApp, PlainMsg::Heartbeat)
        .expect("second encode");

    assert!(matches!(first, OuterFrame::Data { seq: 1, .. }));
    assert!(matches!(second, OuterFrame::Data { seq: 2, .. }));
}

#[test]
fn secure_session_continues_sequence_when_reusing_existing_session_after_transport_reconnect() {
    let keys = test_keys();
    let mut cli_session = SecureSession::new("room-1", keys.clone());
    let mut app_session = SecureSession::new("room-1", keys);

    let first = cli_session
        .encode(Direction::CliToApp, PlainMsg::Heartbeat)
        .expect("first encode");
    assert_eq!(
        app_session.decode(&first).expect("decode first"),
        Some(PlainMsg::Heartbeat)
    );

    let after_reconnect = cli_session
        .encode(Direction::CliToApp, PlainMsg::Heartbeat)
        .expect("encode after reconnect");

    assert!(matches!(after_reconnect, OuterFrame::Data { seq: 2, .. }));
    assert_eq!(
        app_session
            .decode(&after_reconnect)
            .expect("decode after reconnect"),
        Some(PlainMsg::Heartbeat)
    );
}

#[test]
fn secure_session_rejects_replayed_inbound_sequence() {
    let keys = test_keys();
    let mut sender = SecureSession::new("room-1", keys.clone());
    let mut receiver = SecureSession::new("room-1", keys);

    let frame = sender
        .encode(
            Direction::AppToCli,
            PlainMsg::InputEventV2(relaycat_protocol::InputEventV2 {
                input_stream_id: "stream-1".to_string(),
                input_seq: 1,
                bytes: b"pwd\r".to_vec(),
            }),
        )
        .expect("encode");

    assert!(receiver.decode(&frame).expect("first decode").is_some());
    assert!(receiver.decode(&frame).is_err());
}

#[test]
fn secure_session_decodes_authenticated_future_sequence_after_relay_drop() {
    let keys = test_keys();
    let mut sender = SecureSession::new("room-1", keys.clone());
    let mut receiver = SecureSession::new("room-1", keys);

    let dropped = sender
        .encode(
            Direction::AppToCli,
            PlainMsg::InputEventV2(relaycat_protocol::InputEventV2 {
                input_stream_id: "stream-1".to_string(),
                input_seq: 104,
                bytes: b"a".to_vec(),
            }),
        )
        .expect("encode dropped frame");
    let future = sender
        .encode(
            Direction::AppToCli,
            PlainMsg::InputEventV2(relaycat_protocol::InputEventV2 {
                input_stream_id: "stream-1".to_string(),
                input_seq: 105,
                bytes: b"b".to_vec(),
            }),
        )
        .expect("encode future frame");

    assert_eq!(
        receiver.decode(&future).expect("decode future frame"),
        Some(PlainMsg::InputEventV2(relaycat_protocol::InputEventV2 {
            input_stream_id: "stream-1".to_string(),
            input_seq: 105,
            bytes: b"b".to_vec(),
        }))
    );
    assert!(receiver.decode(&dropped).is_err());
}

#[test]
fn secure_session_can_resume_with_expected_inbound_sequence() {
    let keys = test_keys();
    let mut sender = SecureSession::new_with_sequences("room-1", keys.clone(), 2, 1, 1, 1);
    let mut receiver = SecureSession::new_with_sequences("room-1", keys, 1, 1, 2, 1);

    let frame = sender
        .encode(Direction::CliToApp, PlainMsg::Heartbeat)
        .expect("encode resumed frame");

    assert_eq!(
        receiver.decode(&frame).expect("decode resumed frame"),
        Some(PlainMsg::Heartbeat)
    );
}

#[test]
fn secure_session_discards_stale_epoch_frames_until_first_successful_decode() {
    // After a rejoin the relay can still forward in-flight frames encrypted
    // under the previous keys. Before the first successful decode those are
    // silently discarded so the fresh stream can start cleanly.
    let old_keys = test_keys();
    let mut old_sender = SecureSession::new("room-1", old_keys);

    let new_keys = test_keys_with_salts(&[24; 32], &[25; 32]);
    let mut new_sender = SecureSession::new("room-1", new_keys.clone());
    let mut receiver = SecureSession::new("room-1", new_keys);

    let stale = old_sender
        .encode(Direction::CliToApp, PlainMsg::Heartbeat)
        .expect("encode stale frame");
    assert_eq!(receiver.decode(&stale).expect("discard stale frame"), None);

    let fresh = new_sender
        .encode(Direction::CliToApp, PlainMsg::Heartbeat)
        .expect("encode fresh frame");
    assert_eq!(
        receiver.decode(&fresh).expect("decode fresh frame"),
        Some(PlainMsg::Heartbeat)
    );

    // Once synced, an undecodable frame is a real desync and errors.
    let stale_after_sync = old_sender
        .encode(Direction::CliToApp, PlainMsg::Heartbeat)
        .expect("encode stale frame after sync");
    assert!(receiver.decode(&stale_after_sync).is_err());
}

#[test]
fn secure_session_bounds_pre_sync_stale_frame_discards() {
    let old_keys = test_keys();
    let mut old_sender = SecureSession::new("room-1", old_keys);

    let new_keys = test_keys_with_salts(&[24; 32], &[25; 32]);
    let mut receiver = SecureSession::new("room-1", new_keys);

    for _ in 0..SecureSession::MAX_STALE_FRAME_DISCARDS {
        let stale = old_sender
            .encode(Direction::CliToApp, PlainMsg::Heartbeat)
            .expect("encode stale frame");
        assert_eq!(receiver.decode(&stale).expect("discard stale frame"), None);
    }

    let over_limit = old_sender
        .encode(Direction::CliToApp, PlainMsg::Heartbeat)
        .expect("encode stale frame over limit");
    assert!(receiver.decode(&over_limit).is_err());
}
