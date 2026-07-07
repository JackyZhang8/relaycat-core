use chacha20poly1305::{
    ChaCha20Poly1305, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use curve25519_dalek::montgomery::MontgomeryPoint;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand_core::{OsRng, RngCore};
use sha2::Sha256;
use thiserror::Error;

// v3 binds the per-connection salt into both the pairing-token proof and the
// session-key derivation and makes the salt mandatory (no salt-free fallback),
// closing the downgrade in which a malicious relay strips the salt to force
// nonce reuse across reconnects. The version is a domain separator across the
// proof, relay-admission, AEAD AAD and key-derivation contexts, so a v3 peer
// can never interoperate with (or be downgraded to) a v2 peer.
const PROTOCOL_VERSION: &[u8] = b"relaycat-v3";
const CLI_TO_APP_NONCE_PREFIX: [u8; 4] = *b"RCCI";
const APP_TO_CLI_NONCE_PREFIX: [u8; 4] = *b"RCIC";

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    CliToApp,
    AppToCli,
}

impl Direction {
    fn nonce_prefix(self) -> [u8; 4] {
        match self {
            Self::CliToApp => CLI_TO_APP_NONCE_PREFIX,
            Self::AppToCli => APP_TO_CLI_NONCE_PREFIX,
        }
    }

