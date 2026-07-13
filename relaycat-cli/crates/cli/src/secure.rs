use anyhow::{Context, Result, bail, ensure};
use std::sync::Mutex;

use relaycat_crypto::{
    Direction as CryptoDirection, KeyPair, PairingRole, SessionKeys, decrypt, encrypt,
    generate_connection_salt, nonce_for, pairing_token_hash, pairing_token_proof, relay_admission,
    verify_pairing_token_proof,
};
use relaycat_protocol::{
    Direction, OuterFrame, PlainMsg, Role, decode_plain_msg, encode_frame, encode_plain_msg,
    frame_payload, outer_data_frame_encoded_len, plain_msg_type, plain_msg_types, unframe_payload,
};

use crate::pairing::PairingMaterial;

/// Diagnostic for a `PeerJoined` that carried a proof but no per-connection
/// salt. Protocol v3 makes the salt mandatory, so the most common cause is a
/// relay (or peer) built before per-connection salt existed: it silently drops
/// the unknown `connection_salt` field while still forwarding the proof. Under
/// v2 this fell back to an unsalted key; v3 rejects it instead. The cure is to
/// update the dropped component, so the message says so rather than leaving an
/// opaque "missing connection salt".
const MISSING_CONNECTION_SALT_HELP: &str = "missing connection salt: the peer's join carried a pairing proof but no per-connection salt, which protocol v3 requires. This usually means relaycat-relay (or the app) is older than the per-connection-salt change and silently strips the connection_salt field. Rebuild and restart relaycat-relay, and make sure the app is a v3 build, then retry.";
const CHACHA20_POLY1305_TAG_BYTES: usize = 16;

/// The app peer's authenticated identity and per-connection salt, captured
/// from its most recent verified `PeerJoined`.
#[derive(Debug, Clone, Copy)]
struct AppPeerMaterial {
    device_pubkey: [u8; 32],
    connection_salt: [u8; 32],
}

#[derive(Debug)]
pub struct CliSecureHandshake {
    room_id: String,
    cli_keypair: KeyPair,
    pairing_token: Vec<u8>,
    /// Salt mixed into key derivation and advertised in every CLI `Join`.
    /// It stays stable across CLI/GUI relay transport reconnects so peers that
    /// keep the same secure session do not reset sequence counters.
    connection_salt: Mutex<[u8; 32]>,
    /// The app's authenticated pubkey and salt from its latest `PeerJoined`,
    /// kept so [`Self::rederive_session_keys`] can bind the current CLI salt to
    /// the app's still-current salt.
    app_peer: Mutex<Option<AppPeerMaterial>>,
}

impl CliSecureHandshake {
    pub fn new(
        room_id: impl Into<String>,
        cli_keypair: KeyPair,
        pairing_token: impl Into<Vec<u8>>,
    ) -> Self {
        Self {
            room_id: room_id.into(),
            cli_keypair,
            pairing_token: pairing_token.into(),
            connection_salt: Mutex::new(generate_connection_salt()),
            app_peer: Mutex::new(None),
        }
    }

    /// Draws a fresh salt, stores it as the current salt, and returns it.
    ///
    /// This is intentionally not called for ordinary CLI/GUI transport
    /// reconnects: those reconnects preserve the existing `SecureSession` and
    /// sequence counters.
    pub fn rotate_connection_salt(&self) -> [u8; 32] {
        let salt = generate_connection_salt();
        if let Ok(mut guard) = self.connection_salt.lock() {
            *guard = salt;
        }
        salt
    }

    /// Returns the salt sent in CLI `Join` frames. This is the salt the peer
    /// learns via `PeerJoined`, so it must be the one mixed in when deriving
    /// keys from the peer's `PeerJoined`.
    pub fn connection_salt(&self) -> [u8; 32] {
        self.connection_salt
            .lock()
            .map(|guard| *guard)
            .unwrap_or([0_u8; 32])
    }

    pub fn room_id(&self) -> &str {
        &self.room_id
    }

    pub fn cli_private(&self) -> &[u8; 32] {
        self.cli_keypair.private()
    }

    pub fn cli_public(&self) -> [u8; 32] {
        self.cli_keypair.public()
    }

