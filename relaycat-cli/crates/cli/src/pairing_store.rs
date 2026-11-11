use std::{
    fs,
    io::{self, Write},
    fmt,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use relaycat_crypto::KeyPair;
use serde::{Deserialize, Serialize};

use crate::{command::SessionKind, pairing::PairingMaterial};

pub const RELAYCAT_DIR: &str = ".relaycat";
pub const PAIRING_QR_DIR: &str = "pairing";

#[derive(Clone, PartialEq, Eq)]
pub struct StoredPairingSession {
    pub version: u8,
    pub created_at_unix: u64,
    pub relay_url: String,
    pub room_id: String,
    pub session_kind: SessionKind,
    pub cli_private_key: [u8; 32],
    pub cli_public_key: [u8; 32],
    pub pairing_token: Vec<u8>,
}

impl fmt::Debug for StoredPairingSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredPairingSession")
            .field("version", &self.version)
            .field("created_at_unix", &self.created_at_unix)
            .field("relay_url", &self.relay_url)
            .field("room_id", &self.room_id)
            .field("session_kind", &self.session_kind)
            .field("cli_private_key", &"<redacted>")
            .field("cli_public_key", &self.cli_public_key)
            .field("pairing_token", &"<redacted>")
            .finish()
    }
}

impl StoredPairingSession {
    pub fn new(material: PairingMaterial, cli_private_key: [u8; 32]) -> Self {
        Self::new_at(material, cli_private_key, current_unix_timestamp())
    }

    pub(crate) fn new_at(
        material: PairingMaterial,
        cli_private_key: [u8; 32],
        created_at_unix: u64,
    ) -> Self {
        Self {
            version: 1,
            created_at_unix,
            relay_url: material.relay_url,
            room_id: material.room_id,
            session_kind: material.session_kind,
            cli_private_key,
            cli_public_key: material.cli_public_key,
            pairing_token: material.pairing_token,
        }
    }

    pub fn key_pair(&self) -> KeyPair {
        KeyPair::from_private_bytes(self.cli_private_key)
    }

    pub fn material(&self) -> PairingMaterial {
        PairingMaterial {
            relay_url: self.relay_url.clone(),
            room_id: self.room_id.clone(),
            cli_public_key: self.cli_public_key,
            pairing_token: self.pairing_token.clone(),
            session_kind: self.session_kind.clone(),
        }
    }

    fn encode(&self) -> Result<String> {
        let file = StoredPairingSessionFile {
            version: Some(self.version),
            created_at_unix: Some(self.created_at_unix),
            relay: Some(self.relay_url.clone()),
            room: Some(self.room_id.clone()),
            kind: Some(self.session_kind.as_str().to_string()),
            cli_private_key: Some(URL_SAFE_NO_PAD.encode(self.cli_private_key)),
            cli_public_key: Some(URL_SAFE_NO_PAD.encode(self.cli_public_key)),
            token: Some(URL_SAFE_NO_PAD.encode(&self.pairing_token)),
        };
        let mut encoded =
            serde_json::to_string_pretty(&file).context("failed to encode pairing session")?;
        encoded.push('\n');
        Ok(encoded)
    }
}

#[derive(Serialize, Deserialize)]
struct StoredPairingSessionFile {
    version: Option<u8>,
    created_at_unix: Option<u64>,
    relay: Option<String>,
    room: Option<String>,
    kind: Option<String>,
    cli_private_key: Option<String>,
    cli_public_key: Option<String>,
    token: Option<String>,
}

pub fn project_dir(target_cwd: Option<&Path>) -> Result<PathBuf> {
    match target_cwd {
        Some(path) => Ok(path.to_path_buf()),
        None => std::env::current_dir().context("failed to read current directory"),
    }
}

pub fn session_file_path(project_dir: &Path, kind: &SessionKind) -> PathBuf {
    project_dir
        .join(RELAYCAT_DIR)
        .join(format!("session_{}.json", kind.as_str()))
}

pub fn pairing_qr_dir(project_dir: &Path) -> PathBuf {
    project_dir.join(RELAYCAT_DIR).join(PAIRING_QR_DIR)
}

/// File the GUI relay child writes the pairing URL to so the GUI host can read
/// it reliably. The primary channel is a private OSC printed to the PTY, but on
/// Windows the relay child runs under a ConPTY whose VT parser silently drops
/// unknown OSC sequences, so this file is the dependable cross-platform
/// fallback (one per session kind, mirroring `session_file_path`).
pub fn gui_pairing_url_path(project_dir: &Path, kind: &SessionKind) -> PathBuf {
    gui_pairing_url_path_for(project_dir, kind, None)
}

/// Session-scoped variant of [`gui_pairing_url_path`]: when the GUI host hands
/// the relay child a per-tab session id (see
/// [`crate::gui_bridge::GUI_SESSION_ID_ENV`]), the file name includes it so
/// multiple relay tabs on the same project + tool kind never read each other's
/// pairing URL.
pub fn gui_pairing_url_path_for(
    project_dir: &Path,
    kind: &SessionKind,
    gui_session_id: Option<&str>,
) -> PathBuf {
    let name = match gui_session_id.filter(|id| !id.is_empty()) {
        Some(id) => format!("pairing_url_{}_{}.txt", kind.as_str(), id),
        None => format!("pairing_url_{}.txt", kind.as_str()),
    };
    project_dir.join(RELAYCAT_DIR).join(name)
}

