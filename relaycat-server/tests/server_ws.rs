use futures_util::{SinkExt, Stream, StreamExt};
use relaycat_protocol::{
    AppJoinIntent, OuterFrame, RelayErrorCode, Role, decode_frame, encode_frame,
};
use relaycat_relay::server::MAX_BINARY_FRAME_BYTES;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Error as TungsteniteError, Message},
};
use tower::ServiceExt;

async fn next_binary_frame<S>(reader: &mut S) -> OuterFrame
where
    S: Stream<Item = Result<Message, TungsteniteError>> + Unpin,
{
    loop {
        let message = tokio::time::timeout(std::time::Duration::from_secs(2), reader.next())
            .await
            .expect("timed out waiting for websocket frame")
            .expect("websocket closed before frame")
            .expect("websocket frame error");
        match message {
            Message::Binary(bytes) => return decode_frame(&bytes).expect("decode frame"),
            Message::Ping(_) | Message::Pong(_) => continue,
            message => panic!("expected binary frame, got {message:?}"),
        }
    }
}

#[tokio::test]
async fn root_returns_running_status_and_version() {
    let app = relaycat_relay::server::app(relaycat_relay::server::AppState::default());

    let response = app
        .oneshot(
            axum::http::Request::builder()
                .uri("/")
                .body(axum::body::Body::empty())
                .expect("request"),
        )
        .await
        .expect("root response");

    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json body");

    assert_eq!(json["status"], "running");
    assert_eq!(json["version"], format!("v{}", env!("CARGO_PKG_VERSION")));
    assert_eq!(json["protocol_version"], 1);
    assert_eq!(json["min_gui_version"], "0.1.5");
}

#[tokio::test]
async fn websocket_rejects_connections_exceeding_per_ip_limit() {
    if std::env::var_os("RELAYCAT_RUN_NET_TESTS").is_none() {
        eprintln!("skipping network test; set RELAYCAT_RUN_NET_TESTS=1 to run");
        return;
    }

    let state =
        relaycat_relay::server::AppState::with_config(relaycat_relay::config::RelayConfig {
            limits: relaycat_relay::config::LimitsConfig {
                max_connections_per_ip: 1,
                ..relaycat_relay::config::LimitsConfig::default()
            },
            ..relaycat_relay::config::RelayConfig::default()
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test listener");
    let addr = listener.local_addr().expect("listener addr");
    let app = relaycat_relay::server::app(state);
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .expect("server failed");
    });

    let (first_socket, _) = connect_async(format!("ws://{addr}/ws?room_id=room-1&role=cli"))
        .await
        .expect("first connection admitted");

    let error = connect_async(format!("ws://{addr}/ws?room_id=room-2&role=app"))
        .await
        .expect_err("second connection from the same IP is rejected");
    let TungsteniteError::Http(response) = error else {
        panic!("expected HTTP rejection, got {error:?}");
    };
    assert_eq!(response.status().as_u16(), 429);

    drop(first_socket);
    let reopened = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            match connect_async(format!("ws://{addr}/ws?room_id=room-2&role=app")).await {
                Ok((socket, _)) => return socket,
                Err(TungsteniteError::Http(response)) if response.status().as_u16() == 429 => {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                Err(error) => panic!("unexpected retry failure: {error:?}"),
            }
        }
    })
    .await
    .expect("connection slot is released after the first socket closes");
    drop(reopened);
    server.abort();
}