    pub fn accept_peer_joined(&self, frame: &OuterFrame) -> Result<Option<SessionKeys>> {
        let OuterFrame::PeerJoined {
            role,
            device_pubkey,
            pairing_token_proof,
            connection_salt: app_connection_salt,
        } = frame
        else {
            return Ok(None);
        };

        if *role != Role::App {
            bail!("expected app peer, got {role:?}");
        }
        let Some(proof) = pairing_token_proof else {
            bail!("missing pairing token proof");
        };
        // Mandatory under v3: the salt must be present and authenticated. A
        // relay that strips or rewrites it cannot forge a matching proof, so
        // this rejects the downgrade instead of falling back to an unsalted
        // (nonce-reusing) key derivation.
        let Some(app_salt) = app_connection_salt else {
            bail!(MISSING_CONNECTION_SALT_HELP);
        };
        if !verify_pairing_token_proof(
            &self.pairing_token,
            self.room_id.as_bytes(),
            PairingRole::App,
            device_pubkey,
            app_salt,
            proof,
        ) {
            bail!("invalid pairing token proof");
        }

        if let Ok(mut guard) = self.app_peer.lock() {
            *guard = Some(AppPeerMaterial {
                device_pubkey: *device_pubkey,
                connection_salt: *app_salt,
            });
        }

        let token_hash = pairing_token_hash(&self.pairing_token);
        let cli_salt = self.connection_salt();
        Ok(Some(SessionKeys::derive_for_cli(
            self.room_id.as_bytes(),
            self.cli_keypair.private(),
            *device_pubkey,
            self.cli_keypair.public(),
            *device_pubkey,
            &token_hash,
            &cli_salt,
            app_salt,
        )))
    }

    /// Re-derives session keys binding the current CLI salt to the app's last
    /// authenticated salt. Returns `None` until the app has joined at least
    /// once.
    pub fn rederive_session_keys(&self) -> Option<SessionKeys> {
        let peer = (*self.app_peer.lock().ok()?)?;
        let token_hash = pairing_token_hash(&self.pairing_token);
        let cli_salt = self.connection_salt();
        Some(SessionKeys::derive_for_cli(
            self.room_id.as_bytes(),
            self.cli_keypair.private(),
            peer.device_pubkey,
            self.cli_keypair.public(),
            peer.device_pubkey,
            &token_hash,
            &cli_salt,
            &peer.connection_salt,
        ))
    }
}

#[derive(Debug)]
pub struct AppSecureHandshake {
    material: PairingMaterial,
    app_keypair: KeyPair,
    /// Per-connection salt, rotated on every `join_frame` (i.e. every relay
    /// connection). Mirrors [`CliSecureHandshake::connection_salt`].
    connection_salt: Mutex<[u8; 32]>,
}

impl AppSecureHandshake {
    pub fn new(material: PairingMaterial, app_keypair: KeyPair) -> Self {
        Self {
            material,
            app_keypair,
            connection_salt: Mutex::new(generate_connection_salt()),
        }
    }

    pub fn app_public(&self) -> [u8; 32] {
        self.app_keypair.public()
    }

    pub fn room_id(&self) -> &str {
        &self.material.room_id
    }

    /// Draws a fresh per-connection salt and stores it as the current salt.
    pub fn rotate_connection_salt(&self) -> [u8; 32] {
        let salt = generate_connection_salt();
        if let Ok(mut guard) = self.connection_salt.lock() {
            *guard = salt;
        }
        salt
    }

    /// Returns the salt sent in the most recent `Join`.
    pub fn connection_salt(&self) -> [u8; 32] {
        self.connection_salt
            .lock()
            .map(|guard| *guard)
            .unwrap_or([0_u8; 32])
    }

    pub fn join_frame(&self) -> OuterFrame {
        let app_salt = self.rotate_connection_salt();
        OuterFrame::Join {
            room_id: self.material.room_id.clone(),
            role: Role::App,
            device_pubkey: self.app_keypair.public(),
            pairing_token_proof: Some(pairing_token_proof(
                &self.material.pairing_token,
                self.material.room_id.as_bytes(),
                PairingRole::App,
                &self.app_keypair.public(),
                &app_salt,
            )),
            relay_admission: Some(relay_admission(
                &self.material.pairing_token,
                self.material.room_id.as_bytes(),
            )),
            connection_salt: Some(app_salt),
            supports_join_accepted: false,
        }
    }

