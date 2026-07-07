use anyhow::{Context, Result};
use relaycat_protocol::{Direction, OuterFrame, PlainMsg, decode_plain_msg, encode_plain_msg};

pub fn encode_plaintext_data(
    room_id: impl Into<String>,
    direction: Direction,
    seq: u64,
    msg: PlainMsg,
) -> Result<OuterFrame> {
    Ok(OuterFrame::Data {
        room_id: room_id.into(),
        direction,
        seq,
        nonce: [0; 12],
        ciphertext: encode_plain_msg(&msg).context("failed to encode plaintext PlainMsg")?,
    })
}

pub fn decode_plaintext_data(frame: &OuterFrame) -> Result<Option<PlainMsg>> {
    let OuterFrame::Data { ciphertext, .. } = frame else {
        return Ok(None);
    };

    let msg = decode_plain_msg(ciphertext).context("failed to decode plaintext PlainMsg")?;
    Ok(Some(msg))
}