pub fn pairing_qr_png_path(project_dir: &Path, material: &PairingMaterial) -> PathBuf {
    pairing_qr_dir(project_dir).join(format!(
        "{}-{}.png",
        material.session_kind.as_str(),
        material.room_id
    ))
}

pub fn latest_pairing_qr_png_path(project_dir: &Path, kind: &SessionKind) -> PathBuf {
    pairing_qr_dir(project_dir).join(format!("latest-{}.png", kind.as_str()))
}

pub fn write_pairing_qr_pngs(project_dir: &Path, material: &PairingMaterial) -> Result<PathBuf> {
    let path = pairing_qr_png_path(project_dir, material);
    let latest = latest_pairing_qr_png_path(project_dir, &material.session_kind);
    let dir = pairing_qr_dir(project_dir);
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    // The QR encodes the pairing URL, i.e. the token: keep the directory and
    // the PNGs readable by the owner only. Without this a project directory
    // that is shared, synced or backed up leaks a pairing credential.
    restrict_to_owner(&dir, 0o700)?;

    crate::pairing::write_pairing_qr_png(material, &path)?;
    restrict_to_owner(&path, 0o600)?;
    // `fs::copy` also creates with the default umask; pre-create the fixed
    // `latest` name owner-only so the copy inherits the mode.
    crate::private_fs::create_private_file(&latest)?;
    fs::copy(&path, &latest).with_context(|| {
        format!(
            "failed to update latest pairing QR PNG {}",
            latest.display()
        )
    })?;
    restrict_to_owner(&latest, 0o600)?;

    Ok(path)
}

#[cfg(unix)]
fn restrict_to_owner(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .with_context(|| format!("failed to set permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn restrict_to_owner(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

pub fn load_session(path: &Path) -> Result<Option<StoredPairingSession>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(anyhow::Error::from(err))
                .with_context(|| format!("failed to read pairing session {}", path.display()));
        }
    };
    parse_session(&text)
        .with_context(|| format!("failed to parse pairing session {}", path.display()))
}

pub fn save_session(path: &Path, session: &StoredPairingSession) -> Result<()> {
    // The file holds the CLI private key and the pairing token, so it is
    // written through the private atomic writer: never group/world readable,
    // and a crash can no longer leave a truncated (unparseable) session that
    // would force the user to re-pair.
    let encoded = session.encode()?;
    crate::private_fs::write_private_atomic(path, encoded.as_bytes())
        .with_context(|| format!("failed to write pairing session {}", path.display()))
}

pub fn ensure_gitignore(project_dir: &Path) -> Result<()> {
    let path = project_dir.join(".gitignore");
    let entry = ".relaycat/";
    let existing = fs::read_to_string(&path).unwrap_or_default();
    if existing.lines().any(|line| line.trim() == entry) {
        return Ok(());
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("failed to update {}", path.display()))?;
    if !existing.is_empty() && !existing.ends_with('\n') {
        file.write_all(b"\n")?;
    }
    file.write_all(entry.as_bytes())?;
    file.write_all(b"\n")?;
    Ok(())
}

fn parse_session(text: &str) -> Result<Option<StoredPairingSession>> {
    let file: StoredPairingSessionFile =
        serde_json::from_str(text).context("invalid pairing session JSON")?;
    let Some(version) = file.version else {
        return Ok(None);
    };
    if version != 1 {
        bail!("unsupported pairing session version {version}");
    }
    let Some(created_at_unix) = file.created_at_unix else {
        return Ok(None);
    };
    let relay_url = file.relay.context("missing relay")?;
    let room_id = file.room.context("missing room")?;
    let session_kind = decode_session_kind(&file.kind.context("missing kind")?)?;
    let cli_private_key = decode_key(
        "cli_private_key",
        &file.cli_private_key.context("missing cli_private_key")?,
    )?;
    let cli_public_key = decode_key(
        "cli_public_key",
        &file.cli_public_key.context("missing cli_public_key")?,
    )?;
    let pairing_token = decode_token(&file.token.context("missing token")?)?;

    Ok(Some(StoredPairingSession {
        version: 1,
        created_at_unix,
        relay_url,
        room_id,
        session_kind,
        cli_private_key,
        cli_public_key,
        pairing_token,
    }))
}

fn decode_key(field: &str, value: &str) -> Result<[u8; 32]> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .with_context(|| format!("invalid {field} base64"))?;
    bytes
        .try_into()
        .map_err(|bytes: Vec<u8>| anyhow::anyhow!("{field} must be 32 bytes, got {}", bytes.len()))
}

fn decode_token(value: &str) -> Result<Vec<u8>> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .context("invalid token base64")?;
    match bytes.len() {
        16 => Ok(bytes),
        len => bail!("token must be 16 bytes, got {len}"),
    }
}

fn decode_session_kind(value: &str) -> Result<SessionKind> {
    SessionKind::new(value.to_string())
}

pub fn current_unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod debug_redaction_tests {
    use super::*;

    #[test]
    fn debug_redacts_stored_key_and_token() {
        let token = vec![0xab; 16];
        let session = StoredPairingSession::new(
            PairingMaterial {
                relay_url: "wss://example.test".into(),
                room_id: "room".into(),
                cli_public_key: [0xcd; 32],
                pairing_token: token.clone(),
                session_kind: SessionKind::shell(),
            },
            [0xef; 32],
        );
        let debug = format!("{session:?}");
        assert!(!debug.contains(&URL_SAFE_NO_PAD.encode(&token)));
        assert!(debug.contains("<redacted>"));
    }
}
