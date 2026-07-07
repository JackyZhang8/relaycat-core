use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use relaycat_crypto::KeyPair;

use crate::{command::SessionKind, pairing::PairingMaterial};

pub const RELAYCAT_DIR: &str = ".relaycat";
pub const PAIRING_SESSION_TTL_SECS: u64 = 24 * 60 * 60;
pub const PAIRING_QR_DIR: &str = "pairing";

#[derive(Debug, Clone, PartialEq, Eq)]
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

impl StoredPairingSession {
    pub fn new(material: PairingMaterial, cli_private_key: [u8; 32]) -> Self {
        Self {
            version: 1,
            created_at_unix: current_unix_timestamp(),
            relay_url: material.relay_url,
            room_id: material.room_id,
            session_kind: material.session_kind,
            cli_private_key,
            cli_public_key: material.cli_public_key,
            pairing_token: material.pairing_token,
        }
    }

    pub fn is_expired_at(&self, now_unix: u64) -> bool {
        now_unix.saturating_sub(self.created_at_unix) >= PAIRING_SESSION_TTL_SECS
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

    fn encode(&self) -> String {
        format!(
            concat!(
                "{{\n",
                "  \"version\": {},\n",
                "  \"created_at_unix\": {},\n",
                "  \"relay\": \"{}\",\n",
                "  \"room\": \"{}\",\n",
                "  \"kind\": \"{}\",\n",
                "  \"cli_private_key\": \"{}\",\n",
                "  \"cli_public_key\": \"{}\",\n",
                "  \"token\": \"{}\"\n",
                "}}\n"
            ),
            self.version,
            self.created_at_unix,
            escape_json_string(&self.relay_url),
            escape_json_string(&self.room_id),
            self.session_kind.as_str(),
            URL_SAFE_NO_PAD.encode(self.cli_private_key),
            URL_SAFE_NO_PAD.encode(self.cli_public_key),
            URL_SAFE_NO_PAD.encode(&self.pairing_token)
        )
    }
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
    project_dir
        .join(RELAYCAT_DIR)
        .join(format!("pairing_url_{}.txt", kind.as_str()))
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

pub fn cleanup_expired_pairing_qr_pngs(project_dir: &Path, now_unix: u64) -> Result<()> {
    let dir = pairing_qr_dir(project_dir);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(anyhow::Error::from(err))
                .with_context(|| format!("failed to read pairing QR directory {}", dir.display()));
        }
    };

    for entry in entries {
        let entry = entry.with_context(|| format!("failed to read entry in {}", dir.display()))?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("png") {
            continue;
        }
        let metadata = entry
            .metadata()
            .with_context(|| format!("failed to stat pairing QR PNG {}", path.display()))?;
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        let Ok(modified_unix) = modified.duration_since(UNIX_EPOCH) else {
            continue;
        };
        if now_unix.saturating_sub(modified_unix.as_secs()) >= PAIRING_SESSION_TTL_SECS {
            fs::remove_file(&path).with_context(|| {
                format!("failed to remove expired pairing QR PNG {}", path.display())
            })?;
        }
    }
    Ok(())
}

pub fn write_pairing_qr_pngs(project_dir: &Path, material: &PairingMaterial) -> Result<PathBuf> {
    cleanup_expired_pairing_qr_pngs(project_dir, current_unix_timestamp())?;
    let path = pairing_qr_png_path(project_dir, material);
    let latest = latest_pairing_qr_png_path(project_dir, &material.session_kind);
    let dir = pairing_qr_dir(project_dir);
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;

    crate::pairing::write_pairing_qr_png(material, &path)?;
    fs::copy(&path, &latest).with_context(|| {
        format!(
            "failed to update latest pairing QR PNG {}",
            latest.display()
        )
    })?;

    Ok(path)
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
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let mut options = fs::OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("failed to write pairing session {}", path.display()))?;
    file.write_all(session.encode().as_bytes())
        .with_context(|| format!("failed to write pairing session {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to set permissions on {}", path.display()))?;
    }
    Ok(())
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
    let Some(version) = json_number(text, "version") else {
        return Ok(None);
    };
    if version != 1 {
        bail!("unsupported pairing session version {version}");
    }
    let Some(created_at_unix) = json_u64(text, "created_at_unix") else {
        return Ok(None);
    };
    let relay_url = required_json_string(text, "relay")?;
    let room_id = required_json_string(text, "room")?;
    let session_kind = decode_session_kind(&required_json_string(text, "kind")?)?;
    let cli_private_key = decode_key(
        "cli_private_key",
        &required_json_string(text, "cli_private_key")?,
    )?;
    let cli_public_key = decode_key(
        "cli_public_key",
        &required_json_string(text, "cli_public_key")?,
    )?;
    let pairing_token = decode_token(&required_json_string(text, "token")?)?;

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

fn json_number(text: &str, key: &str) -> Option<u8> {
    json_u64(text, key)?.try_into().ok()
}

fn json_u64(text: &str, key: &str) -> Option<u64> {
    let needle = format!("\"{key}\"");
    let (_, rest) = text.split_once(&needle)?;
    let (_, rest) = rest.split_once(':')?;
    let digits: String = rest
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

fn required_json_string(text: &str, key: &str) -> Result<String> {
    json_string(text, key).with_context(|| format!("missing {key}"))
}

fn json_string(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let (_, rest) = text.split_once(&needle)?;
    let (_, rest) = rest.split_once(':')?;
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('"')?;
    let mut value = String::new();
    let mut chars = rest.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => return Some(value),
            '\\' => match chars.next()? {
                '"' => value.push('"'),
                '\\' => value.push('\\'),
                '/' => value.push('/'),
                'n' => value.push('\n'),
                'r' => value.push('\r'),
                't' => value.push('\t'),
                'b' => value.push('\x08'),
                'f' => value.push('\x0C'),
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if hex.len() == 4
                        && let Ok(n) = u32::from_str_radix(&hex, 16)
                        && let Some(c) = char::from_u32(n)
                    {
                        value.push(c);
                        continue;
                    }
                    return None;
                }
                other => value.push(other),
            },
            _ => value.push(ch),
        }
    }
    None
}

fn escape_json_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
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