    /// Verifies the CLI peer's salt-bound proof from `PeerJoined` and derives
    /// the session keys, binding the CLI's per-connection salt (now
    /// authenticated) together with this connection's own app salt. Returns an
    /// error if the salt or proof is missing or fails to verify, so a relay
    /// cannot downgrade to an unsalted derivation.
    pub fn accept_peer_joined(&self, frame: &OuterFrame) -> Result<Option<SessionKeys>> {
        let OuterFrame::PeerJoined {
            role,
            device_pubkey,
            pairing_token_proof,
            connection_salt: cli_connection_salt,
        } = frame
        else {
            return Ok(None);
        };

        if *role != Role::Cli {
            bail!("expected cli peer, got {role:?}");
        }
        let Some(proof) = pairing_token_proof else {
            bail!("missing pairing token proof");
        };
        let Some(cli_salt) = cli_connection_salt else {
            bail!(MISSING_CONNECTION_SALT_HELP);
        };
        if !verify_pairing_token_proof(
            &self.material.pairing_token,
            self.material.room_id.as_bytes(),
            PairingRole::Cli,
            device_pubkey,
            cli_salt,
            proof,
        ) {
            bail!("invalid pairing token proof");
        }

        Ok(Some(self.session_keys(cli_salt)))
    }

    /// Derives the session keys for the app, mixing in the CLI's per-connection
    /// salt (learned from and authenticated via `PeerJoined`) together with
    /// this connection's own app salt.
    pub fn session_keys(&self, cli_connection_salt: &[u8; 32]) -> SessionKeys {
        let token_hash = pairing_token_hash(&self.material.pairing_token);
        let app_salt = self.connection_salt();
        SessionKeys::derive_for_app(
            self.material.room_id.as_bytes(),
            self.app_keypair.private(),
            self.material.cli_public_key,
            self.material.cli_public_key,
            self.app_keypair.public(),
            &token_hash,
            cli_connection_salt,
            &app_salt,
        )
    }
}

pub fn secure_join_frame(handshake: &CliSecureHandshake) -> OuterFrame {
    let cli_salt = handshake.connection_salt();
    OuterFrame::Join {
        room_id: handshake.room_id().to_string(),
        role: Role::Cli,
        device_pubkey: handshake.cli_public(),
        // The CLI now proves possession of the pairing token and authenticates
        // its own per-connection salt, so the app can reject a relay that
        // tampers with the salt. (Previously the CLI sent no proof and was
        // authenticated only by the app pinning its public key.)
        pairing_token_proof: Some(pairing_token_proof(
            &handshake.pairing_token,
            handshake.room_id().as_bytes(),
            PairingRole::Cli,
            &handshake.cli_public(),
            &cli_salt,
        )),
        relay_admission: Some(relay_admission(
            &handshake.pairing_token,
            handshake.room_id().as_bytes(),
        )),
        connection_salt: Some(cli_salt),
        supports_join_accepted: false,
    }
}

#[derive(Debug, Clone)]
pub struct SecureSession {
    room_id: String,
    keys: SessionKeys,
    next_cli_to_app_seq: u64,
    next_app_to_cli_seq: u64,
    expected_cli_to_app_seq: u64,
    expected_app_to_cli_seq: u64,
    /// Whether at least one frame has been successfully decoded in each
    /// direction. Until then, undecodable frames are treated as stale
    /// leftovers from the previous crypto epoch (see [`Self::decode`]) instead
    /// of fatal errors.
    cli_to_app_synced: bool,
    app_to_cli_synced: bool,
    cli_to_app_stale_discards: u32,
    app_to_cli_stale_discards: u32,
    /// When set, outbound plaintexts are payload-framed and compressed where it
    /// helps. Enabled only after the peer advertises `Compression` via Hello, so
    /// a peer that cannot decode framing keeps receiving raw msgpack.
    compress_outbound: bool,
}

impl SecureSession {
    /// Upper bound on pre-sync discards so genuinely mismatched keys still
    /// surface an error instead of silently eating the stream forever.
    /// Mirrors the iOS/Android `SecureSession.maxStaleFrameDiscards`.
    pub const MAX_STALE_FRAME_DISCARDS: u32 = 128;

    pub fn new(room_id: impl Into<String>, keys: SessionKeys) -> Self {
        Self::new_with_sequences(room_id, keys, 1, 1, 1, 1)
    }