#[tokio::test]
async fn websocket_join_notifies_existing_peer_with_peer_joined() {
    if std::env::var_os("RELAYCAT_RUN_NET_TESTS").is_none() {
        eprintln!("skipping network test; set RELAYCAT_RUN_NET_TESTS=1 to run");
        return;
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test listener");
    let addr = listener.local_addr().expect("listener addr");
    let app = relaycat_relay::server::app(relaycat_relay::server::AppState::default());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .expect("server failed");
    });

    let (cli_socket, _) = connect_async(format!("ws://{addr}/ws?room_id=room-1&role=cli"))
        .await
        .expect("cli connect");
    let (mut cli_writer, mut cli_reader) = cli_socket.split();
    cli_writer
        .send(Message::Binary(
            encode_frame(&OuterFrame::Join {
                room_id: "room-1".to_string(),
                role: Role::Cli,
                device_pubkey: [1; 32],
                pairing_token_proof: None,
                relay_admission: None,
                supports_join_accepted: false,
                app_join_intent: AppJoinIntent::Takeover,

                connection_salt: None,
            })
            .expect("encode cli join")
            .into(),
        ))
        .await
        .expect("send cli join");

    let (app_socket, _) = connect_async(format!("ws://{addr}/ws?room_id=room-1&role=app"))
        .await
        .expect("app connect");
    let (mut app_writer, _app_reader) = app_socket.split();
    app_writer
        .send(Message::Binary(
            encode_frame(&OuterFrame::Join {
                room_id: "room-1".to_string(),
                role: Role::App,
                device_pubkey: [2; 32],
                pairing_token_proof: Some([9; 32]),
                relay_admission: None,
                supports_join_accepted: false,
                app_join_intent: AppJoinIntent::Takeover,

                connection_salt: None,
            })
            .expect("encode app join")
            .into(),
        ))
        .await
        .expect("send app join");

    assert_eq!(
        next_binary_frame(&mut cli_reader).await,
        OuterFrame::PeerJoined {
            role: Role::App,
            device_pubkey: [2; 32],
            pairing_token_proof: Some([9; 32]),

            connection_salt: None,
        }
    );

    server.abort();
}

#[tokio::test]
async fn websocket_join_accepted_sent_when_advertised() {
    if std::env::var_os("RELAYCAT_RUN_NET_TESTS").is_none() {
        eprintln!("skipping network test; set RELAYCAT_RUN_NET_TESTS=1 to run");
        return;
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test listener");
    let addr = listener.local_addr().expect("listener addr");
    let app = relaycat_relay::server::app(relaycat_relay::server::AppState::default());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .expect("server failed");
    });

    let (cli_socket, _) = connect_async(format!("ws://{addr}/ws?room_id=room-1&role=cli"))
        .await
        .expect("cli connect");
    let (mut cli_writer, _cli_reader) = cli_socket.split();
    cli_writer
        .send(Message::Binary(
            encode_frame(&OuterFrame::Join {
                room_id: "room-1".to_string(),
                role: Role::Cli,
                device_pubkey: [1; 32],
                pairing_token_proof: None,
                relay_admission: None,
                connection_salt: None,
                supports_join_accepted: false,
                app_join_intent: AppJoinIntent::Takeover,
            })
            .expect("encode cli join")
            .into(),
        ))
        .await
        .expect("send cli join");

    let (app_socket, _) = connect_async(format!("ws://{addr}/ws?room_id=room-1&role=app"))
        .await
        .expect("app connect");
    let (mut app_writer, mut app_reader) = app_socket.split();
    app_writer
        .send(Message::Binary(
            encode_frame(&OuterFrame::Join {
                room_id: "room-1".to_string(),
                role: Role::App,
                device_pubkey: [2; 32],
                pairing_token_proof: Some([9; 32]),
                relay_admission: None,
                connection_salt: None,
                supports_join_accepted: true,
                app_join_intent: AppJoinIntent::Takeover,
            })
            .expect("encode app join")
            .into(),
        ))
        .await
        .expect("send app join");

    // The advertising joiner's very first frame must be JoinAccepted, before
    // any PeerJoined notification for the already-present CLI.
    let message = app_reader
        .next()
        .await
        .expect("join accepted message")
        .expect("join accepted websocket message");
    let Message::Binary(bytes) = message else {
        panic!("expected binary JoinAccepted, got {message:?}");
    };
    assert_eq!(
        decode_frame(&bytes).expect("decode join accepted"),
        OuterFrame::JoinAccepted
    );

    let message = app_reader
        .next()
        .await
        .expect("peer joined message")
        .expect("peer joined websocket message");
    let Message::Binary(bytes) = message else {
        panic!("expected binary PeerJoined, got {message:?}");
    };
    assert!(matches!(
        decode_frame(&bytes).expect("decode peer joined"),
        OuterFrame::PeerJoined {
            role: Role::Cli,
            ..
        }
    ));

    server.abort();
}

