use relaycat_protocol::{Direction, OuterFrame, Role};
use relaycat_relay::hub::{AdmissionCheck, CloseSignal, Hub, HubError, JoinRequest};
use std::time::Duration;
use tokio::sync::mpsc;

#[tokio::test]
async fn forwards_frame_from_cli_to_app_peer() {
    let hub = Hub::default();
    let (cli_tx, _cli_rx) = mpsc::channel(64);
    let (app_tx, mut app_rx) = mpsc::channel(64);

    hub.join(JoinRequest::new(
        "room-1",
        Role::Cli,
        1,
        [1; 32],
        None,
        cli_tx,
    ))
    .expect("cli joins");
    hub.join(JoinRequest::new(
        "room-1",
        Role::App,
        2,
        [2; 32],
        Some([9; 32]),
        app_tx,
    ))
    .expect("app joins");

    let frame = OuterFrame::Data {
        room_id: "room-1".to_string(),
        direction: Direction::CliToApp,
        seq: 1,
        nonce: [1; 12],
        ciphertext: vec![1, 2, 3],
    };

    hub.forward("room-1", Role::Cli, 1, frame.clone())
        .expect("forward frame");

    // The joining app now also receives a PeerJoined describing the CLI (so it
    // can learn the CLI's per-connection salt); drain it before the data frame.
    assert!(matches!(
        app_rx.recv().await,
        Some(OuterFrame::PeerJoined {
            role: Role::Cli,
            ..
        })
    ));
    assert_eq!(app_rx.recv().await, Some(frame));
}

#[tokio::test]
async fn forward_without_peer_is_ignored() {
    let hub = Hub::default();
    let (cli_tx, _cli_rx) = mpsc::channel(64);

    hub.join(JoinRequest::new(
        "room-1",
        Role::Cli,
        1,
        [1; 32],
        None,
        cli_tx,
    ))
    .expect("cli joins");

    let frame = OuterFrame::Ping;

    hub.forward("room-1", Role::Cli, 1, frame)
        .expect("missing peer is not fatal");
}

#[tokio::test]
async fn forward_to_full_peer_channel_evicts_slow_peer_for_reconnect() {
    let hub = Hub::default();
    let (cli_tx, mut cli_rx) = mpsc::channel(64);
    let (app_tx, mut app_rx) = mpsc::channel(1);

    hub.join(JoinRequest::new(
        "room-1",
        Role::Cli,
        1,
        [1; 32],
        None,
        cli_tx,
    ))
    .expect("cli joins");
    hub.join(JoinRequest::new(
        "room-1",
        Role::App,
        2,
        [2; 32],
        Some([9; 32]),
        app_tx,
    ))
    .expect("app joins");

    let _ = cli_rx.recv().await;
    // The joining app received a PeerJoined describing the CLI; drain it so the
    // cap-1 channel is empty before we exercise slow-peer eviction.
    let _ = app_rx.recv().await;
    let frame = OuterFrame::Data {
        room_id: "room-1".to_string(),
        direction: Direction::CliToApp,
        seq: 1,
        nonce: [1; 12],
        ciphertext: vec![1, 2, 3],
    };

    hub.forward("room-1", Role::Cli, 1, frame.clone())
        .expect("first forward fills app channel");
    hub.forward("room-1", Role::Cli, 1, frame)
        .expect("slow app eviction is not fatal");

    let stats = hub.stats();
    assert_eq!(stats.app_connected_total, 0);
    assert_eq!(stats.cli_idle, 1);
    assert_eq!(
        cli_rx.recv().await,
        Some(OuterFrame::PeerLeft { role: Role::App })
    );
    assert_eq!(
        app_rx.recv().await,
        Some(OuterFrame::Data {
            room_id: "room-1".to_string(),
            direction: Direction::CliToApp,
            seq: 1,
            nonce: [1; 12],
            ciphertext: vec![1, 2, 3],
        })
    );
    assert_eq!(app_rx.recv().await, None);
    hub.forward(
        "room-1",
        Role::App,
        2,
        OuterFrame::Data {
            room_id: "room-1".to_string(),
            direction: Direction::AppToCli,
            seq: 1,
            nonce: [2; 12],
            ciphertext: vec![4, 5, 6],
        },
    )
    .expect("evicted app transport is ignored after congestion");
    assert!(cli_rx.try_recv().is_err());
}