    pub fn new_with_sequences(
        room_id: impl Into<String>,
        keys: SessionKeys,
        next_cli_to_app_seq: u64,
        next_app_to_cli_seq: u64,
        expected_cli_to_app_seq: u64,
        expected_app_to_cli_seq: u64,
    ) -> Self {
        Self {
            room_id: room_id.into(),
            keys,
            next_cli_to_app_seq,
            next_app_to_cli_seq,
            expected_cli_to_app_seq,
            expected_app_to_cli_seq,
            cli_to_app_synced: expected_cli_to_app_seq > 1,
            app_to_cli_synced: expected_app_to_cli_seq > 1,
            cli_to_app_stale_discards: 0,
            app_to_cli_stale_discards: 0,
            compress_outbound: false,
        }
    }

    /// Enable or disable payload compression on this session's outbound frames.
    pub fn set_compress_outbound(&mut self, enabled: bool) {
        self.compress_outbound = enabled;
    }

    pub fn encode(&mut self, direction: Direction, msg: PlainMsg) -> Result<OuterFrame> {
        let seq = match direction {
            Direction::CliToApp => self.next_cli_to_app_seq,
            Direction::AppToCli => self.next_app_to_cli_seq,
        };
        let next = seq.checked_add(1).context("sequence number overflow")?;

        let frame = encode_secure_data(
            &self.room_id,
            direction,
            seq,
            &self.keys,
            msg,
            self.compress_outbound,
        )?;
        // Consume the seq only after successful encryption: if encoding fails,
        // the next call reuses it and the outbound stream stays gap-free.
        // Mirrors the iOS/Android `SecureSession.encode`.
        match direction {
            Direction::CliToApp => self.next_cli_to_app_seq = next,
            Direction::AppToCli => self.next_app_to_cli_seq = next,
        }
        Ok(frame)
    }

    /// Encode one secure data message to its final WebSocket bytes while
    /// enforcing the relay's outer-frame limit before encryption. An oversized
    /// candidate does not consume a sequence number and never creates a
    /// ciphertext under that nonce.
    pub fn encode_wire(
        &mut self,
        direction: Direction,
        msg: PlainMsg,
        max_frame_bytes: usize,
    ) -> Result<Vec<u8>> {
        let seq = match direction {
            Direction::CliToApp => self.next_cli_to_app_seq,
            Direction::AppToCli => self.next_app_to_cli_seq,
        };
        let next = seq.checked_add(1).context("sequence number overflow")?;
        let crypto_direction = crypto_direction(direction);
        let nonce = nonce_for(crypto_direction, seq);
        let plaintext = encode_outbound_plaintext(&msg, self.compress_outbound)?;
        let ciphertext_len = plaintext
            .len()
            .checked_add(CHACHA20_POLY1305_TAG_BYTES)
            .context("secure ciphertext length overflow")?;
        let predicted_len =
            outer_data_frame_encoded_len(&self.room_id, direction, seq, nonce, ciphertext_len);
        ensure!(
            predicted_len <= max_frame_bytes,
            "encoded relay frame exceeds {max_frame_bytes} bytes: {predicted_len}"
        );

        let ciphertext = encrypt(
            &self.keys,
            crypto_direction,
            self.room_id.as_bytes(),
            seq,
            plain_msg_type(&msg),
            &plaintext,
        )
        .context("failed to encrypt PlainMsg")?;
        let frame = OuterFrame::Data {
            room_id: self.room_id.clone(),
            direction,
            seq,
            nonce,
            ciphertext,
        };
        // Encryption has now occurred under this nonce. Consume the sequence
        // even if the infallible-in-practice Vec serialization below reports
        // an internal error, so a retry can never encrypt different plaintext
        // with the same key/nonce pair.
        match direction {
            Direction::CliToApp => self.next_cli_to_app_seq = next,
            Direction::AppToCli => self.next_app_to_cli_seq = next,
        }
        let encoded = encode_frame(&frame).context("failed to encode secure relay frame")?;
        ensure!(
            encoded.len() == predicted_len,
            "secure relay frame length prediction mismatch: predicted {predicted_len}, encoded {}",
            encoded.len()
        );
        Ok(encoded)
    }

