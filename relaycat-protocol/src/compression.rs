//! Per-message payload framing used to (optionally) compress a `PlainMsg`
//! plaintext before it is encrypted.
//!
//! WebSocket `permessage-deflate` is useless here because the relay only ever
//! sees AES/ChaCha ciphertext, which does not compress. To get any benefit the
//! terminal text has to be compressed *before* encryption. Each framed payload
//! carries a single leading tag byte so the receiver can tell a raw payload
//! from a compressed one without any side-channel:
//!
//! ```text
//! [0x00][raw msgpack ...]        // identity
//! [0x01][raw DEFLATE stream ...] // compressed (RFC 1951)
//! ```
//!
//! Backward compatibility: a legacy peer emits a bare msgpack `PlainMsg`, which
//! is an externally-tagged enum and therefore always starts with a map marker
//! (`0x80..=0x8f`, `0xde`, `0xdf`). Those bytes are disjoint from the tag bytes
//! above, so [`unframe_payload`] can transparently accept headerless payloads
//! from older clients. Compressed frames are only ever *emitted* once the peer
//! has advertised [`crate::ProtocolCapabilityV2::Compression`], so an old peer
//! never has to decode a tag it does not understand.

use std::borrow::Cow;
use std::io::Write;

use flate2::write::DeflateEncoder;
use flate2::{Compression, Decompress, FlushDecompress, Status};

/// Tag for an uncompressed (raw msgpack) framed payload.
pub const PAYLOAD_FRAME_IDENTITY: u8 = 0x00;
/// Tag for a raw-DEFLATE compressed framed payload.
pub const PAYLOAD_FRAME_DEFLATE: u8 = 0x01;

/// Payloads below this size rarely recoup the ~1 byte header plus the CPU cost,
/// so they are always sent identity-framed (e.g. keystrokes, acks, heartbeats).
pub const PAYLOAD_COMPRESSION_MIN_BYTES: usize = 256;

/// Hard cap on the number of bytes a single framed payload may decompress to.
///
/// DEFLATE has an unbounded expansion ratio, so a ~1 MB ciphertext (the relay's
/// per-frame limit) could otherwise inflate to gigabytes and OOM the receiver.
/// Legitimate plaintexts are bounded well below this (terminal snapshots/patches
/// are budgeted under 1 MB), so the cap only ever trips on a malicious or
/// corrupt frame.
pub const MAX_INFLATED_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;

/// Frame an already-encoded `PlainMsg` plaintext.
///
/// When `allow_compression` is set and raw DEFLATE actually shrinks a payload
/// above [`PAYLOAD_COMPRESSION_MIN_BYTES`], the result is `[DEFLATE][stream]`;
/// otherwise it is `[IDENTITY][plaintext]`.
pub fn frame_payload(plaintext: &[u8], allow_compression: bool) -> Vec<u8> {
    // Both framings add the same 1-byte header, so compression wins iff the
    // DEFLATE stream is strictly smaller than the raw plaintext.
    if allow_compression
        && plaintext.len() >= PAYLOAD_COMPRESSION_MIN_BYTES
        && let Some(compressed) = deflate(plaintext)
        && compressed.len() < plaintext.len()
    {
        let mut out = Vec::with_capacity(compressed.len() + 1);
        out.push(PAYLOAD_FRAME_DEFLATE);
        out.extend_from_slice(&compressed);
        return out;
    }
    let mut out = Vec::with_capacity(plaintext.len() + 1);
    out.push(PAYLOAD_FRAME_IDENTITY);
    out.extend_from_slice(plaintext);
    out
}

/// Reverse [`frame_payload`]. Headerless (legacy) payloads are returned as-is.
pub fn unframe_payload(framed: &[u8]) -> std::io::Result<Cow<'_, [u8]>> {
    match framed.first() {
        Some(&PAYLOAD_FRAME_IDENTITY) => Ok(Cow::Borrowed(&framed[1..])),
        Some(&PAYLOAD_FRAME_DEFLATE) => inflate(&framed[1..]).map(Cow::Owned),
        _ => Ok(Cow::Borrowed(framed)),
    }
}

fn deflate(input: &[u8]) -> Option<Vec<u8>> {
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(input).ok()?;
    encoder.finish().ok()
}