#[tokio::test]
async fn slow_consumer_eviction_sends_close_signal_and_records_metrics() {
    let hub = Hub::default();
    let (cli_tx, mut cli_rx) = mpsc::channel(64);
    let (app_tx, mut app_rx) = mpsc::channel(1);
    let (app_close_tx, mut app_close_rx) = mpsc::channel(1);

    hub.join(JoinRequest::new(
        "room-1",
        Role::Cli,
        1,
        [1; 32],
        None,
        cli_tx,
    ))
    .expect("cli joins");
    hub.join(
        JoinRequest::new("room-1", Role::App, 2, [2; 32], Some([9; 32]), app_tx)
            .with_close_signal(app_close_tx),
    )
    .expect("app joins");

    let _ = cli_rx.recv().await;
    let _ = app_rx.recv().await;

    let frame = OuterFrame::Data {
        room_id: "room-1".to_string(),
        direction: Direction::CliToApp,
        seq: 1,
        nonce: [1; 12],
        ciphertext: vec![1, 2, 3],
    };
    hub.forward("room-1", Role::Cli, 1, frame.clone())
        .expect("first forward fills app channel");
    hub.forward("room-1", Role::Cli, 1, frame)
        .expect("slow app eviction is not fatal");

    // The eviction is delivered out-of-band even though the ordinary
    // outbound queue is full, so the ws task can close with a reason.
    assert_eq!(app_close_rx.recv().await, Some(CloseSignal::SlowConsumer));
    assert_eq!(
        CloseSignal::SlowConsumer.close_reason(),
        "slow_consumer retryable"
    );

    let stats = hub.stats();
    assert_eq!(stats.slow_consumer_evictions_total, 1);
    assert_eq!(stats.slow_consumer_evictions_app, 1);
    assert_eq!(stats.slow_consumer_evictions_cli, 0);
    assert_eq!(stats.slow_consumer_discarded_bytes, 3);
    assert_eq!(stats.outbound_queue_high_water, 1);
}

#[tokio::test]
async fn app_replacement_sends_evicted_close_signal_even_with_full_queue() {
    let hub = Hub::default();
    let (cli_tx, mut cli_rx) = mpsc::channel(64);
    let (old_app_tx, mut old_app_rx) = mpsc::channel(1);
    let (old_app_close_tx, mut old_app_close_rx) = mpsc::channel(1);
    let (new_app_tx, _new_app_rx) = mpsc::channel(64);

    hub.join(JoinRequest::new(
        "room-1",
        Role::Cli,
        1,
        [1; 32],
        None,
        cli_tx,
    ))
    .expect("cli joins");
    hub.join(
        JoinRequest::new("room-1", Role::App, 2, [2; 32], Some([9; 32]), old_app_tx)
            .with_close_signal(old_app_close_tx),
    )
    .expect("old app joins");

    let _ = cli_rx.recv().await;
    // Leave the old app's PeerJoined queued so its cap-1 channel is full and
    // the ordinary Evicted control frame cannot be delivered.
    assert!(
        hub.join(JoinRequest::new(
            "room-1",
            Role::App,
            3,
            [3; 32],
            Some([9; 32]),
            new_app_tx,
        ))
        .expect("new app evicts old app")
    );

    assert_eq!(old_app_close_rx.recv().await, Some(CloseSignal::Evicted));
    assert_eq!(CloseSignal::Evicted.close_reason(), "evicted");
    // The queued frame is still the original PeerJoined; Evicted never fit.
    assert!(matches!(
        old_app_rx.recv().await,
        Some(OuterFrame::PeerJoined { .. })
    ));
}