    pub fn decode(&mut self, frame: &OuterFrame) -> Result<Option<PlainMsg>> {
        let OuterFrame::Data { direction, seq, .. } = frame else {
            return Ok(None);
        };

        let (expected_seq, synced, stale_discards) = match direction {
            Direction::CliToApp => (
                &mut self.expected_cli_to_app_seq,
                &mut self.cli_to_app_synced,
                &mut self.cli_to_app_stale_discards,
            ),
            Direction::AppToCli => (
                &mut self.expected_app_to_cli_seq,
                &mut self.app_to_cli_synced,
                &mut self.app_to_cli_stale_discards,
            ),
        };
        if *seq < *expected_seq {
            return discard_stale_frame_or_throw(
                *synced,
                stale_discards,
                anyhow::anyhow!(
                    "unexpected secure data sequence: got {}, expected {}",
                    seq,
                    expected_seq
                ),
            );
        }

        // A forward seq gap is tolerated: relay queues can drop an
        // intermediate frame, but a later authenticated frame can still be
        // decrypted independently using its own seq/nonce.
        match decode_secure_data(frame, &self.keys) {
            Ok(msg) => {
                *expected_seq = seq
                    .checked_add(1)
                    .context("secure data sequence number overflow")?;
                *synced = true;
                Ok(msg)
            }
            // Handles an undecodable frame. Before the first successful decode
            // in a direction, such frames are almost always stale leftovers
            // from the previous crypto epoch: on a reconnect the peer keeps
            // encoding under the old keys/counters until it processes the
            // rejoin, and the relay forwards those in-flight frames to us.
            // Discarding them (bounded) lets the fresh stream start cleanly
            // instead of tearing the connection down in a reconnect loop. Once
            // synced, any mismatch is a real desync and errors. Mirrors the
            // iOS/Android `discardStaleFrameOrThrow`.
            Err(err) => discard_stale_frame_or_throw(*synced, stale_discards, err),
        }
    }
}

fn discard_stale_frame_or_throw(
    synced: bool,
    stale_discards: &mut u32,
    error: anyhow::Error,
) -> Result<Option<PlainMsg>> {
    if synced || *stale_discards >= SecureSession::MAX_STALE_FRAME_DISCARDS {
        return Err(error);
    }
    *stale_discards += 1;
    Ok(None)
}

pub fn encode_secure_data(
    room_id: impl Into<String>,
    direction: Direction,
    seq: u64,
    keys: &SessionKeys,
    msg: PlainMsg,
    compress: bool,
) -> Result<OuterFrame> {
    let room_id = room_id.into();
    let crypto_direction = crypto_direction(direction);
    let plaintext = encode_outbound_plaintext(&msg, compress)?;
    let ciphertext = encrypt(
        keys,
        crypto_direction,
        room_id.as_bytes(),
        seq,
        plain_msg_type(&msg),
        &plaintext,
    )
    .context("failed to encrypt PlainMsg")?;

    Ok(OuterFrame::Data {
        room_id,
        direction,
        seq,
        nonce: nonce_for(crypto_direction, seq),
        ciphertext,
    })
}

fn encode_outbound_plaintext(msg: &PlainMsg, compress: bool) -> Result<Vec<u8>> {
    let encoded = encode_plain_msg(msg).context("failed to encode PlainMsg")?;
    // Frame the plaintext only when the peer negotiated compression; otherwise
    // emit bare msgpack so older peers keep decoding it.
    Ok(if compress {
        frame_payload(&encoded, true)
    } else {
        encoded
    })
}

pub fn decode_secure_data(frame: &OuterFrame, keys: &SessionKeys) -> Result<Option<PlainMsg>> {
    let OuterFrame::Data {
        room_id,
        direction,
        seq,
        nonce,
        ciphertext,
    } = frame
    else {
        return Ok(None);
    };

    let crypto_direction = crypto_direction(*direction);
    if *nonce != nonce_for(crypto_direction, *seq) {
        bail!("secure data nonce does not match direction and sequence");
    }

    for msg_type in plain_msg_types() {
        if let Ok(plaintext) = decrypt(
            keys,
            crypto_direction,
            room_id.as_bytes(),
            *seq,
            msg_type,
            ciphertext,
        ) {
            // Payload framing is self-describing: a legacy (unframed) plaintext
            // is returned unchanged, a framed one is unwrapped/decompressed.
            let unframed = unframe_payload(&plaintext)
                .map_err(|e| anyhow::anyhow!("failed to decompress PlainMsg payload: {e}"))?;
            let msg = decode_plain_msg(&unframed).context("failed to decode PlainMsg")?;
            if plain_msg_type(&msg) == *msg_type {
                return Ok(Some(msg));
            }
        }
    }

    bail!("failed to decrypt secure data frame")
}

pub fn crypto_direction(direction: Direction) -> CryptoDirection {
    match direction {
        Direction::CliToApp => CryptoDirection::CliToApp,
        Direction::AppToCli => CryptoDirection::AppToCli,
    }
}
