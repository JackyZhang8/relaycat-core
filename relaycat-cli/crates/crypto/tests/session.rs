use relaycat_crypto::{
    Direction, KeyPair, PairingRole, SessionKeys, decrypt, encrypt, nonce_for, pairing_token_proof,
    relay_admission,
};

#[test]
fn x25519_handshake_derives_matching_directional_keys() {
    let cli = KeyPair::from_private_bytes([1; 32]);
    let app = KeyPair::from_private_bytes([2; 32]);
    let token_hash = [9; 32];
    let cli_salt = [10; 32];
    let app_salt = [20; 32];

    let cli_keys = SessionKeys::derive_for_cli(
        b"room-1",
        cli.private(),
        app.public(),
        cli.public(),
        app.public(),
        &token_hash,
        &cli_salt,
        &app_salt,
    );
    let app_keys = SessionKeys::derive_for_app(
        b"room-1",
        app.private(),
        cli.public(),
        cli.public(),
        app.public(),
        &token_hash,
        &cli_salt,
        &app_salt,
    );

    assert_eq!(cli_keys.cli_to_app_key(), app_keys.cli_to_app_key());
    assert_eq!(cli_keys.app_to_cli_key(), app_keys.app_to_cli_key());
    assert_ne!(cli_keys.cli_to_app_key(), cli_keys.app_to_cli_key());
}

#[test]
fn connection_salt_changes_keys_but_both_sides_still_agree() {
    let cli = KeyPair::from_private_bytes([1; 32]);
    let app = KeyPair::from_private_bytes([2; 32]);
    let token_hash = [9; 32];

    let derive = |cli_salt: &[u8; 32], app_salt: &[u8; 32]| {
        let cli_keys = SessionKeys::derive_for_cli(
            b"room-1",
            cli.private(),
            app.public(),
            cli.public(),
            app.public(),
            &token_hash,
            cli_salt,
            app_salt,
        );
        let app_keys = SessionKeys::derive_for_app(
            b"room-1",
            app.private(),
            cli.public(),
            cli.public(),
            app.public(),
            &token_hash,
            cli_salt,
            app_salt,
        );
        // Both peers must always derive identical keys.
        assert_eq!(cli_keys.cli_to_app_key(), app_keys.cli_to_app_key());
        assert_eq!(cli_keys.app_to_cli_key(), app_keys.app_to_cli_key());
        cli_keys
    };

    // Two distinct connections (different salts) must derive different keys,
    // which is what prevents ChaCha20-Poly1305 nonce reuse across reconnects.
    let conn1 = derive(&[10; 32], &[20; 32]);
    let conn2 = derive(&[11; 32], &[20; 32]); // only cli salt rotated
    let conn3 = derive(&[10; 32], &[21; 32]); // only app salt rotated

    assert_ne!(conn1.cli_to_app_key(), conn2.cli_to_app_key());
    assert_ne!(conn1.cli_to_app_key(), conn3.cli_to_app_key());
    assert_ne!(conn2.cli_to_app_key(), conn3.cli_to_app_key());
}

#[test]
fn encrypt_decrypt_round_trip_with_aad_context() {
    let cli = KeyPair::from_private_bytes([3; 32]);
    let app = KeyPair::from_private_bytes([4; 32]);
    let token_hash = [7; 32];
    let keys = SessionKeys::derive_for_cli(
        b"room-1",
        cli.private(),
        app.public(),
        cli.public(),
        app.public(),
        &token_hash,
        &[10; 32],
        &[20; 32],
    );

    let ciphertext = encrypt(
        &keys,
        Direction::CliToApp,
        b"room-1",
        42,
        b"terminal_patch_v2",
        b"hello",
    )
    .expect("encrypt");

    let plaintext = decrypt(
        &keys,
        Direction::CliToApp,
        b"room-1",
        42,
        b"terminal_patch_v2",
        &ciphertext,
    )
    .expect("decrypt");

    assert_eq!(plaintext, b"hello");
}

#[test]
fn decrypt_rejects_wrong_sequence_or_plain_message_type() {
    let cli = KeyPair::from_private_bytes([5; 32]);
    let app = KeyPair::from_private_bytes([6; 32]);
    let keys = SessionKeys::derive_for_cli(
        b"room-1",
        cli.private(),
        app.public(),
        cli.public(),
        app.public(),
        &[8; 32],
        &[10; 32],
        &[20; 32],
    );

    let ciphertext = encrypt(
        &keys,
        Direction::AppToCli,
        b"room-1",
        10,
        b"input_event_v2",
        b"pwd\r",
    )
    .expect("encrypt");

    assert!(
        decrypt(
            &keys,
            Direction::AppToCli,
            b"room-1",
            11,
            b"input_event_v2",
            &ciphertext,
        )
        .is_err()
    );
    assert!(
        decrypt(
            &keys,
            Direction::AppToCli,
            b"room-1",
            10,
            b"resize_event_v2",
            &ciphertext,
        )
        .is_err()
    );
}

#[test]
fn nonce_uses_direction_prefix_and_big_endian_sequence() {
    assert_eq!(
        nonce_for(Direction::CliToApp, 0x0102_0304_0506_0708),
        [0x52, 0x43, 0x43, 0x49, 1, 2, 3, 4, 5, 6, 7, 8]
    );
    assert_eq!(
        nonce_for(Direction::AppToCli, 0x0102_0304_0506_0708),
        [0x52, 0x43, 0x49, 0x43, 1, 2, 3, 4, 5, 6, 7, 8]
    );
}

#[test]
fn pairing_token_proof_binds_room_role_device_key_and_salt() {
    let token = b"pairing-token";
    let salt = [5; 32];
    let proof = pairing_token_proof(token, b"room-1", PairingRole::App, &[3; 32], &salt);

    assert_eq!(
        proof,
        pairing_token_proof(token, b"room-1", PairingRole::App, &[3; 32], &salt)
    );
    assert_ne!(
        proof,
        pairing_token_proof(token, b"room-2", PairingRole::App, &[3; 32], &salt)
    );
    // Tampering with the salt (e.g. a malicious relay) must invalidate the proof.
    assert_ne!(
        proof,
        pairing_token_proof(token, b"room-1", PairingRole::App, &[3; 32], &[6; 32])
    );
}

#[test]
fn relay_admission_binds_token_and_room_without_device_key() {
    let token = b"pairing-token";
    let admission = relay_admission(token, b"room-1");

    assert_eq!(admission, relay_admission(token, b"room-1"));
    assert_ne!(admission, relay_admission(token, b"room-2"));
    assert_ne!(admission, relay_admission(b"other-token", b"room-1"));
}