#[tokio::test]
async fn websocket_forwards_connection_salt_to_both_peers() {
    if std::env::var_os("RELAYCAT_RUN_NET_TESTS").is_none() {
        eprintln!("skipping network test; set RELAYCAT_RUN_NET_TESTS=1 to run");
        return;
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test listener");
    let addr = listener.local_addr().expect("listener addr");
    let app = relaycat_relay::server::app(relaycat_relay::server::AppState::default());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .expect("server failed");
    });

    let cli_salt = [0x5a_u8; 32];
    let app_salt = [0xa5_u8; 32];

    let (cli_socket, _) = connect_async(format!("ws://{addr}/ws?room_id=room-1&role=cli"))
        .await
        .expect("cli connect");
    let (mut cli_writer, mut cli_reader) = cli_socket.split();
    cli_writer
        .send(Message::Binary(
            encode_frame(&OuterFrame::Join {
                room_id: "room-1".to_string(),
                role: Role::Cli,
                device_pubkey: [1; 32],
                pairing_token_proof: Some([7; 32]),
                relay_admission: None,
                supports_join_accepted: false,
                app_join_intent: AppJoinIntent::Takeover,
                connection_salt: Some(cli_salt),
            })
            .expect("encode cli join")
            .into(),
        ))
        .await
        .expect("send cli join");

    let (app_socket, _) = connect_async(format!("ws://{addr}/ws?room_id=room-1&role=app"))
        .await
        .expect("app connect");
    let (mut app_writer, mut app_reader) = app_socket.split();
    app_writer
        .send(Message::Binary(
            encode_frame(&OuterFrame::Join {
                room_id: "room-1".to_string(),
                role: Role::App,
                device_pubkey: [2; 32],
                pairing_token_proof: Some([9; 32]),
                relay_admission: None,
                supports_join_accepted: false,
                app_join_intent: AppJoinIntent::Takeover,
                connection_salt: Some(app_salt),
            })
            .expect("encode app join")
            .into(),
        ))
        .await
        .expect("send app join");

    // The already-present CLI must learn the app's salt verbatim.
    assert_eq!(
        next_binary_frame(&mut cli_reader).await,
        OuterFrame::PeerJoined {
            role: Role::App,
            device_pubkey: [2; 32],
            pairing_token_proof: Some([9; 32]),
            connection_salt: Some(app_salt),
        }
    );
    // The newcomer app must learn the already-present CLI's salt verbatim.
    assert_eq!(
        next_binary_frame(&mut app_reader).await,
        OuterFrame::PeerJoined {
            role: Role::Cli,
            device_pubkey: [1; 32],
            pairing_token_proof: Some([7; 32]),
            connection_salt: Some(cli_salt),
        }
    );

    server.abort();
}

#[tokio::test]
async fn websocket_app_disconnect_then_reconnect_only_notifies_cli_on_rejoin() {
    if std::env::var_os("RELAYCAT_RUN_NET_TESTS").is_none() {
        eprintln!("skipping network test; set RELAYCAT_RUN_NET_TESTS=1 to run");
        return;
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test listener");
    let addr = listener.local_addr().expect("listener addr");
    let app = relaycat_relay::server::app(relaycat_relay::server::AppState::default());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .expect("server failed");
    });

    let (cli_socket, _) = connect_async(format!("ws://{addr}/ws?room_id=room-1&role=cli"))
        .await
        .expect("cli connect");
    let (mut cli_writer, mut cli_reader) = cli_socket.split();
    cli_writer
        .send(Message::Binary(
            encode_frame(&OuterFrame::Join {
                room_id: "room-1".to_string(),
                role: Role::Cli,
                device_pubkey: [1; 32],
                pairing_token_proof: None,
                relay_admission: None,
                supports_join_accepted: false,
                app_join_intent: AppJoinIntent::Takeover,

                connection_salt: None,
            })
            .expect("encode cli join")
            .into(),
        ))
        .await
        .expect("send cli join");

    let (app_socket, _) = connect_async(format!("ws://{addr}/ws?room_id=room-1&role=app"))
        .await
        .expect("app connect");
    let (mut app_writer, _) = app_socket.split();
    app_writer
        .send(Message::Binary(
            encode_frame(&OuterFrame::Join {
                room_id: "room-1".to_string(),
                role: Role::App,
                device_pubkey: [2; 32],
                pairing_token_proof: Some([9; 32]),
                relay_admission: None,
                supports_join_accepted: false,
                app_join_intent: AppJoinIntent::Takeover,

                connection_salt: None,
            })
            .expect("encode first app join")
            .into(),
        ))
        .await
        .expect("send first app join");

    assert_eq!(
        next_binary_frame(&mut cli_reader).await,
        OuterFrame::PeerJoined {
            role: Role::App,
            device_pubkey: [2; 32],
            pairing_token_proof: Some([9; 32]),

            connection_salt: None,
        }
    );

    app_writer
        .send(Message::Close(None))
        .await
        .expect("close first app");

    let (app_socket, _) = connect_async(format!("ws://{addr}/ws?room_id=room-1&role=app"))
        .await
        .expect("app reconnect");
    let (mut app_writer, _) = app_socket.split();
    app_writer
        .send(Message::Binary(
            encode_frame(&OuterFrame::Join {
                room_id: "room-1".to_string(),
                role: Role::App,
                device_pubkey: [3; 32],
                pairing_token_proof: Some([8; 32]),
                relay_admission: None,
                supports_join_accepted: false,
                app_join_intent: AppJoinIntent::Takeover,

                connection_salt: None,
            })
            .expect("encode second app join")
            .into(),
        ))
        .await
        .expect("send second app join");

    assert_eq!(
        next_binary_frame(&mut cli_reader).await,
        OuterFrame::PeerJoined {
            role: Role::App,
            device_pubkey: [3; 32],
            pairing_token_proof: Some([8; 32]),

            connection_salt: None,
        }
    );

    server.abort();
}