#[tokio::test]
async fn cli_replacement_sends_replaced_close_signal() {
    let hub = Hub::default();
    let (old_cli_tx, _old_cli_rx) = mpsc::channel(64);
    let (old_cli_close_tx, mut old_cli_close_rx) = mpsc::channel(1);
    let (new_cli_tx, _new_cli_rx) = mpsc::channel(64);

    hub.join(
        JoinRequest::new("room-1", Role::Cli, 1, [1; 32], None, old_cli_tx)
            .with_close_signal(old_cli_close_tx),
    )
    .expect("old cli joins");
    hub.join(JoinRequest::new(
        "room-1",
        Role::Cli,
        2,
        [1; 32],
        None,
        new_cli_tx,
    ))
    .expect("new cli replaces old cli");

    assert_eq!(old_cli_close_rx.recv().await, Some(CloseSignal::Replaced));
    assert_eq!(CloseSignal::Replaced.close_reason(), "replaced retryable");
}

#[tokio::test]
async fn forward_evicting_slow_cli_notifies_app() {
    let hub = Hub::default();
    let (cli_tx, mut cli_rx) = mpsc::channel(1);
    let (app_tx, mut app_rx) = mpsc::channel(64);

    hub.join(JoinRequest::new(
        "room-1",
        Role::Cli,
        1,
        [1; 32],
        None,
        cli_tx,
    ))
    .expect("cli joins");
    hub.join(JoinRequest::new(
        "room-1",
        Role::App,
        2,
        [2; 32],
        Some([9; 32]),
        app_tx,
    ))
    .expect("app joins");

    // Drain the PeerJoined the CLI received so its cap-1 channel is empty.
    let _ = cli_rx.recv().await;
    let _ = app_rx.recv().await;

    let frame = OuterFrame::Data {
        room_id: "room-1".to_string(),
        direction: Direction::AppToCli,
        seq: 1,
        nonce: [2; 12],
        ciphertext: vec![4, 5, 6],
    };
    hub.forward("room-1", Role::App, 2, frame.clone())
        .expect("first forward fills cli channel");
    hub.forward("room-1", Role::App, 2, frame)
        .expect("slow cli eviction is not fatal");

    let stats = hub.stats();
    assert_eq!(stats.cli_connected_total, 0);
    assert_eq!(stats.app_without_cli, 1);
    // The surviving app must be told the CLI left so its SecureSession resets.
    assert_eq!(
        app_rx.recv().await,
        Some(OuterFrame::PeerLeft { role: Role::Cli })
    );
}

#[tokio::test]
async fn leave_disconnects_sender_and_removes_empty_room() {
    let hub = Hub::default();
    let (cli_tx, _cli_rx) = mpsc::channel(64);

    hub.join(JoinRequest::new(
        "room-1",
        Role::Cli,
        1,
        [1; 32],
        None,
        cli_tx,
    ))
    .expect("cli joins");
    hub.leave("room-1", Role::Cli, 1);

    assert_eq!(hub.room_count(), 0);
}

#[tokio::test]
async fn app_leave_keeps_cli_registered_for_reconnect() {
    let hub = Hub::default();
    let (cli_tx, mut cli_rx) = mpsc::channel(64);
    let (app_tx, _app_rx) = mpsc::channel(64);

    hub.join(JoinRequest::new(
        "room-1",
        Role::Cli,
        1,
        [1; 32],
        None,
        cli_tx,
    ))
    .expect("cli joins");
    hub.join(JoinRequest::new(
        "room-1",
        Role::App,
        2,
        [2; 32],
        Some([9; 32]),
        app_tx,
    ))
    .expect("app joins");

    let _ = cli_rx.recv().await;
    hub.leave("room-1", Role::App, 2);

    assert_eq!(hub.room_count(), 1);
    assert_eq!(hub.stats().cli_idle, 1);
    assert!(cli_rx.try_recv().is_err());
}