    fn aad_label(self) -> &'static [u8] {
        match self {
            Self::CliToApp => b"cli_to_app",
            Self::AppToCli => b"app_to_cli",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingRole {
    Cli,
    App,
}

impl PairingRole {
    fn label(self) -> &'static [u8] {
        match self {
            Self::Cli => b"cli",
            Self::App => b"app",
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CryptoError {
    #[error("invalid key material")]
    InvalidKey,
    #[error("encryption failed")]
    Encrypt,
    #[error("decryption failed")]
    Decrypt,
}

#[derive(Debug)]
pub struct KeyPair {
    private: [u8; 32],
    public: [u8; 32],
}

impl KeyPair {
    pub fn generate() -> Self {
        let mut private = [0_u8; 32];
        OsRng.fill_bytes(&mut private);
        Self::from_private_bytes(private)
    }

    pub fn from_private_bytes(private: [u8; 32]) -> Self {
        let public = MontgomeryPoint::mul_base_clamped(private).to_bytes();
        Self { private, public }
    }

    pub fn private(&self) -> &[u8; 32] {
        &self.private
    }

    pub fn public(&self) -> [u8; 32] {
        self.public
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionKeys {
    cli_to_app: [u8; 32],
    app_to_cli: [u8; 32],
}

impl SessionKeys {
    #[allow(clippy::too_many_arguments)]
    pub fn derive_for_cli(
        room_id: &[u8],
        private: &[u8; 32],
        peer_public: [u8; 32],
        cli_public: [u8; 32],
        app_public: [u8; 32],
        pairing_token_hash: &[u8; 32],
        cli_connection_salt: &[u8; 32],
        app_connection_salt: &[u8; 32],
    ) -> Self {
        derive_session_keys(
            room_id,
            private,
            peer_public,
            cli_public,
            app_public,
            pairing_token_hash,
            cli_connection_salt,
            app_connection_salt,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn derive_for_app(
        room_id: &[u8],
        private: &[u8; 32],
        peer_public: [u8; 32],
        cli_public: [u8; 32],
        app_public: [u8; 32],
        pairing_token_hash: &[u8; 32],
        cli_connection_salt: &[u8; 32],
        app_connection_salt: &[u8; 32],
    ) -> Self {
        derive_session_keys(
            room_id,
            private,
            peer_public,
            cli_public,
            app_public,
            pairing_token_hash,
            cli_connection_salt,
            app_connection_salt,
        )
    }

    pub fn cli_to_app_key(&self) -> [u8; 32] {
        self.cli_to_app
    }

    pub fn app_to_cli_key(&self) -> [u8; 32] {
        self.app_to_cli
    }

    fn key_for(&self, direction: Direction) -> &[u8; 32] {
        match direction {
            Direction::CliToApp => &self.cli_to_app,
            Direction::AppToCli => &self.app_to_cli,
        }
    }
}

pub fn nonce_for(direction: Direction, seq: u64) -> [u8; 12] {
    let mut nonce = [0_u8; 12];
    nonce[..4].copy_from_slice(&direction.nonce_prefix());
    nonce[4..].copy_from_slice(&seq.to_be_bytes());
    nonce
}

pub fn encrypt(
    keys: &SessionKeys,
    direction: Direction,
    room_id: &[u8],
    seq: u64,
    plain_msg_type: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let cipher = ChaCha20Poly1305::new_from_slice(keys.key_for(direction))
        .map_err(|_| CryptoError::InvalidKey)?;
    let nonce = nonce_for(direction, seq);
    let aad = aad(room_id, direction, seq, plain_msg_type);

    cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: &aad,
            },
        )
        .map_err(|_| CryptoError::Encrypt)
}

pub fn decrypt(
    keys: &SessionKeys,
    direction: Direction,
    room_id: &[u8],
    seq: u64,
    plain_msg_type: &[u8],
    ciphertext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let cipher = ChaCha20Poly1305::new_from_slice(keys.key_for(direction))
        .map_err(|_| CryptoError::InvalidKey)?;
    let nonce = nonce_for(direction, seq);
    let aad = aad(room_id, direction, seq, plain_msg_type);

    cipher
        .decrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: ciphertext,
                aad: &aad,
            },
        )
        .map_err(|_| CryptoError::Decrypt)
}

/// HMAC proof that the sender holds the pairing token and authorizes joining
/// `room_id` as `role` with the given device key **and** per-connection salt.
///
/// Binding `connection_salt` here is what defeats a malicious relay stripping or
/// substituting the salt: the receiver recomputes the proof over the salt it was
/// handed, so any tampering invalidates the proof and the handshake is rejected
/// instead of silently falling back to an unsalted (nonce-reusing) derivation.
pub fn pairing_token_proof(
    pairing_token: &[u8],
    room_id: &[u8],
    role: PairingRole,
    device_pubkey: &[u8; 32],
    connection_salt: &[u8; 32],
) -> [u8; 32] {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(pairing_token)
        .expect("HMAC accepts keys of any length");
    mac.update(PROTOCOL_VERSION);
    mac.update(&(room_id.len() as u32).to_be_bytes());
    mac.update(room_id);
    mac.update(role.label());
    mac.update(device_pubkey);
    mac.update(connection_salt);

    mac.finalize().into_bytes().into()
}

pub fn verify_pairing_token_proof(
    pairing_token: &[u8],
    room_id: &[u8],
    role: PairingRole,
    device_pubkey: &[u8; 32],
    connection_salt: &[u8; 32],
    proof: &[u8; 32],
) -> bool {
    let expected =
        pairing_token_proof(pairing_token, room_id, role, device_pubkey, connection_salt);
    constant_time_eq(&expected, proof)
}

pub fn pairing_token_hash(pairing_token: &[u8]) -> [u8; 32] {
    use sha2::Digest;

    Sha256::digest(pairing_token).into()
}

pub fn relay_admission(pairing_token: &[u8], room_id: &[u8]) -> [u8; 32] {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(pairing_token)
        .expect("HMAC accepts keys of any length");
    mac.update(PROTOCOL_VERSION);
    mac.update(b"relay-admission");
    mac.update(&(room_id.len() as u32).to_be_bytes());
    mac.update(room_id);

    mac.finalize().into_bytes().into()
}

/// Generates a fresh 32-byte per-connection salt from the OS CSPRNG.
///
/// A new salt must be drawn on every relay (re)connection and mixed into the
/// session-key derivation; this is what guarantees a distinct key per
/// connection and therefore prevents ChaCha20-Poly1305 nonce reuse when the
/// sequence counters reset to 1 on reconnect.
pub fn generate_connection_salt() -> [u8; 32] {
    let mut salt = [0_u8; 32];
    OsRng.fill_bytes(&mut salt);
    salt
}

#[allow(clippy::too_many_arguments)]
fn derive_session_keys(
    room_id: &[u8],
    private: &[u8; 32],
    peer_public: [u8; 32],
    cli_public: [u8; 32],
    app_public: [u8; 32],
    pairing_token_hash: &[u8; 32],
    cli_connection_salt: &[u8; 32],
    app_connection_salt: &[u8; 32],
) -> SessionKeys {
    let shared_secret = MontgomeryPoint(peer_public)
        .mul_clamped(*private)
        .to_bytes();
    let salt = key_context(
        room_id,
        cli_public,
        app_public,
        pairing_token_hash,
        cli_connection_salt,
        app_connection_salt,
    );
    let hkdf = Hkdf::<Sha256>::new(Some(&salt), &shared_secret);

    let mut cli_to_app = [0_u8; 32];
    let mut app_to_cli = [0_u8; 32];
    hkdf.expand(b"relaycat-v2-session-key cli_to_app", &mut cli_to_app)
        .expect("32-byte HKDF output is valid");
    hkdf.expand(b"relaycat-v2-session-key app_to_cli", &mut app_to_cli)
        .expect("32-byte HKDF output is valid");

    SessionKeys {
        cli_to_app,
        app_to_cli,
    }
}

fn key_context(
    room_id: &[u8],
    cli_public: [u8; 32],
    app_public: [u8; 32],
    pairing_token_hash: &[u8; 32],
    cli_connection_salt: &[u8; 32],
    app_connection_salt: &[u8; 32],
) -> Vec<u8> {
    let mut context = Vec::with_capacity(PROTOCOL_VERSION.len() + room_id.len() + 100);
    context.extend_from_slice(PROTOCOL_VERSION);
    append_len_prefixed(&mut context, room_id);
    context.extend_from_slice(&cli_public);
    context.extend_from_slice(&app_public);
    context.extend_from_slice(pairing_token_hash);
    // Both per-connection salts are always mixed in (mandatory under v3), in a
    // fixed (cli, app) order so both peers reach the same context regardless of
    // which side performs the derivation. A fresh salt per (re)connection makes
    // the derived key unique even though the long-term ECDH material and the
    // sequence counters reset, which is what prevents nonce reuse.
    context.extend_from_slice(b"relaycat-v3-connection-salt");
    context.extend_from_slice(cli_connection_salt);
    context.extend_from_slice(app_connection_salt);
    context
}

fn aad(room_id: &[u8], direction: Direction, seq: u64, plain_msg_type: &[u8]) -> Vec<u8> {
    let mut aad =
        Vec::with_capacity(PROTOCOL_VERSION.len() + room_id.len() + plain_msg_type.len() + 32);
    aad.extend_from_slice(PROTOCOL_VERSION);
    append_len_prefixed(&mut aad, room_id);
    aad.extend_from_slice(direction.aad_label());
    aad.extend_from_slice(&seq.to_be_bytes());
    append_len_prefixed(&mut aad, plain_msg_type);
    aad
}

fn append_len_prefixed(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }

    let mut diff = 0_u8;
    for (a, b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}