#[tokio::test]
async fn websocket_allows_repeated_app_reconnects_for_same_room() {
    if std::env::var_os("RELAYCAT_RUN_NET_TESTS").is_none() {
        eprintln!("skipping network test; set RELAYCAT_RUN_NET_TESTS=1 to run");
        return;
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test listener");
    let addr = listener.local_addr().expect("listener addr");
    let app = relaycat_relay::server::app(relaycat_relay::server::AppState::default());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .expect("server failed");
    });

    let (cli_socket, _) = connect_async(format!("ws://{addr}/ws?room_id=room-1&role=cli"))
        .await
        .expect("cli connect");
    let (mut cli_writer, mut cli_reader) = cli_socket.split();
    cli_writer
        .send(Message::Binary(
            encode_frame(&OuterFrame::Join {
                room_id: "room-1".to_string(),
                role: Role::Cli,
                device_pubkey: [1; 32],
                pairing_token_proof: None,
                relay_admission: None,
                supports_join_accepted: false,
                app_join_intent: AppJoinIntent::Takeover,

                connection_salt: None,
            })
            .expect("encode cli join")
            .into(),
        ))
        .await
        .expect("send cli join");

    for attempt in 1..=4 {
        let (app_socket, _) = connect_async(format!("ws://{addr}/ws?room_id=room-1&role=app"))
            .await
            .expect("app reconnect");
        let (mut app_writer, mut app_reader) = app_socket.split();
        app_writer
            .send(Message::Binary(
                encode_frame(&OuterFrame::Join {
                    room_id: "room-1".to_string(),
                    role: Role::App,
                    device_pubkey: [attempt; 32],
                    pairing_token_proof: Some([attempt + 10; 32]),
                    relay_admission: None,
                    supports_join_accepted: false,
                    app_join_intent: AppJoinIntent::Takeover,

                    connection_salt: None,
                })
                .expect("encode app join")
                .into(),
            ))
            .await
            .expect("send app join");

        assert_eq!(
            next_binary_frame(&mut cli_reader).await,
            OuterFrame::PeerJoined {
                role: Role::App,
                device_pubkey: [attempt; 32],
                pairing_token_proof: Some([attempt + 10; 32]),

                connection_salt: None,
            }
        );
        app_writer
            .send(Message::Close(None))
            .await
            .expect("close app");
        assert!(app_reader.next().await.is_some());
    }

    server.abort();
}