#[tokio::test]
async fn hub_stats_counts_paired_and_idle_cli_rooms_with_app_labels() {
    let hub = Hub::default();
    let (cli_idle_tx, _cli_idle_rx) = mpsc::channel(64);
    let (cli_paired_tx, _cli_paired_rx) = mpsc::channel(64);
    let (app_tx, _app_rx) = mpsc::channel(64);

    hub.join(JoinRequest::new(
        "idle-room",
        Role::Cli,
        1,
        [1; 32],
        None,
        cli_idle_tx,
    ))
    .expect("idle cli joins");
    hub.join(JoinRequest::new(
        "paired-room",
        Role::Cli,
        2,
        [2; 32],
        None,
        cli_paired_tx,
    ))
    .expect("paired cli joins");
    hub.join(JoinRequest::new(
        "paired-room",
        Role::App,
        3,
        [3; 32],
        Some([9; 32]),
        app_tx,
    ))
    .expect("app joins");

    let stats = hub.stats();
    assert_eq!(stats.rooms_total, 2);
    assert_eq!(stats.cli_connected_total, 2);
    assert_eq!(stats.app_connected_total, 1);
    assert_eq!(stats.cli_paired, 1);
    assert_eq!(stats.cli_idle, 1);
    assert_eq!(stats.app_paired, 1);
    assert_eq!(stats.app_without_cli, 0);
}

#[tokio::test]
async fn cli_transport_leave_notifies_app_to_stop() {
    let hub = Hub::default();
    let (cli_tx, mut cli_rx) = mpsc::channel(64);
    let (app_tx, mut app_rx) = mpsc::channel(64);

    hub.join(JoinRequest::new(
        "room-1",
        Role::Cli,
        1,
        [1; 32],
        None,
        cli_tx,
    ))
    .expect("cli joins");
    hub.join(JoinRequest::new(
        "room-1",
        Role::App,
        2,
        [2; 32],
        Some([9; 32]),
        app_tx,
    ))
    .expect("app joins");

    let _ = cli_rx.recv().await;
    // Drain the PeerJoined the relay sent to the joining app.
    let _ = app_rx.recv().await;
    hub.leave("room-1", Role::Cli, 1);

    assert_eq!(hub.room_count(), 1);
    assert_eq!(hub.stats().app_without_cli, 1);
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_millis(200), app_rx.recv())
            .await
            .expect("app should be notified when cli leaves"),
        Some(OuterFrame::PeerLeft { role: Role::Cli })
    );
    assert!(cli_rx.recv().await.is_none());
}

#[tokio::test]
async fn cli_rejoin_after_transport_leave_notifies_existing_app() {
    let hub = Hub::default();
    let (cli_old_tx, mut cli_old_rx) = mpsc::channel(64);
    let (app_tx, mut app_rx) = mpsc::channel(64);
    let (cli_new_tx, _cli_new_rx) = mpsc::channel(64);

    hub.join(JoinRequest::new(
        "room-1",
        Role::Cli,
        1,
        [1; 32],
        None,
        cli_old_tx,
    ))
    .expect("cli joins");
    hub.join(JoinRequest::new(
        "room-1",
        Role::App,
        2,
        [2; 32],
        Some([9; 32]),
        app_tx,
    ))
    .expect("app joins");

    let _ = cli_old_rx.recv().await;
    // Drain the PeerJoined the relay sent to the app when it first joined.
    let _ = app_rx.recv().await;
    hub.leave("room-1", Role::Cli, 1);

    hub.join(JoinRequest::new(
        "room-1",
        Role::Cli,
        3,
        [3; 32],
        None,
        cli_new_tx,
    ))
    .expect("cli transport rejoins");

    assert_eq!(
        app_rx.recv().await,
        Some(OuterFrame::PeerLeft { role: Role::Cli })
    );
    assert_eq!(
        app_rx.recv().await,
        Some(OuterFrame::PeerJoined {
            role: Role::Cli,
            device_pubkey: [3; 32],
            pairing_token_proof: None,

            connection_salt: None,
        })
    );
}

