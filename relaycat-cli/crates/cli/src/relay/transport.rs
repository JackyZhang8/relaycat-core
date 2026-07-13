use super::*;

const RELAY_DEFAULT_INBOUND_BYTES_PER_SECOND: usize = 2 * 1024 * 1024;

#[derive(Debug)]
pub(crate) struct RelayOutboundBytePacer {
    byte_tokens: f64,
    last_refill: Instant,
}

impl RelayOutboundBytePacer {
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            byte_tokens: RELAY_DEFAULT_INBOUND_BYTES_PER_SECOND as f64,
            last_refill: now,
        }
    }

    pub(crate) fn delay_for(&mut self, bytes: usize, now: Instant) -> Option<Duration> {
        let rate = RELAY_DEFAULT_INBOUND_BYTES_PER_SECOND as f64;
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.last_refill = now;
        self.byte_tokens = (self.byte_tokens + elapsed * rate).min(rate);

        if self.byte_tokens >= bytes as f64 {
            self.byte_tokens -= bytes as f64;
            return None;
        }
        Some(Duration::from_secs_f64(
            (bytes as f64 - self.byte_tokens) / rate,
        ))
    }

    pub(crate) async fn wait_for(&mut self, bytes: usize) {
        while let Some(delay) = self.delay_for(bytes, Instant::now()) {
            tokio::time::sleep(delay).await;
        }
    }
}

/// Send a WebSocket message with a hard per-send timeout.
///
/// A "zombie" TCP connection (no RST, no FIN, buffer full) will cause
/// `SinkExt::send` to block forever.  The timeout makes this failure loud and
/// fast so the caller can clean up instead of hanging indefinitely.
pub(crate) async fn ws_send(ws_writer: &mut WsWriter, msg: Message) -> Result<()> {
    tokio::time::timeout(WS_SEND_TIMEOUT, ws_writer.send(msg))
        .await
        .context("WebSocket send timed out")?
        .context("WebSocket send failed")
}

pub(crate) async fn reconnect_relay_transport(
    reconnect: &RelayTransportReconnect,
    writer_update_tx: &mpsc::UnboundedSender<WsWriter>,
) -> Result<WsReader> {
    let mut attempt = 0_u32;
    loop {
        // Fire the first attempt immediately. This reconnect is always driven
        // by an explicit event (re-entering RemoteMode via Ctrl-G, or a dropped
        // transport), so there is no reason to wait before trying once; the 1s
        // backoff that used to precede the first attempt was pure latency on
        // the mode switch. Only back off *between* retries.
        if attempt > 0 {
            tokio::time::sleep(RelayTransportReconnectPolicy::retry_delay(attempt)).await;
        }
        match reconnect.connect().await {
            Ok((writer, reader)) => {
                writer_update_tx
                    .send(writer)
                    .map_err(|_| anyhow::anyhow!("relay writer update channel closed"))?;
                relaycat_log("INFO", "reconnected to relay");
                return Ok(reader);
            }
            Err(err) => {
                attempt = attempt.saturating_add(1);
                relaycat_log(
                    "WARN",
                    format!("relay reconnect attempt {attempt} failed: {err:#}"),
                );
            }
        }
    }
}

pub(crate) fn drain_reconnect_signals(rx: &mut mpsc::UnboundedReceiver<()>) {
    while rx.try_recv().is_ok() {}
}

#[derive(Debug)]
pub(crate) struct RelayTransportReconnectPolicy;

impl RelayTransportReconnectPolicy {
    pub(crate) const MAX_DELAY: Duration = Duration::from_secs(30);

    pub(crate) fn retry_delay(attempt: u32) -> Duration {
        let exponent = attempt.saturating_sub(1).min(5);
        Duration::from_secs(1 << exponent).min(Self::MAX_DELAY)
    }
}

pub(crate) fn relay_websocket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(MAX_OUTER_FRAME_BYTES))
        .max_frame_size(Some(MAX_OUTER_FRAME_BYTES))
}

pub(crate) fn decode_relay_binary_frame(bytes: &[u8]) -> Result<OuterFrame> {
    anyhow::ensure!(
        bytes.len() <= MAX_OUTER_FRAME_BYTES,
        "relay binary frame exceeds {} bytes: {}",
        MAX_OUTER_FRAME_BYTES,
        bytes.len()
    );
    decode_frame(bytes).context("failed to decode relay frame")
}

#[derive(Debug)]
pub(crate) enum RelayTransportReconnect {
    Secure {
        ws_url: String,
        handshake: Arc<CliSecureHandshake>,
    },
    Plaintext {
        ws_url: String,
        room_id: String,
    },
}

impl RelayTransportReconnect {
    pub(crate) async fn connect(&self) -> Result<(WsWriter, WsReader)> {
        match self {
            Self::Secure { ws_url, handshake } => {
                let (socket, _) = tokio::time::timeout(
                    RELAY_CONNECT_TIMEOUT,
                    connect_async_with_config(ws_url, Some(relay_websocket_config()), false),
                )
                .await
                .context("relay connection timed out")?
                .with_context(|| format!("failed to connect relay {ws_url}"))?;
                let (mut ws_writer, ws_reader) = socket.split();
                ws_send(
                    &mut ws_writer,
                    Message::Binary(encode_frame(&secure_join_frame(handshake))?.into()),
                )
                .await
                .context("failed to send secure join frame")?;
                Ok((ws_writer, ws_reader))
            }
            Self::Plaintext { ws_url, room_id } => {
                let (socket, _) = tokio::time::timeout(
                    RELAY_CONNECT_TIMEOUT,
                    connect_async_with_config(ws_url, Some(relay_websocket_config()), false),
                )
                .await
                .context("relay connection timed out")?
                .with_context(|| format!("failed to connect relay {ws_url}"))?;
                let (mut ws_writer, ws_reader) = socket.split();
                let join = OuterFrame::Join {
                    room_id: room_id.clone(),
                    role: Role::Cli,
                    device_pubkey: [0; 32],
                    pairing_token_proof: None,
                    relay_admission: None,
                    // Plaintext dev relay carries no end-to-end encryption, so
                    // there is no key derivation to salt.
                    connection_salt: None,
                    supports_join_accepted: false,
                    app_join_intent: relaycat_protocol::AppJoinIntent::Takeover,
                };
                ws_send(&mut ws_writer, Message::Binary(encode_frame(&join)?.into()))
                    .await
                    .context("failed to send join frame")?;
                Ok((ws_writer, ws_reader))
            }
        }
    }
}