#[tokio::test]
async fn websocket_cli_reconnect_notifies_app_with_peer_joined_without_peer_left() {
    if std::env::var_os("RELAYCAT_RUN_NET_TESTS").is_none() {
        eprintln!("skipping network test; set RELAYCAT_RUN_NET_TESTS=1 to run");
        return;
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test listener");
    let addr = listener.local_addr().expect("listener addr");
    let app = relaycat_relay::server::app(relaycat_relay::server::AppState::default());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .expect("server failed");
    });

    let (cli_socket, _) = connect_async(format!("ws://{addr}/ws?room_id=room-1&role=cli"))
        .await
        .expect("first cli connect");
    let (mut cli_writer, _cli_reader) = cli_socket.split();
    cli_writer
        .send(Message::Binary(
            encode_frame(&OuterFrame::Join {
                room_id: "room-1".to_string(),
                role: Role::Cli,
                device_pubkey: [1; 32],
                pairing_token_proof: None,
                relay_admission: None,
                supports_join_accepted: false,
                app_join_intent: AppJoinIntent::Takeover,

                connection_salt: None,
            })
            .expect("encode first cli join")
            .into(),
        ))
        .await
        .expect("send first cli join");

    let (app_socket, _) = connect_async(format!("ws://{addr}/ws?room_id=room-1&role=app"))
        .await
        .expect("app connect");
    let (mut app_writer, mut app_reader) = app_socket.split();
    app_writer
        .send(Message::Binary(
            encode_frame(&OuterFrame::Join {
                room_id: "room-1".to_string(),
                role: Role::App,
                device_pubkey: [2; 32],
                pairing_token_proof: Some([9; 32]),
                relay_admission: None,
                supports_join_accepted: false,
                app_join_intent: AppJoinIntent::Takeover,

                connection_salt: None,
            })
            .expect("encode app join")
            .into(),
        ))
        .await
        .expect("send app join");

    assert_eq!(
        next_binary_frame(&mut app_reader).await,
        OuterFrame::PeerJoined {
            role: Role::Cli,
            device_pubkey: [1; 32],
            pairing_token_proof: None,
            connection_salt: None,
        }
    );

    let (second_cli_socket, _) = connect_async(format!("ws://{addr}/ws?room_id=room-1&role=cli"))
        .await
        .expect("second cli connect");
    let (mut second_cli_writer, _second_cli_reader) = second_cli_socket.split();
    second_cli_writer
        .send(Message::Binary(
            encode_frame(&OuterFrame::Join {
                room_id: "room-1".to_string(),
                role: Role::Cli,
                device_pubkey: [3; 32],
                pairing_token_proof: None,
                relay_admission: None,
                supports_join_accepted: false,
                app_join_intent: AppJoinIntent::Takeover,

                connection_salt: None,
            })
            .expect("encode second cli join")
            .into(),
        ))
        .await
        .expect("send second cli join");

    assert_eq!(
        next_binary_frame(&mut app_reader).await,
        OuterFrame::PeerJoined {
            role: Role::Cli,
            device_pubkey: [3; 32],
            pairing_token_proof: None,

            connection_salt: None,
        }
    );

    server.abort();
}

#[tokio::test]
async fn websocket_rejects_oversized_initial_binary_frame_with_error() {
    if std::env::var_os("RELAYCAT_RUN_NET_TESTS").is_none() {
        eprintln!("skipping network test; set RELAYCAT_RUN_NET_TESTS=1 to run");
        return;
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test listener");
    let addr = listener.local_addr().expect("listener addr");
    let app = relaycat_relay::server::app(relaycat_relay::server::AppState::default());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .expect("server failed");
    });

    let (mut socket, _) = connect_async(format!("ws://{addr}/ws?room_id=room-1&role=cli"))
        .await
        .expect("connect");
    socket
        .send(Message::Binary(vec![0; MAX_BINARY_FRAME_BYTES + 1].into()))
        .await
        .expect("send oversized frame");

    let OuterFrame::Error { message, code } = next_binary_frame(&mut socket).await else {
        panic!("expected protocol Error");
    };
    assert!(message.contains("size limit"));
    assert_eq!(code, Some(RelayErrorCode::FrameTooLarge));

    server.abort();
}