#[tokio::test]
async fn joining_peer_notifies_existing_peer_with_public_key_and_pairing_proof() {
    let hub = Hub::default();
    let (cli_tx, mut cli_rx) = mpsc::channel(64);
    let (app_tx, _app_rx) = mpsc::channel(64);

    hub.join(JoinRequest::new(
        "room-1",
        Role::Cli,
        1,
        [1; 32],
        None,
        cli_tx,
    ))
    .expect("cli joins");
    hub.join(JoinRequest::new(
        "room-1",
        Role::App,
        2,
        [2; 32],
        Some([9; 32]),
        app_tx,
    ))
    .expect("app joins");

    assert_eq!(
        cli_rx.recv().await,
        Some(OuterFrame::PeerJoined {
            role: Role::App,
            device_pubkey: [2; 32],
            pairing_token_proof: Some([9; 32]),

            connection_salt: None,
        })
    );
}

#[tokio::test]
async fn app_join_without_cli_is_rejected_without_creating_room() {
    let hub = Hub::default();
    let (app_tx, mut app_rx) = mpsc::channel(64);

    let error = hub
        .join(JoinRequest::new(
            "room-1",
            Role::App,
            2,
            [2; 32],
            Some([9; 32]),
            app_tx,
        ))
        .expect_err("app cannot join before cli is registered");

    assert_eq!(error.to_string(), "cli not registered");
    assert_eq!(hub.room_count(), 0);
    assert!(app_rx.try_recv().is_err());
}

#[tokio::test]
async fn cleanup_inactive_rooms_drops_expired_rooms() {
    let hub = Hub::default();
    let (cli_tx, mut cli_rx) = mpsc::channel(64);

    hub.join(JoinRequest::new(
        "room-1",
        Role::Cli,
        1,
        [1; 32],
        None,
        cli_tx,
    ))
    .expect("cli joins");

    assert_eq!(hub.cleanup_inactive_rooms(Duration::from_secs(60)), 0);
    assert_eq!(hub.room_count(), 1);

    assert_eq!(hub.cleanup_inactive_rooms(Duration::ZERO), 1);
    assert_eq!(hub.room_count(), 0);
    assert_eq!(
        cli_rx.recv().await,
        Some(OuterFrame::Error {
            message: "room expired".to_string()
        })
    );
    assert!(cli_rx.recv().await.is_none());
}

#[tokio::test]
async fn app_rejoin_evicts_old_app_and_sends_new_peer_joined_to_cli() {
    let hub = Hub::default();
    let (cli_tx, mut cli_rx) = mpsc::channel(64);
    let (app_old_tx, mut app_old_rx) = mpsc::channel(64);
    let (app_new_tx, _app_new_rx) = mpsc::channel(64);

    hub.join(JoinRequest::new(
        "room-1",
        Role::Cli,
        1,
        [1; 32],
        None,
        cli_tx,
    ))
    .expect("cli joins");
    hub.join(JoinRequest::new(
        "room-1",
        Role::App,
        2,
        [2; 32],
        Some([9; 32]),
        app_old_tx,
    ))
    .expect("old app joins");

    // Drain PeerJoined for old app.
    let _ = cli_rx.recv().await;
    // The old app also received a PeerJoined describing the CLI; drain it.
    let _ = app_old_rx.recv().await;

    // New app joins while old app is still registered.
    hub.join(JoinRequest::new(
        "room-1",
        Role::App,
        3,
        [3; 32],
        Some([8; 32]),
        app_new_tx,
    ))
    .expect("new app evicts old app");

    // CLI only receives PeerJoined for the new app; app-side disconnects are
    // intentionally invisible to CLI so it can remain idle.
    assert_eq!(
        cli_rx.recv().await,
        Some(OuterFrame::PeerJoined {
            role: Role::App,
            device_pubkey: [3; 32],
            pairing_token_proof: Some([8; 32]),

            connection_salt: None,
        })
    );

    // Old app receives Evicted so it can surface "另一台设备已连接" and not auto-retry.
    assert_eq!(app_old_rx.recv().await, Some(OuterFrame::Evicted));
}

