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
        ProtocolCapabilityV2::WorkspaceRpc,
        ProtocolCapabilityV2::TerminalStreams,
    ]
}

/// Capabilities both sides must share for a session to work at all. Terminal
/// state sync and snapshot recovery are the foundation of every other flow, so
/// a peer missing either is rejected instead of degraded.
pub(crate) const MANDATORY_CAPABILITIES: [ProtocolCapabilityV2; 2] = [
    ProtocolCapabilityV2::TerminalState,
    ProtocolCapabilityV2::SnapshotRecovery,
];

pub(crate) fn negotiate_capabilities(peer: &[ProtocolCapabilityV2]) -> Vec<ProtocolCapabilityV2> {
    cli_protocol_capabilities()
        .into_iter()
        .filter(|capability| peer.contains(capability))
        .collect()
}

/// Highest protocol version both peers support, or `None` when the peer only
/// speaks versions this CLI does not (no silent fallback to v2).
pub(crate) fn negotiated_protocol_version(peer_versions: &[u16]) -> Option<u16> {
    peer_versions
        .iter()
        .copied()
        .filter(|version| *version <= TERMINAL_STATE_PROTOCOL_V2)
        .max()
}

pub(crate) fn hello_ack_for(hello: &HelloV2) -> Result<HelloAckV2, ProtocolRejectV2> {
    let Some(selected_protocol_version) = negotiated_protocol_version(&hello.protocol_versions)
    else {
        return Err(ProtocolRejectV2 {
            reason: format!(
                "no shared protocol version (peer offered {:?}, CLI supports up to {}); upgrade the older side",
                hello.protocol_versions, TERMINAL_STATE_PROTOCOL_V2
            ),
            supported_versions: vec![TERMINAL_STATE_PROTOCOL_V2],
        });
    };

    let capabilities = negotiate_capabilities(&hello.capabilities);
    let missing: Vec<&ProtocolCapabilityV2> = MANDATORY_CAPABILITIES
        .iter()
        .filter(|capability| !capabilities.contains(capability))
        .collect();
    if !missing.is_empty() {
        return Err(ProtocolRejectV2 {
            reason: format!(
                "peer is missing mandatory capabilities {missing:?}; upgrade the older side"
            ),
            supported_versions: vec![TERMINAL_STATE_PROTOCOL_V2],
        });
    }

    Ok(HelloAckV2 {
        selected_protocol_version,
        capabilities,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello(versions: Vec<u16>, capabilities: Vec<ProtocolCapabilityV2>) -> HelloV2 {
        HelloV2 {
            protocol_versions: versions,
            capabilities,
        }
    }

    #[test]
    fn negotiates_highest_shared_version() {
        assert_eq!(negotiated_protocol_version(&[1, 2]), Some(2));
        assert_eq!(negotiated_protocol_version(&[1]), Some(1));
    }

    #[test]
    fn rejects_when_no_shared_version() {
        assert_eq!(negotiated_protocol_version(&[3, 4]), None);
        assert_eq!(negotiated_protocol_version(&[]), None);

        let rejected = hello_ack_for(&hello(vec![3], cli_protocol_capabilities()))
            .expect_err("newer-only peer must be rejected");
        assert!(rejected.reason.contains("no shared protocol version"));
        assert_eq!(rejected.supported_versions, vec![TERMINAL_STATE_PROTOCOL_V2]);
    }

    #[test]
    fn rejects_when_terminal_state_capability_missing() {
        let rejected = hello_ack_for(&hello(
            vec![TERMINAL_STATE_PROTOCOL_V2],
            vec![ProtocolCapabilityV2::SnapshotRecovery],
        ))
        .expect_err("peer without terminal_state must be rejected");
        assert!(rejected.reason.contains("mandatory capabilities"));
    }

    #[test]
    fn rejects_when_snapshot_recovery_capability_missing() {
        let rejected = hello_ack_for(&hello(
            vec![TERMINAL_STATE_PROTOCOL_V2],
            vec![ProtocolCapabilityV2::TerminalState],
        ))
        .expect_err("peer without snapshot_recovery must be rejected");
        assert!(rejected.reason.contains("mandatory capabilities"));
    }

    /// P2-2 cross-version compatibility matrix: peers one release apart in
    /// either direction must still negotiate (upgrades are not synchronized
    /// across the three products), and anything else must produce an explicit
    /// ProtocolRejectV2 instead of a silent fallback.
    #[test]
    fn cross_version_negotiation_matrix() {
        let n = TERMINAL_STATE_PROTOCOL_V2;
        let mandatory = MANDATORY_CAPABILITIES.to_vec();

        // Same release (N/N): highest shared version is N.
        let ack = hello_ack_for(&hello(vec![n], mandatory.clone())).expect("N/N peer");
        assert_eq!(ack.selected_protocol_version, n);

        // Peer one release behind (N/N-1): both offer their full ranges; the
        // shared older version is selected rather than rejecting the peer.
        if n > 1 {
            let ack =
                hello_ack_for(&hello(vec![n - 1], mandatory.clone())).expect("N-1 peer");
            assert_eq!(ack.selected_protocol_version, n - 1);
        }

        // Peer one release ahead (N/N+1): a well-behaved newer peer still
        // offers N alongside N+1, so negotiation lands on N.
        let ack = hello_ack_for(&hello(vec![n, n + 1], mandatory.clone())).expect("N+1 peer");
        assert_eq!(ack.selected_protocol_version, n);

        // No overlap (peer only speaks futures versions): explicit reject
        // that names both sides' versions so the user knows what to upgrade.
        let rejected = hello_ack_for(&hello(vec![n + 1, n + 2], mandatory.clone()))
            .expect_err("future-only peer");
        assert!(rejected.reason.contains("no shared protocol version"));
        assert_eq!(rejected.supported_versions, vec![n]);

        // Unknown optional capabilities from a newer peer are already dropped
        // at decode; a peer advertising extras it cannot prove shared simply
        // gets the intersection back, never an error.
        let ack = hello_ack_for(&hello(
            vec![n],
            [
                mandatory.clone(),
                vec![ProtocolCapabilityV2::Compression],
            ]
            .concat(),
        ))
        .expect("peer with extra optional capabilities");
        assert!(ack.capabilities.contains(&ProtocolCapabilityV2::Compression));

        // Missing mandatory capability: explicit reject, not degraded mode.
        let rejected = hello_ack_for(&hello(vec![n], vec![ProtocolCapabilityV2::TerminalState]))
            .expect_err("peer missing snapshot_recovery");
        assert!(rejected.reason.contains("mandatory capabilities"));
    }

    #[test]
    fn acks_compatible_peer_with_intersected_capabilities() {
        let ack = hello_ack_for(&hello(
            vec![TERMINAL_STATE_PROTOCOL_V2],
            vec![
                ProtocolCapabilityV2::TerminalState,
                ProtocolCapabilityV2::SnapshotRecovery,
                ProtocolCapabilityV2::Compression,
            ],
        ))
        .expect("compatible peer");
        assert_eq!(ack.selected_protocol_version, TERMINAL_STATE_PROTOCOL_V2);
        assert!(ack.capabilities.contains(&ProtocolCapabilityV2::Compression));
        assert!(
            !ack.capabilities
                .contains(&ProtocolCapabilityV2::IncrementalAttrs)
        );
    }
}