#[tokio::test]
async fn websocket_rejects_oversized_binary_frame_after_join_with_error() {
    if std::env::var_os("RELAYCAT_RUN_NET_TESTS").is_none() {
        eprintln!("skipping network test; set RELAYCAT_RUN_NET_TESTS=1 to run");
        return;
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test listener");
    let addr = listener.local_addr().expect("listener addr");
    let app = relaycat_relay::server::app(relaycat_relay::server::AppState::default());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .expect("server failed");
    });

    let (mut socket, _) = connect_async(format!("ws://{addr}/ws?room_id=room-1&role=cli"))
        .await
        .expect("connect");
    socket
        .send(Message::Binary(
            encode_frame(&OuterFrame::Join {
                room_id: "room-1".to_string(),
                role: Role::Cli,
                device_pubkey: [1; 32],
                pairing_token_proof: None,
                relay_admission: None,
                supports_join_accepted: false,
                app_join_intent: AppJoinIntent::Takeover,

                connection_salt: None,
            })
            .expect("encode join")
            .into(),
        ))
        .await
        .expect("send join");
    socket
        .send(Message::Binary(vec![0; MAX_BINARY_FRAME_BYTES + 1].into()))
        .await
        .expect("send oversized frame");

    let OuterFrame::Error { message, code } = next_binary_frame(&mut socket).await else {
        panic!("expected protocol Error");
    };
    assert!(message.contains("size limit"));
    assert_eq!(code, Some(RelayErrorCode::FrameTooLarge));

    server.abort();
}

#[tokio::test]
async fn websocket_replies_to_protocol_ping_with_pong() {
    if std::env::var_os("RELAYCAT_RUN_NET_TESTS").is_none() {
        eprintln!("skipping network test; set RELAYCAT_RUN_NET_TESTS=1 to run");
        return;
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test listener");
    let addr = listener.local_addr().expect("listener addr");
    let app = relaycat_relay::server::app(relaycat_relay::server::AppState::default());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .expect("server failed");
    });

    let (mut socket, _) = connect_async(format!("ws://{addr}/ws?room_id=room-1&role=cli"))
        .await
        .expect("connect");
    socket
        .send(Message::Binary(
            encode_frame(&OuterFrame::Join {
                room_id: "room-1".to_string(),
                role: Role::Cli,
                device_pubkey: [1; 32],
                pairing_token_proof: None,
                relay_admission: None,
                supports_join_accepted: false,
                app_join_intent: AppJoinIntent::Takeover,

                connection_salt: None,
            })
            .expect("encode join")
            .into(),
        ))
        .await
        .expect("send join");
    socket
        .send(Message::Binary(
            encode_frame(&OuterFrame::Ping).expect("encode ping").into(),
        ))
        .await
        .expect("send ping");

    let message = socket
        .next()
        .await
        .expect("pong message")
        .expect("pong websocket message");
    let Message::Binary(bytes) = message else {
        panic!("expected binary Pong, got {message:?}");
    };

    assert_eq!(decode_frame(&bytes).expect("decode pong"), OuterFrame::Pong);

    server.abort();
}

#[tokio::test]
async fn websocket_rejects_inbound_message_rate_burst_with_error() {
    if std::env::var_os("RELAYCAT_RUN_NET_TESTS").is_none() {
        eprintln!("skipping network test; set RELAYCAT_RUN_NET_TESTS=1 to run");
        return;
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test listener");
    let addr = listener.local_addr().expect("listener addr");
    let app = relaycat_relay::server::app(relaycat_relay::server::AppState::default());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .expect("server failed");
    });

    let (mut socket, _) = connect_async(format!("ws://{addr}/ws?room_id=room-1&role=cli"))
        .await
        .expect("connect");
    socket
        .send(Message::Binary(
            encode_frame(&OuterFrame::Join {
                room_id: "room-1".to_string(),
                role: Role::Cli,
                device_pubkey: [1; 32],
                pairing_token_proof: None,
                relay_admission: None,
                supports_join_accepted: false,
                app_join_intent: AppJoinIntent::Takeover,

                connection_salt: None,
            })
            .expect("encode join")
            .into(),
        ))
        .await
        .expect("send join");

    let ping = Message::Binary(encode_frame(&OuterFrame::Ping).expect("encode ping").into());
    for _ in 0..150 {
        socket.send(ping.clone()).await.expect("send ping burst");
    }

    let mut saw_error = false;
    for _ in 0..151 {
        let message = tokio::time::timeout(std::time::Duration::from_secs(2), socket.next())
            .await
            .expect("timed out waiting for rate-limit response")
            .expect("rate-limit response")
            .expect("rate-limit websocket message");
        let Message::Binary(bytes) = message else {
            continue;
        };
        if let OuterFrame::Error { message, code } = decode_frame(&bytes).expect("decode frame") {
            assert!(message.contains("rate limit"));
            assert_eq!(code, Some(RelayErrorCode::RateLimited));
            saw_error = true;
            break;
        }
    }

    assert!(saw_error);

    server.abort();
}