#[tokio::test]
async fn protected_room_rejects_app_without_matching_relay_admission() {
    let hub = Hub::default();
    let (cli_tx, mut cli_rx) = mpsc::channel(64);
    let (app_tx, mut app_rx) = mpsc::channel(64);

    hub.join(
        JoinRequest::new("room-1", Role::Cli, 1, [1; 32], None, cli_tx)
            .with_relay_admission([7; 32]),
    )
    .expect("protected cli joins");

    let err = hub
        .join(JoinRequest::new(
            "room-1",
            Role::App,
            2,
            [2; 32],
            Some([9; 32]),
            app_tx,
        ))
        .expect_err("missing admission is rejected for protected room");

    assert_eq!(err, HubError::AdmissionRejected);
    assert_eq!(hub.stats().app_connected_total, 0);
    assert!(cli_rx.try_recv().is_err());
    assert!(app_rx.try_recv().is_err());
}

#[tokio::test]
async fn checks_registered_room_admission_for_http_authorization() {
    let hub = Hub::default();
    let (cli_tx, _) = mpsc::channel(64);

    hub.join(
        JoinRequest::new("room-1", Role::Cli, 1, [1; 32], None, cli_tx)
            .with_relay_admission([7; 32]),
    )
    .expect("protected cli joins");

    assert_eq!(
        hub.check_room_admission("room-1", [7; 32]),
        AdmissionCheck::Authorized
    );
    assert_eq!(
        hub.check_room_admission("room-1", [8; 32]),
        AdmissionCheck::Unauthorized
    );
    assert_eq!(
        hub.check_room_admission("missing", [7; 32]),
        AdmissionCheck::RoomMissing
    );
}

#[tokio::test]
async fn mismatched_relay_admission_does_not_evict_existing_app() {
    let hub = Hub::default();
    let (cli_tx, mut cli_rx) = mpsc::channel(64);
    let (app_old_tx, mut app_old_rx) = mpsc::channel(64);
    let (app_bad_tx, mut app_bad_rx) = mpsc::channel(64);

    hub.join(
        JoinRequest::new("room-1", Role::Cli, 1, [1; 32], None, cli_tx)
            .with_relay_admission([7; 32]),
    )
    .expect("protected cli joins");
    hub.join(
        JoinRequest::new("room-1", Role::App, 2, [2; 32], Some([9; 32]), app_old_tx)
            .with_relay_admission([7; 32]),
    )
    .expect("app with matching admission joins");

    let _ = cli_rx.recv().await;
    // Drain the PeerJoined the relay sent to the app when it joined.
    let _ = app_old_rx.recv().await;

    let err = hub
        .join(
            JoinRequest::new("room-1", Role::App, 3, [3; 32], Some([8; 32]), app_bad_tx)
                .with_relay_admission([8; 32]),
        )
        .expect_err("mismatched admission is rejected");

    assert_eq!(err, HubError::AdmissionRejected);
    assert_eq!(hub.stats().app_connected_total, 1);
    assert!(cli_rx.try_recv().is_err());
    assert!(app_old_rx.try_recv().is_err());
    assert!(app_bad_rx.try_recv().is_err());

    hub.forward("room-1", Role::App, 2, OuterFrame::Ping)
        .expect("existing app remains active");
    assert_eq!(cli_rx.recv().await, Some(OuterFrame::Ping));
}

