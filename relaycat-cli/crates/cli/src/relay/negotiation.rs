use super::*;

/// Capabilities this CLI implements. Sent back (intersected with the peer's) in
/// the HelloAck so the app knows what it can rely on, most importantly whether
/// payload `Compression` is mutually supported.
pub(crate) fn cli_protocol_capabilities() -> Vec<ProtocolCapabilityV2> {
    vec![
        ProtocolCapabilityV2::TerminalState,
        ProtocolCapabilityV2::SnapshotRecovery,
        ProtocolCapabilityV2::ExactlyOnceInput,
        ProtocolCapabilityV2::IncrementalScrollback,
        ProtocolCapabilityV2::TerminalTranscript,
        ProtocolCapabilityV2::CliMetadata,
        ProtocolCapabilityV2::Compression,
        ProtocolCapabilityV2::IncrementalAttrs,
    ]
}

pub(crate) fn negotiate_capabilities(peer: &[ProtocolCapabilityV2]) -> Vec<ProtocolCapabilityV2> {
    cli_protocol_capabilities()
        .into_iter()
        .filter(|capability| peer.contains(capability))
        .collect()
}

pub(crate) fn negotiated_protocol_version(peer_versions: &[u16]) -> u16 {
    peer_versions
        .iter()
        .copied()
        .filter(|version| *version <= TERMINAL_STATE_PROTOCOL_V2)
        .max()
        .unwrap_or(TERMINAL_STATE_PROTOCOL_V2)
}

pub(crate) fn hello_ack_for(hello: &HelloV2) -> HelloAckV2 {
    HelloAckV2 {
        selected_protocol_version: negotiated_protocol_version(&hello.protocol_versions),
        capabilities: negotiate_capabilities(&hello.capabilities),
    }
}