fn inflate(input: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut decoder = Decompress::new(false);
    let mut input_offset = 0;
    let mut out = Vec::with_capacity(input.len().min(MAX_INFLATED_PAYLOAD_BYTES));
    let mut chunk = [0_u8; 8192];

    loop {
        let total_in_before = decoder.total_in();
        let total_out_before = decoder.total_out();
        let flush = if input_offset == input.len() {
            FlushDecompress::Finish
        } else {
            FlushDecompress::None
        };
        let status = decoder
            .decompress(&input[input_offset..], &mut chunk, flush)
            .map_err(|err| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("invalid DEFLATE payload: {err}"),
                )
            })?;

        let consumed = (decoder.total_in() - total_in_before) as usize;
        let produced = (decoder.total_out() - total_out_before) as usize;
        input_offset += consumed;

        if out.len().saturating_add(produced) > MAX_INFLATED_PAYLOAD_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "decompressed payload exceeds maximum allowed size",
            ));
        }
        out.extend_from_slice(&chunk[..produced]);

        if status == Status::StreamEnd {
            if input_offset != input.len() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "trailing bytes after DEFLATE stream",
                ));
            }
            return Ok(out);
        }

        if consumed == 0 && produced == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "truncated DEFLATE stream",
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_roundtrip_small_payload() {
        let payload = b"hi";
        let framed = frame_payload(payload, true);
        assert_eq!(framed[0], PAYLOAD_FRAME_IDENTITY);
        assert_eq!(unframe_payload(&framed).unwrap().as_ref(), payload);
    }

    #[test]
    fn compresses_large_repetitive_payload() {
        let payload = vec![b'a'; 4096];
        let framed = frame_payload(&payload, true);
        assert_eq!(framed[0], PAYLOAD_FRAME_DEFLATE);
        assert!(framed.len() < payload.len());
        assert_eq!(
            unframe_payload(&framed).unwrap().as_ref(),
            payload.as_slice()
        );
    }

    #[test]
    fn never_compresses_when_disallowed() {
        let payload = vec![b'a'; 4096];
        let framed = frame_payload(&payload, false);
        assert_eq!(framed[0], PAYLOAD_FRAME_IDENTITY);
        assert_eq!(
            unframe_payload(&framed).unwrap().as_ref(),
            payload.as_slice()
        );
    }

    #[test]
    fn incompressible_payload_falls_back_to_identity() {
        // High-entropy (xorshift) data: DEFLATE cannot beat identity, so the
        // framing must stay raw rather than emit a larger compressed payload.
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        let payload: Vec<u8> = (0..1024)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect();
        let framed = frame_payload(&payload, true);
        assert_eq!(framed[0], PAYLOAD_FRAME_IDENTITY);
        assert_eq!(
            unframe_payload(&framed).unwrap().as_ref(),
            payload.as_slice()
        );
    }

    #[test]
    fn rejects_decompression_bomb() {
        // A small DEFLATE stream that inflates past the cap must be rejected
        // rather than allocating gigabytes.
        let oversized = vec![0_u8; MAX_INFLATED_PAYLOAD_BYTES + 1024];
        let compressed = deflate(&oversized).expect("deflate");
        assert!(compressed.len() < oversized.len());
        let mut framed = Vec::with_capacity(compressed.len() + 1);
        framed.push(PAYLOAD_FRAME_DEFLATE);
        framed.extend_from_slice(&compressed);
        let err = unframe_payload(&framed).expect_err("bomb must be rejected");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn rejects_empty_deflate_stream() {
        assert!(unframe_payload(&[PAYLOAD_FRAME_DEFLATE]).is_err());
    }

    #[test]
    fn rejects_truncated_deflate_stream() {
        let payload = vec![b'a'; 4096];
        let mut framed = frame_payload(&payload, true);
        assert_eq!(framed[0], PAYLOAD_FRAME_DEFLATE);
        framed.pop();

        assert!(unframe_payload(&framed).is_err());
    }

    #[test]
    fn rejects_trailing_bytes_after_deflate_stream() {
        let payload = vec![b'a'; 4096];
        let mut framed = frame_payload(&payload, true);
        assert_eq!(framed[0], PAYLOAD_FRAME_DEFLATE);
        framed.push(0);

        assert!(unframe_payload(&framed).is_err());
    }

    #[test]
    fn accepts_payload_at_inflated_cap() {
        let at_cap = vec![0_u8; MAX_INFLATED_PAYLOAD_BYTES];
        let framed = frame_payload(&at_cap, true);
        assert_eq!(framed[0], PAYLOAD_FRAME_DEFLATE);
        assert_eq!(
            unframe_payload(&framed).unwrap().as_ref(),
            at_cap.as_slice()
        );
    }

    #[test]
    fn accepts_legacy_headerless_msgpack_map() {
        // A bare msgpack map (fixmap with one entry) as an old peer would send.
        let legacy = [0x81, 0xa5, b'h', b'e', b'l', b'l', b'o'];
        assert_eq!(unframe_payload(&legacy).unwrap().as_ref(), &legacy);
    }
}