#[tokio::test]
async fn legacy_room_without_relay_admission_still_allows_legacy_app() {
    let hub = Hub::default();
    let (cli_tx, mut cli_rx) = mpsc::channel(64);
    let (app_tx, _app_rx) = mpsc::channel(64);

    hub.join(JoinRequest::new(
        "room-1",
        Role::Cli,
        1,
        [1; 32],
        None,
        cli_tx,
    ))
    .expect("legacy cli joins");
    hub.join(JoinRequest::new(
        "room-1",
        Role::App,
        2,
        [2; 32],
        Some([9; 32]),
        app_tx,
    ))
    .expect("legacy app remains allowed");

    assert_eq!(
        cli_rx.recv().await,
        Some(OuterFrame::PeerJoined {
            role: Role::App,
            device_pubkey: [2; 32],
            pairing_token_proof: Some([9; 32]),

            connection_salt: None,
        })
    );
}

#[tokio::test]
async fn cli_rejoin_evicts_old_cli_and_notifies_app() {
    let hub = Hub::default();
    let (cli_old_tx, mut cli_old_rx) = mpsc::channel(64);
    let (cli_new_tx, _cli_new_rx) = mpsc::channel(64);
    let (app_tx, mut app_rx) = mpsc::channel(64);

    hub.join(JoinRequest::new(
        "room-1",
        Role::Cli,
        1,
        [1; 32],
        None,
        cli_old_tx,
    ))
    .expect("old cli joins");
    hub.join(JoinRequest::new(
        "room-1",
        Role::App,
        2,
        [2; 32],
        Some([9; 32]),
        app_tx,
    ))
    .expect("app joins");

    let _ = cli_old_rx.recv().await;
    // Drain the PeerJoined the relay sent to the app when it joined.
    let _ = app_rx.recv().await;

    hub.join(JoinRequest::new(
        "room-1",
        Role::Cli,
        3,
        [3; 32],
        None,
        cli_new_tx,
    ))
    .expect("new cli evicts old cli");

    assert_eq!(
        app_rx.recv().await,
        Some(OuterFrame::PeerJoined {
            role: Role::Cli,
            device_pubkey: [3; 32],
            pairing_token_proof: None,

            connection_salt: None,
        })
    );
    assert!(app_rx.try_recv().is_err());
    assert_eq!(
        cli_old_rx.recv().await,
        Some(OuterFrame::PeerLeft { role: Role::Cli })
    );
}

#[tokio::test]
async fn cleanup_inactive_rooms_notifies_connected_peers_before_removal() {
    let hub = Hub::default();
    let (cli_tx, mut cli_rx) = mpsc::channel(64);
    let (app_tx, mut app_rx) = mpsc::channel(64);

    hub.join(JoinRequest::new(
        "room-1",
        Role::Cli,
        1,
        [1; 32],
        None,
        cli_tx,
    ))
    .expect("cli joins");
    hub.join(JoinRequest::new(
        "room-1",
        Role::App,
        2,
        [2; 32],
        Some([9; 32]),
        app_tx,
    ))
    .expect("app joins");

    assert_eq!(
        cli_rx.recv().await,
        Some(OuterFrame::PeerJoined {
            role: Role::App,
            device_pubkey: [2; 32],
            pairing_token_proof: Some([9; 32]),

            connection_salt: None,
        })
    );

    // The app also received a PeerJoined describing the CLI when it joined.
    let _ = app_rx.recv().await;

    assert_eq!(hub.cleanup_inactive_rooms(Duration::ZERO), 1);
    assert_eq!(hub.room_count(), 0);
    assert_eq!(
        cli_rx.recv().await,
        Some(OuterFrame::Error {
            message: "room expired".to_string()
        })
    );
    assert_eq!(
        app_rx.recv().await,
        Some(OuterFrame::Error {
            message: "room expired".to_string()
        })
    );
    assert!(cli_rx.recv().await.is_none());
    assert!(app_rx.recv().await.is_none());
}
