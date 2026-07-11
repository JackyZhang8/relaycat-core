use relaycat_cli::plaintext::{decode_plaintext_data, encode_plaintext_data};
use relaycat_protocol::{Direction, OuterFrame, PlainMsg};

#[test]
fn wraps_plain_msg_in_data_frame_for_dev_relay() {
    let msg = PlainMsg::Heartbeat;

    let frame = encode_plaintext_data("room-1", Direction::CliToApp, 3, msg.clone())
        .expect("encode data frame");

    assert_eq!(
        frame,
        OuterFrame::Data {
            room_id: "room-1".to_string(),
            direction: Direction::CliToApp,
            seq: 3,
            nonce: [0; 12],
            ciphertext: relaycat_protocol::encode_plain_msg(&msg).expect("encode plain msg"),
        }
    );
}

#[test]
fn unwraps_plain_msg_from_data_frame_for_dev_relay() {
    let msg = PlainMsg::InputEventV2(relaycat_protocol::InputEventV2 {
        input_stream_id: "stream-1".to_string(),
        input_seq: 1,
        bytes: b"pwd\r".to_vec(),
    });
    let frame = encode_plaintext_data("room-1", Direction::AppToCli, 8, msg.clone())
        .expect("encode data frame");

    let decoded = decode_plaintext_data(&frame).expect("decode plaintext data");

    assert_eq!(decoded, Some(msg));
}

#[test]
fn ignores_non_data_frames_for_plain_msg_decode() {
    let decoded = decode_plaintext_data(&OuterFrame::Ping).expect("decode ping");

    assert_eq!(decoded, None);
}
