use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use image::Luma;
use qrcode::{EcLevel, QrCode, types::Color};
use rand_core::{OsRng, RngCore};
use relaycat_crypto::KeyPair;
use std::path::Path;

use crate::command::SessionKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingMaterial {
    pub relay_url: String,
    pub room_id: String,
    pub cli_public_key: [u8; 32],
    pub pairing_token: Vec<u8>,
    pub session_kind: SessionKind,
}

pub fn generate_pairing_material(relay_url: impl Into<String>) -> PairingMaterial {
    let keypair = KeyPair::generate();
    generate_pairing_material_for_public_key(relay_url, keypair.public())
}

pub fn generate_pairing_material_for_public_key(
    relay_url: impl Into<String>,
    cli_public_key: [u8; 32],
) -> PairingMaterial {
    generate_pairing_material_for_public_key_and_kind(
        relay_url,
        cli_public_key,
        SessionKind::shell(),
    )
}

pub fn generate_pairing_material_for_public_key_and_kind(
    relay_url: impl Into<String>,
    cli_public_key: [u8; 32],
    session_kind: SessionKind,
) -> PairingMaterial {
    let mut room_bytes = [0_u8; 12];
    let mut pairing_token = vec![0_u8; 16];
    OsRng.fill_bytes(&mut room_bytes);
    OsRng.fill_bytes(&mut pairing_token);

    PairingMaterial {
        relay_url: relay_url.into(),
        room_id: URL_SAFE_NO_PAD.encode(room_bytes),
        cli_public_key,
        pairing_token,
        session_kind,
    }
}

pub fn pairing_url(material: &PairingMaterial) -> String {
    format!(
        "relaycat://pair?relay={}&room={}&pubkey={}&token={}&kind={}",
        percent_encode_query_value(&material.relay_url),
        percent_encode_query_value(&material.room_id),
        URL_SAFE_NO_PAD.encode(material.cli_public_key),
        URL_SAFE_NO_PAD.encode(&material.pairing_token),
        material.session_kind.as_str()
    )
}

pub fn parse_pairing_url(url: &str) -> Result<PairingMaterial> {
    let Some(query) = url.strip_prefix("relaycat://pair?") else {
        bail!("pairing URL must start with relaycat://pair?");
    };

    let mut relay_url = None;
    let mut room_id = None;
    let mut cli_public_key = None;
    let mut pairing_token = None;
    let mut session_kind = None;

    for part in query.split('&') {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        match key {
            "relay" => relay_url = Some(percent_decode_query_value(value)?),
            "room" => room_id = Some(percent_decode_query_value(value)?),
            "pubkey" => cli_public_key = Some(decode_32("pubkey", value)?),
            "token" => pairing_token = Some(decode_token(value)?),
            "kind" => session_kind = Some(decode_session_kind(value)?),
            _ => {}
        }
    }

    Ok(PairingMaterial {
        relay_url: relay_url.context("pairing URL missing relay")?,
        room_id: room_id.context("pairing URL missing room")?,
        cli_public_key: cli_public_key.context("pairing URL missing pubkey")?,
        pairing_token: pairing_token.context("pairing URL missing token")?,
        session_kind: session_kind.unwrap_or_default(),
    })
}

pub fn render_pairing_qr(material: &PairingMaterial) -> Result<String> {
    let url = pairing_url(material);

    #[cfg(not(windows))]
    {
        qr2term::generate_qr_string(url.as_bytes()).context("failed to build pairing QR code")
    }

    #[cfg(windows)]
    {
        let code = QrCode::with_error_correction_level(url.as_bytes(), EcLevel::M)
            .context("failed to build pairing QR code")?;

        Ok(render_qr_code_with_white_background(&code))
    }
}

pub fn render_pairing_qr_with_white_background(material: &PairingMaterial) -> Result<String> {
    let url = pairing_url(material);
    let code = QrCode::with_error_correction_level(url.as_bytes(), EcLevel::M)
        .context("failed to build pairing QR code")?;
    Ok(render_qr_code_with_white_background(&code))
}

fn render_qr_code_with_white_background(code: &QrCode) -> String {
    const QUIET_ZONE_MODULES: usize = 4;

    let qr_width = code.width();
    let total_width = qr_width + QUIET_ZONE_MODULES * 2;
    let mut rendered = String::new();

    for y in 0..total_width {
        for x in 0..total_width {
            if qr_module_is_dark(code, x, y, qr_width) {
                rendered.push_str("\x1b[40m  ");
            } else {
                rendered.push_str("\x1b[47m  ");
            }
        }
        rendered.push_str("\x1b[0m\n");
    }

    rendered
}

fn qr_module_is_dark(code: &QrCode, x: usize, y: usize, qr_width: usize) -> bool {
    const QUIET_ZONE_MODULES: usize = 4;

    let Some(qr_x) = x.checked_sub(QUIET_ZONE_MODULES) else {
        return false;
    };
    let Some(qr_y) = y.checked_sub(QUIET_ZONE_MODULES) else {
        return false;
    };
    qr_x < qr_width && qr_y < qr_width && code[(qr_x, qr_y)] == Color::Dark
}

pub fn write_pairing_qr_png(material: &PairingMaterial, path: &Path) -> Result<()> {
    let url = pairing_url(material);
    let code = QrCode::with_error_correction_level(url.as_bytes(), EcLevel::M)
        .context("failed to build pairing QR code")?;
    let image = code
        .render::<Luma<u8>>()
        .quiet_zone(true)
        .min_dimensions(512, 512)
        .dark_color(Luma([0]))
        .light_color(Luma([255]))
        .build();

    image
        .save(path)
        .with_context(|| format!("failed to write pairing QR PNG {}", path.display()))
}

fn percent_encode_query_value(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

fn percent_decode_query_value(value: &str) -> Result<String> {
    let mut decoded = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3])
                    .context("invalid percent encoding")?;
                let byte = u8::from_str_radix(hex, 16).context("invalid percent encoding")?;
                decoded.push(byte);
                index += 3;
            }
            b'%' => bail!("invalid percent encoding"),
            b'+' => {
                decoded.push(b' ');
                index += 1;
            }
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }

    String::from_utf8(decoded).context("query value is not utf-8")
}

fn decode_32(field: &str, value: &str) -> Result<[u8; 32]> {
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
    SessionKind::new(percent_decode_query_value(value)?)
}
