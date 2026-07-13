#[cfg(unix)]
use std::{ffi::CStr, mem::MaybeUninit};
use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use axum::{
    Json, Router,
    extract::{
        Query, State,
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use dashmap::DashMap;
use futures_util::StreamExt;
use relaycat_protocol::{OuterFrame, Role, decode_frame, encode_frame};
use serde::{Deserialize, Serialize};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use tokio::sync::mpsc;

use crate::{
    config::RelayConfig,
    hub::{AdmissionCheck, CloseSignal, Hub, HubStats, JoinRequest},
    room::ConnId,
    xfyun::{RtasrUrlResponse, signed_rtasr_url},
};

pub const MAX_BINARY_FRAME_BYTES: usize = relaycat_protocol::MAX_OUTER_FRAME_BYTES;
/// Default transport limits. These are the values the relay shipped with before
/// they became configurable; `config::LimitsConfig` falls back to them when a
/// field is absent from the config file.
pub const DEFAULT_INBOUND_MESSAGES_PER_SECOND: u32 = 120;
pub const DEFAULT_INBOUND_BYTES_PER_SECOND: usize = 2 * 1024 * 1024;
const ROOM_TTL: Duration = Duration::from_secs(30 * 60);
const ROOM_CLEANUP_INTERVAL: Duration = Duration::from_secs(60);
const STATS_INTERVAL: Duration = Duration::from_secs(10 * 60);
const CLIENT_PING_INTERVAL: Duration = Duration::from_secs(10);
const CLIENT_PONG_TIMEOUT: Duration = Duration::from_secs(30);
/// Capacity of the per-connection outbound channel.  If the channel fills up
/// the recipient is too slow; the hub evicts it so the ws task disconnects.
pub const DEFAULT_OUTBOUND_CHANNEL_CAPACITY: usize = 64;
/// Maximum number of simultaneous WebSocket connections across all rooms.
pub const DEFAULT_MAX_CONCURRENT_CONNECTIONS: usize = 4096;
/// How long to wait for the initial Join frame after the WebSocket is upgraded.
/// Without a timeout, a client that connects but never speaks holds a file
/// descriptor and a tokio task indefinitely.
const JOIN_FRAME_TIMEOUT: Duration = Duration::from_secs(10);
/// Close code used when the relay evicts a peer (slow consumer or replaced):
/// 1013 "Try Again Later". Clients treat it as an abnormal close and the
/// reason text tells them whether the condition is retryable.
const EVICTION_CLOSE_CODE: u16 = 1013;

/// Rate limits for the xfyun rtasr-url signing endpoint. Each signed URL is
/// minted with the relay's xfyun credentials, so any party holding a room's
/// `relay_admission` (CLI and App share it) could otherwise mint unlimited
/// URLs and run up the upstream quota/bill. The per-room bucket caps a single
/// (possibly compromised) room; the global bucket caps total upstream cost.
const RTASR_PER_ROOM_REQUESTS_PER_MINUTE: f64 = 12.0;
const RTASR_GLOBAL_REQUESTS_PER_MINUTE: f64 = 600.0;

#[derive(Debug, Clone)]
pub struct AppState {
    hub: Arc<Hub>,
    config: Arc<RelayConfig>,
    next_conn_id: Arc<AtomicU64>,
    rooms_expired_since_stats: Arc<AtomicU64>,
    active_connections: Arc<AtomicUsize>,
    rtasr_limiter: Arc<RtasrRateLimiter>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            hub: Arc::new(Hub::default()),
            config: Arc::new(RelayConfig::default()),
            next_conn_id: Arc::new(AtomicU64::new(1)),
            rooms_expired_since_stats: Arc::new(AtomicU64::new(0)),
            active_connections: Arc::new(AtomicUsize::new(0)),
            rtasr_limiter: Arc::new(RtasrRateLimiter::new(
                RTASR_GLOBAL_REQUESTS_PER_MINUTE,
                RTASR_PER_ROOM_REQUESTS_PER_MINUTE,
            )),
        }
    }
}

/// RAII guard that decrements the active-connection counter when dropped,
/// ensuring every handle_socket exit path (early return or normal) releases
/// its slot.
struct ConnectionGuard(Arc<AtomicUsize>);

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

impl AppState {
    pub fn with_config(config: RelayConfig) -> Self {
        Self {
            config: Arc::new(config),
            ..Self::default()
        }
    }

    fn next_conn_id(&self) -> ConnId {
        self.next_conn_id.fetch_add(1, Ordering::Relaxed)
    }

    fn record_expired_rooms(&self, count: usize) {
        self.rooms_expired_since_stats
            .fetch_add(count as u64, Ordering::Relaxed);
    }

    fn take_expired_rooms_since_stats(&self) -> u64 {
        self.rooms_expired_since_stats.swap(0, Ordering::Relaxed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ProcessStats {
    cpu_percent: f32,
    memory_rss_mb: f64,
}

struct ProcessStatsCollector {
    system: System,
    pid: Pid,
}

impl ProcessStatsCollector {
    fn new() -> Self {
        let pid = Pid::from_u32(std::process::id());
        let mut system = System::new();
        refresh_current_process(&mut system, pid);
        Self { system, pid }
    }

    fn refresh(&mut self) -> ProcessStats {
        refresh_current_process(&mut self.system, self.pid);
        self.system
            .process(self.pid)
            .map(|process| ProcessStats {
                cpu_percent: process.cpu_usage(),
                memory_rss_mb: bytes_to_mb(process.memory()),
            })
            .unwrap_or(ProcessStats {
                cpu_percent: 0.0,
                memory_rss_mb: 0.0,
            })
    }
}

fn refresh_current_process(system: &mut System, pid: Pid) {
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().with_cpu().with_memory(),
    );
}

fn bytes_to_mb(bytes: u64) -> f64 {
    bytes as f64 / 1024.0 / 1024.0
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct WsQuery {
    room_id: String,
    #[serde(deserialize_with = "deserialize_role")]
    role: Role,
}

#[derive(Debug, Deserialize)]
struct RtasrUrlRequest {
    room_id: String,
    relay_admission: String,
}

#[derive(Debug, Serialize)]
struct ApiError {
    error: &'static str,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
    version: String,
}

fn deserialize_role<'de, D>(deserializer: D) -> Result<Role, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    match value.as_str() {
        "cli" => Ok(Role::Cli),
        "app" => Ok(Role::App),
        other => Err(serde::de::Error::custom(format!(
            "invalid role {other}; expected cli or app"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lowercase_role_query_values() {
        let cli: WsQuery =
            serde_urlencoded::from_str("room_id=room-1&role=cli").expect("parse cli query");
        let app: WsQuery =
            serde_urlencoded::from_str("room_id=room-1&role=app").expect("parse app query");

        assert_eq!(
            cli,
            WsQuery {
                room_id: "room-1".to_string(),
                role: Role::Cli
            }
        );
        assert_eq!(
            app,
            WsQuery {
                room_id: "room-1".to_string(),
                role: Role::App
            }
        );
    }

    #[test]
    fn rejects_binary_frames_larger_than_limit() {
        assert_eq!(MAX_BINARY_FRAME_BYTES, 1024 * 1024);
        assert!(binary_frame_within_limit(
            MAX_BINARY_FRAME_BYTES,
            MAX_BINARY_FRAME_BYTES
        ));
        assert!(!binary_frame_within_limit(
            MAX_BINARY_FRAME_BYTES + 1,
            MAX_BINARY_FRAME_BYTES
        ));
    }

    #[test]
    fn outbound_frame_guard_returns_error_instead_of_silent_drop() {
        let oversized = OuterFrame::Error {
            message: "x".repeat(MAX_BINARY_FRAME_BYTES),
        };

        assert!(encode_outbound_frame(&oversized).is_err());
    }

    #[test]
    fn inbound_rate_limiter_rejects_message_burst_over_limit() {
        let now = Instant::now();
        let mut limiter = InboundRateLimiter::new(2, 1024);

        assert!(limiter.allow_at(100, now));
        assert!(limiter.allow_at(100, now));
        assert!(!limiter.allow_at(100, now));
    }

    #[test]
    fn inbound_rate_limiter_rejects_byte_burst_over_limit() {
        let now = Instant::now();
        let mut limiter = InboundRateLimiter::new(120, 100);

        assert!(limiter.allow_at(60, now));
        assert!(!limiter.allow_at(41, now));
    }

    #[test]
    fn inbound_rate_limiter_refills_over_time() {
        let now = Instant::now();
        let mut limiter = InboundRateLimiter::new(1, 100);

        assert!(limiter.allow_at(100, now));
        assert!(!limiter.allow_at(1, now));
        assert!(limiter.allow_at(100, now + Duration::from_secs(1)));
    }

    #[test]
    fn token_bucket_acquires_until_empty_then_refills() {
        let now = Instant::now();
        let mut bucket = TokenBucket::new(2.0, 1.0);

        assert!(bucket.try_acquire_at(now));
        assert!(bucket.try_acquire_at(now));
        assert!(!bucket.try_acquire_at(now));
        // Refills 1 token per second.
        assert!(bucket.try_acquire_at(now + Duration::from_secs(1)));
        assert!(!bucket.try_acquire_at(now + Duration::from_secs(1)));
    }

    #[test]
    fn rtasr_limiter_enforces_per_room_budget_and_isolates_rooms() {
        // Large global budget so only the per-room cap (3) bites here.
        let limiter = RtasrRateLimiter::new(1_000.0, 3.0);

        assert!(limiter.try_acquire("room-a"));
        assert!(limiter.try_acquire("room-a"));
        assert!(limiter.try_acquire("room-a"));
        assert!(!limiter.try_acquire("room-a"));
        // A different room has its own independent budget.
        assert!(limiter.try_acquire("room-b"));
    }

    #[test]
    fn rtasr_limiter_enforces_global_budget_across_rooms() {
        // Global cap of 2 with generous per-room caps: the 3rd request from a
        // fresh room is still denied because the global budget is spent.
        let limiter = RtasrRateLimiter::new(2.0, 1_000.0);

        assert!(limiter.try_acquire("room-a"));
        assert!(limiter.try_acquire("room-b"));
        assert!(!limiter.try_acquire("room-c"));
    }

    #[test]
    fn rtasr_limiter_does_not_create_entry_when_global_denied() {
        let limiter = RtasrRateLimiter::new(0.0, 10.0);
        assert!(!limiter.try_acquire("room-a"));
        assert!(limiter.per_room.is_empty());
    }

    #[test]
    fn rtasr_limiter_per_room_denial_does_not_drain_global_budget() {
        // Global budget of 5 with a per-room cap of 1.
        let limiter = RtasrRateLimiter::new(5.0, 1.0);

        // room-a spends its single per-room token, then floods the endpoint.
        assert!(limiter.try_acquire("room-a"));
        for _ in 0..50 {
            assert!(!limiter.try_acquire("room-a"));
        }

        // Each denied flood request refunded its global token, so the remaining
        // global budget (5 - 1 successful = 4) is still available to other
        // rooms rather than having been drained by room-a.
        assert!(limiter.try_acquire("room-b"));
        assert!(limiter.try_acquire("room-c"));
        assert!(limiter.try_acquire("room-d"));
        assert!(limiter.try_acquire("room-e"));
        // Now the global budget is genuinely exhausted.
        assert!(!limiter.try_acquire("room-f"));
    }

    #[test]
    fn rtasr_limiter_prune_drops_idle_rooms() {
        let limiter = RtasrRateLimiter::new(1_000.0, 60.0);
        assert!(limiter.try_acquire("room-a"));
        assert_eq!(limiter.per_room.len(), 1);
        // Before a full refill window the entry is retained.
        limiter.prune_idle();
        assert_eq!(limiter.per_room.len(), 1);
        // After the bucket fully refills it is dropped. 60 tokens at 1/sec
        // refills in ~60s; advance the stored timestamp well past that.
        if let Some(mut entry) = limiter.per_room.get_mut("room-a") {
            entry.last_refill = Instant::now() - Duration::from_secs(120);
        }
        limiter.prune_idle();
        assert!(limiter.per_room.is_empty());
    }

    #[test]
    fn room_id_validation_accepts_valid_ids() {
        assert!(room_id_is_valid("abc123"));
        assert!(room_id_is_valid("room-1"));
        assert!(room_id_is_valid("A_B.C~D"));
        assert!(room_id_is_valid(&"x".repeat(256)));
    }

    #[test]
    fn room_id_validation_rejects_invalid_ids() {
        assert!(!room_id_is_valid(""));
        assert!(!room_id_is_valid(&"x".repeat(257)));
        assert!(!room_id_is_valid("room id"));
        assert!(!room_id_is_valid("room\x00id"));
        assert!(!room_id_is_valid("room/id"));
        assert!(!room_id_is_valid("room?id"));
    }

    #[test]
    fn formats_connected_logs_for_cli_and_app_roles() {
        assert_eq!(
            connected_log("room-1", Role::Cli, 7),
            "relay: cli connected room=room-1 conn=7"
        );
        assert_eq!(
            connected_log("room-1", Role::App, 8),
            "relay: app connected room=room-1 conn=8"
        );
    }

    #[test]
    fn formats_rejected_join_log_with_reason() {
        assert_eq!(
            rejected_log("room-1", Role::Cli, 9, "join room/role mismatch"),
            "relay: rejected role=cli room=room-1 conn=9 reason=join room/role mismatch"
        );
    }

    #[test]
    fn escape_log_value_neutralizes_control_chars_and_caps_length() {
        // Newlines / control chars are rendered as escape sequences so a crafted
        // room_id cannot forge a second log line.
        assert_eq!(
            escape_log_value("room\n[2026-01-01] INFO relaycat: forged"),
            "room\\n[2026-01-01] INFO relaycat: forged"
        );
        assert_eq!(escape_log_value("\r\t"), "\\r\\t");
        // A valid room_id charset is passed through unchanged.
        assert_eq!(escape_log_value("room-1.A_b~Z"), "room-1.A_b~Z");
        // Oversized values are truncated with an ellipsis marker.
        let long = "a".repeat(500);
        let escaped = escape_log_value(&long);
        assert_eq!(escaped, format!("{}…", "a".repeat(128)));
    }

    #[test]
    fn relaycat_log_line_uses_timestamp_level_and_message() {
        assert_eq!(
            format_relaycat_log_line(
                "2026-05-20 08:45:30",
                "WARN",
                "relay: rejected role=cli room=room-1 conn=9 reason=join timed out"
            ),
            "[2026-05-20 08:45:30] WARN relaycat: relay: rejected role=cli room=room-1 conn=9 reason=join timed out"
        );
    }

    #[test]
    fn formats_periodic_relay_stats_with_app_labels() {
        let hub = HubStats {
            rooms_total: 4,
            cli_connected_total: 3,
            app_connected_total: 2,
            cli_paired: 2,
            cli_idle: 1,
            app_paired: 2,
            app_without_cli: 0,
            slow_consumer_evictions_total: 5,
            slow_consumer_evictions_cli: 1,
            slow_consumer_evictions_app: 4,
            slow_consumer_discarded_bytes: 2048,
            outbound_queue_high_water: 64,
        };
        let process = ProcessStats {
            cpu_percent: 1.25,
            memory_rss_mb: 42.5,
        };

        assert_eq!(
            relay_stats_log(hub, process, 7),
            "relay stats: cpu_percent=1.2 memory_rss_mb=42.5 rooms_total=4 cli_connected_total=3 app_connected_total=2 cli_paired=2 cli_idle=1 app_paired=2 app_without_cli=0 rooms_expired_last_interval=7 slow_consumer_evictions_total=5 slow_consumer_evictions_cli=1 slow_consumer_evictions_app=4 slow_consumer_discarded_bytes=2048 outbound_queue_high_water=64"
        );
    }

    #[test]
    fn rtasr_url_request_rejects_disabled_config() {
        let state = AppState::default();

        let err = issue_xfyun_rtasr_url(
            &state,
            RtasrUrlRequest {
                room_id: "room-1".to_string(),
                relay_admission: URL_SAFE_NO_PAD.encode([7; 32]),
            },
        )
        .expect_err("disabled config is rejected");

        assert_eq!(err, (StatusCode::SERVICE_UNAVAILABLE, "rtasr_disabled"));
    }

    #[tokio::test]
    async fn rtasr_url_request_requires_matching_room_admission() {
        let state = AppState::with_config(RelayConfig {
            xfyun: crate::config::XfyunConfig {
                rtasr: Some(crate::config::XfyunRtasrConfig {
                    enabled: true,
                    app_id: "app".to_string(),
                    api_secret: "secret".to_string(),
                    api_key: "key".to_string(),
                    endpoint: "wss://office-api-ast-dx.iflyaisol.com/ast/communicate/v1"
                        .to_string(),
                    audio_encode: "pcm_s16le".to_string(),
                    lang: "autodialect".to_string(),
                    recognized_language: "cn".to_string(),
                    samplerate: "16000".to_string(),
                    max_duration_seconds: 60,
                    url_ttl_seconds: 60,
                }),
            },
            limits: crate::config::LimitsConfig::default(),
        });
        let (cli_tx, _) = mpsc::channel(1);
        state
            .hub
            .join(
                JoinRequest::new("room-1", Role::Cli, 1, [1; 32], None, cli_tx)
                    .with_relay_admission([7; 32]),
            )
            .expect("protected cli joins");

        let err = issue_xfyun_rtasr_url(
            &state,
            RtasrUrlRequest {
                room_id: "room-1".to_string(),
                relay_admission: URL_SAFE_NO_PAD.encode([8; 32]),
            },
        )
        .expect_err("mismatched admission is rejected");

        assert_eq!(err, (StatusCode::UNAUTHORIZED, "unauthorized"));
    }

    #[tokio::test]
    async fn rtasr_url_request_issues_signed_url_for_authorized_room() {
        let state = AppState::with_config(RelayConfig {
            xfyun: crate::config::XfyunConfig {
                rtasr: Some(crate::config::XfyunRtasrConfig {
                    enabled: true,
                    app_id: "app".to_string(),
                    api_secret: "secret".to_string(),
                    api_key: "key".to_string(),
                    endpoint: "wss://office-api-ast-dx.iflyaisol.com/ast/communicate/v1"
                        .to_string(),
                    audio_encode: "pcm_s16le".to_string(),
                    lang: "autodialect".to_string(),
                    recognized_language: "cn".to_string(),
                    samplerate: "16000".to_string(),
                    max_duration_seconds: 60,
                    url_ttl_seconds: 60,
                }),
            },
            limits: crate::config::LimitsConfig::default(),
        });
        let (cli_tx, _) = mpsc::channel(1);
        state
            .hub
            .join(
                JoinRequest::new("room-1", Role::Cli, 1, [1; 32], None, cli_tx)
                    .with_relay_admission([7; 32]),
            )
            .expect("protected cli joins");

        let response = issue_xfyun_rtasr_url(
            &state,
            RtasrUrlRequest {
                room_id: "room-1".to_string(),
                relay_admission: URL_SAFE_NO_PAD.encode([7; 32]),
            },
        )
        .expect("authorized room gets url");

        assert!(
            response
                .url
                .starts_with("wss://office-api-ast-dx.iflyaisol.com/ast/communicate/v1?")
        );
        assert!(response.url.contains("appId=app"));
        assert!(response.url.contains("accessKeyId=key"));
        assert!(response.url.contains("&signature="));
        assert_eq!(response.max_duration_seconds, 60);
        assert!(response.expires_at >= current_unix_seconds());
    }

    #[tokio::test]
    async fn rtasr_url_request_is_rate_limited_after_per_room_budget() {
        let state = AppState::with_config(RelayConfig {
            xfyun: crate::config::XfyunConfig {
                rtasr: Some(crate::config::XfyunRtasrConfig {
                    enabled: true,
                    app_id: "app".to_string(),
                    api_secret: "secret".to_string(),
                    api_key: "key".to_string(),
                    endpoint: "wss://office-api-ast-dx.iflyaisol.com/ast/communicate/v1"
                        .to_string(),
                    audio_encode: "pcm_s16le".to_string(),
                    lang: "autodialect".to_string(),
                    recognized_language: "cn".to_string(),
                    samplerate: "16000".to_string(),
                    max_duration_seconds: 60,
                    url_ttl_seconds: 60,
                }),
            },
            limits: crate::config::LimitsConfig::default(),
        });
        let (cli_tx, _) = mpsc::channel(1);
        state
            .hub
            .join(
                JoinRequest::new("room-1", Role::Cli, 1, [1; 32], None, cli_tx)
                    .with_relay_admission([7; 32]),
            )
            .expect("protected cli joins");

        let request = || RtasrUrlRequest {
            room_id: "room-1".to_string(),
            relay_admission: URL_SAFE_NO_PAD.encode([7; 32]),
        };

        // The per-room burst capacity worth of requests all succeed.
        let budget = RTASR_PER_ROOM_REQUESTS_PER_MINUTE as usize;
        for _ in 0..budget {
            issue_xfyun_rtasr_url(&state, request()).expect("within budget");
        }

        // The next request from the same room is rejected with 429.
        let err = issue_xfyun_rtasr_url(&state, request())
            .expect_err("over per-room budget is rejected");
        assert_eq!(err, (StatusCode::TOO_MANY_REQUESTS, "rate_limited"));
    }
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/", get(root_handler))
        .route("/ws", get(ws_handler))
        .route("/api/asr/xfyun/rtasr-url", post(xfyun_rtasr_url_handler))
        .with_state(state)
}

pub async fn serve(addr: SocketAddr) -> anyhow::Result<()> {
    serve_with_state(addr, AppState::default()).await
}

pub async fn serve_with_state(addr: SocketAddr, state: AppState) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind relay listener on {addr}"))?;
    relaycat_log("INFO", format!("relay listening on ws://{addr}/ws"));

    tokio::spawn(cleanup_rooms_loop(state.clone()));
    tokio::spawn(relay_stats_loop(state.clone()));

    axum::serve(listener, app(state))
        .await
        .context("relay server failed")
}

async fn cleanup_rooms_loop(state: AppState) {
    let mut interval = tokio::time::interval_at(
        tokio::time::Instant::now() + ROOM_CLEANUP_INTERVAL,
        ROOM_CLEANUP_INTERVAL,
    );
    loop {
        interval.tick().await;
        let expired = state.hub.cleanup_inactive_rooms(ROOM_TTL);
        if expired > 0 {
            state.record_expired_rooms(expired);
        }
        state.rtasr_limiter.prune_idle();
    }
}

async fn relay_stats_loop(state: AppState) {
    let mut interval =
        tokio::time::interval_at(tokio::time::Instant::now() + STATS_INTERVAL, STATS_INTERVAL);
    let mut collector = ProcessStatsCollector::new();
    loop {
        interval.tick().await;
        let hub_stats = state.hub.stats();
        let process_stats = collector.refresh();
        let expired = state.take_expired_rooms_since_stats();
        relaycat_log("INFO", relay_stats_log(hub_stats, process_stats, expired));
    }
}

async fn ws_handler(
    State(state): State<AppState>,
    Query(query): Query<WsQuery>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.max_message_size(MAX_BINARY_FRAME_BYTES)
        .max_frame_size(MAX_BINARY_FRAME_BYTES)
        .on_upgrade(move |socket| handle_socket(state, query, socket))
}

async fn root_handler() -> impl IntoResponse {
    Json(HealthResponse {
        status: "running",
        version: format!("v{}", env!("CARGO_PKG_VERSION")),
    })
}

async fn xfyun_rtasr_url_handler(
    State(state): State<AppState>,
    Json(request): Json<RtasrUrlRequest>,
) -> impl IntoResponse {
    match issue_xfyun_rtasr_url(&state, request) {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err((status, error)) => (status, Json(ApiError { error })).into_response(),
    }
}

fn issue_xfyun_rtasr_url(
    state: &AppState,
    request: RtasrUrlRequest,
) -> Result<RtasrUrlResponse, (StatusCode, &'static str)> {
    let Some(config) = state.config.rtasr() else {
        return Err((StatusCode::SERVICE_UNAVAILABLE, "rtasr_disabled"));
    };
    if !room_id_is_valid(&request.room_id) {
        return Err((StatusCode::BAD_REQUEST, "invalid_room_id"));
    }
    let relay_admission = decode_relay_admission(&request.relay_admission)
        .map_err(|_| (StatusCode::BAD_REQUEST, "invalid_relay_admission"))?;
    match state
        .hub
        .check_room_admission(&request.room_id, relay_admission)
    {
        AdmissionCheck::Authorized => {}
        AdmissionCheck::Unauthorized => {
            return Err((StatusCode::UNAUTHORIZED, "unauthorized"));
        }
        AdmissionCheck::RoomMissing => return Err((StatusCode::NOT_FOUND, "room_not_found")),
        AdmissionCheck::RoomUnprotected => {
            return Err((StatusCode::UNAUTHORIZED, "room_unprotected"));
        }
    }

    // Rate-limit only after admission so unauthorized probes cannot drain the
    // budget away from legitimate callers.
    if !state.rtasr_limiter.try_acquire(&request.room_id) {
        return Err((StatusCode::TOO_MANY_REQUESTS, "rate_limited"));
    }

    Ok(signed_rtasr_url(config, current_unix_seconds()))
}

fn decode_relay_admission(value: &str) -> Result<[u8; 32], ()> {
    let bytes = URL_SAFE_NO_PAD.decode(value).map_err(|_| ())?;
    bytes.try_into().map_err(|_| ())
}

fn current_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn binary_frame_within_limit(len: usize, max_bytes: usize) -> bool {
    len <= max_bytes
}

fn room_id_is_valid(room_id: &str) -> bool {
    !room_id.is_empty()
        && room_id.len() <= 256
        && room_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~'))
}

async fn handle_socket(state: AppState, query: WsQuery, socket: WebSocket) {
    let limits = &state.config.limits;
    let conn_id = state.next_conn_id();
    let (outbound_tx, outbound_rx) =
        mpsc::channel(limits.outbound_channel_capacity.max(1));
    // Dedicated eviction channel: never shares capacity with the ordinary
    // outbound queue, so a close reason can still be delivered when that
    // queue is full.
    let (close_signal_tx, close_signal_rx) = mpsc::channel(1);
    let mut socket = socket;
    let connected_at = Instant::now();

    // Increment connection counter immediately; the guard ensures it is
    // decremented on every exit path (early return or normal completion).
    let prev_connections = state.active_connections.fetch_add(1, Ordering::Relaxed);
    let _conn_guard = ConnectionGuard(Arc::clone(&state.active_connections));

    // Validate room_id before it reaches any log line. The query value is
    // percent-decoded by axum, so an unvalidated room_id can carry newlines or
    // other control characters that forge log entries. Once validated its
    // charset is restricted to `[A-Za-z0-9._~-]`, which is log-safe.
    if !room_id_is_valid(&query.room_id) {
        relaycat_log(
            "WARN",
            rejected_log(
                &escape_log_value(&query.room_id),
                query.role,
                conn_id,
                "invalid room_id",
            ),
        );
        let _ = send_error(&mut socket, "invalid room_id").await;
        return;
    }

    if prev_connections >= limits.max_concurrent_connections {
        relaycat_log(
            "WARN",
            rejected_log(
                &query.room_id,
                query.role,
                conn_id,
                "server at connection limit",
            ),
        );
        let _ = send_error(&mut socket, "server at connection limit").await;
        return;
    }

    relaycat_log(
        "INFO",
        format!(
            "websocket upgraded role={} room={} conn={}",
            role_label(query.role),
            query.room_id,
            conn_id
        ),
    );

    let bytes = match tokio::time::timeout(JOIN_FRAME_TIMEOUT, socket.next()).await {
        Err(_elapsed) => {
            relaycat_log(
                "WARN",
                rejected_log(&query.room_id, query.role, conn_id, "join timed out"),
            );
            let _ = send_error(&mut socket, "join timed out").await;
            return;
        }
        Ok(Some(Ok(Message::Binary(bytes)))) => bytes,
        Ok(Some(Ok(Message::Close(_)))) | Ok(None) => {
            relaycat_log(
                "WARN",
                rejected_log(&query.room_id, query.role, conn_id, "closed before join"),
            );
            return;
        }
        Ok(Some(Ok(_))) => {
            relaycat_log(
                "WARN",
                rejected_log(
                    &query.room_id,
                    query.role,
                    conn_id,
                    "initial frame was not binary join",
                ),
            );
            return;
        }
        Ok(Some(Err(error))) => {
            relaycat_log(
                "WARN",
                rejected_log(
                    &query.room_id,
                    query.role,
                    conn_id,
                    &format!("websocket receive failed: {error}"),
                ),
            );
            return;
        }
    };
    if !binary_frame_within_limit(bytes.len(), limits.max_binary_frame_bytes) {
        relaycat_log(
            "WARN",
            rejected_log(
                &query.room_id,
                query.role,
                conn_id,
                "initial binary frame exceeds size limit",
            ),
        );
        let _ = send_error(&mut socket, "binary frame exceeds size limit").await;
        return;
    }

    let join = decode_frame(&bytes);
    let Ok(OuterFrame::Join {
        room_id,
        role,
        device_pubkey,
        pairing_token_proof,
        relay_admission,
        connection_salt,
    }) = join
    else {
        relaycat_log(
            "WARN",
            rejected_log(
                &query.room_id,
                query.role,
                conn_id,
                "initial binary frame was not valid join",
            ),
        );
        let _ = send_error(&mut socket, "initial binary frame was not valid join").await;
        return;
    };
    if room_id != query.room_id || role != query.role {
        relaycat_log(
            "WARN",
            rejected_log(
                &query.room_id,
                query.role,
                conn_id,
                &format!(
                    "join room/role mismatch joined_room={} joined_role={}",
                    escape_log_value(&room_id),
                    role_label(role)
                ),
            ),
        );
        let _ = send_error(&mut socket, "join room/role mismatch").await;
        return;
    }

    relaycat_log(
        "DEBUG",
        format!(
            "relay: join decoded room={} role={} conn={} proof={} salt={}",
            query.room_id,
            role_label(role),
            conn_id,
            if pairing_token_proof.is_some() {
                "present"
            } else {
                "absent"
            },
            if connection_salt.is_some() {
                "present"
            } else {
                "absent"
            },
        ),
    );

    let mut request = JoinRequest::new(
        query.room_id.clone(),
        query.role,
        conn_id,
        device_pubkey,
        pairing_token_proof,
        outbound_tx,
    );
    if let Some(relay_admission) = relay_admission {
        request = request.with_relay_admission(relay_admission);
    }
    request = request
        .with_connection_salt(connection_salt)
        .with_close_signal(close_signal_tx);
    let join_result = state.hub.join(request);
    match join_result {
        Err(error) => {
            relaycat_log(
                "WARN",
                rejected_log(
                    &query.room_id,
                    query.role,
                    conn_id,
                    &format!("room join failed: {error}"),
                ),
            );
            let _ = send_error(&mut socket, &error.to_string()).await;
            return;
        }
        Ok(true) => {
            relaycat_log(
                "INFO",
                format!(
                    "relay: app evicted previous connection room={} conn={}",
                    query.room_id, conn_id
                ),
            );
        }
        Ok(false) => {}
    }

    relaycat_log("INFO", connected_log(&query.room_id, query.role, conn_id));
    relay_socket_frames(
        state.clone(),
        query.clone(),
        conn_id,
        socket,
        outbound_rx,
        close_signal_rx,
    )
    .await;
    state.hub.leave(&query.room_id, query.role, conn_id);
    relaycat_log(
        "INFO",
        disconnected_log(&query.room_id, query.role, conn_id, connected_at.elapsed()),
    );
}

async fn relay_socket_frames(
    state: AppState,
    query: WsQuery,
    conn_id: ConnId,
    mut socket: WebSocket,
    mut outbound_rx: mpsc::Receiver<OuterFrame>,
    mut close_signal_rx: mpsc::Receiver<CloseSignal>,
) {
    let limits = state.config.limits.clone();
    let mut rate_limiter = InboundRateLimiter::new(
        limits.inbound_messages_per_second,
        limits.inbound_bytes_per_second,
    );
    let mut heartbeat = tokio::time::interval(CLIENT_PING_INTERVAL);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_client_seen = Instant::now();

    loop {
        tokio::select! {
            signal = close_signal_rx.recv() => {
                let Some(signal) = signal else {
                    break;
                };
                let reason = signal.close_reason();
                relaycat_log(
                    "WARN",
                    format!(
                        "relay: closing evicted connection role={} room={} conn={} reason={}",
                        role_label(query.role),
                        query.room_id,
                        conn_id,
                        reason
                    ),
                );
                let _ = socket
                    .send(Message::Close(Some(CloseFrame {
                        code: EVICTION_CLOSE_CODE,
                        reason: reason.into(),
                    })))
                    .await;
                break;
            }
            _ = heartbeat.tick() => {
                if last_client_seen.elapsed() > CLIENT_PONG_TIMEOUT {
                    let _ = send_error(&mut socket, "client heartbeat timeout").await;
                    break;
                }
                if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
            outbound = outbound_rx.recv() => {
                let Some(frame) = outbound else {
                    relaycat_log(
                        "WARN",
                        format!(
                            "relay: outbound channel closed role={} room={} conn={}",
                            role_label(query.role),
                            query.room_id,
                            conn_id
                        ),
                    );
                    break;
                };
                if send_frame(&mut socket, &frame).await.is_err() {
                    relaycat_log(
                        "WARN",
                        format!(
                            "relay: websocket send failed role={} room={} conn={}",
                            role_label(query.role),
                            query.room_id,
                            conn_id
                        ),
                    );
                    break;
                }
            }
            inbound = socket.next() => {
                let Some(message) = inbound else {
                    break;
                };

                match message {
                    Ok(Message::Binary(bytes)) => {
                        last_client_seen = Instant::now();
                        if !binary_frame_within_limit(bytes.len(), limits.max_binary_frame_bytes) {
                            let _ = send_error(&mut socket, "binary frame exceeds size limit").await;
                            relaycat_log(
                                "WARN",
                                format!(
                                    "relay: closing oversized inbound frame role={} room={} conn={} bytes={}",
                                    role_label(query.role),
                                    query.room_id,
                                    conn_id,
                                    bytes.len()
                                ),
                            );
                            break;
                        }
                        if !rate_limiter.allow(bytes.len()) {
                            let _ = send_error(&mut socket, "inbound rate limit exceeded").await;
                            relaycat_log(
                                "WARN",
                                format!(
                                    "relay: closing rate-limited connection role={} room={} conn={}",
                                    role_label(query.role),
                                    query.room_id,
                                    conn_id
                                ),
                            );
                            break;
                        }
                        if let Ok(frame) = decode_frame(&bytes) {
                            match frame {
                                OuterFrame::Ping => {
                                    let _ = send_frame(&mut socket, &OuterFrame::Pong).await;
                                }
                                OuterFrame::Pong => {}
                                OuterFrame::Data { .. } | OuterFrame::Ack { .. } => {
                                    let _ = state
                                        .hub
                                        .forward(&query.room_id, query.role, conn_id, frame);
                                }
                                // Drop protocol-control frames (Join, PeerJoined, PeerLeft,
                                // Error) — a peer must not inject these into the relay stream
                                // to manipulate the other peer's state machine.
                                _ => {}
                            }
                        }
                    }
                    Ok(Message::Close(close_frame)) => {
                        relaycat_log(
                            "INFO",
                            format!(
                                "relay: websocket close received role={} room={} conn={} close={:?}",
                                role_label(query.role),
                                query.room_id,
                                conn_id,
                                close_frame
                            ),
                        );
                        break;
                    }
                    Ok(Message::Text(text)) => {
                        last_client_seen = Instant::now();
                        // Text frames carry no protocol meaning, but they must
                        // still be size- and rate-limited so they cannot be used
                        // to bypass the binary-frame guards and flood the relay.
                        if !binary_frame_within_limit(text.len(), limits.max_binary_frame_bytes)
                            || !rate_limiter.allow(text.len())
                        {
                            let _ = send_error(&mut socket, "inbound rate limit exceeded").await;
                            relaycat_log(
                                "WARN",
                                format!(
                                    "relay: closing flooding text frames role={} room={} conn={}",
                                    role_label(query.role),
                                    query.room_id,
                                    conn_id
                                ),
                            );
                            break;
                        }
                    }
                    Ok(Message::Ping(payload)) | Ok(Message::Pong(payload)) => {
                        last_client_seen = Instant::now();
                        // Charge ping/pong floods against the same budget.
                        if !rate_limiter.allow(payload.len()) {
                            let _ =
                                send_error(&mut socket, "inbound rate limit exceeded").await;
                            relaycat_log(
                                "WARN",
                                format!(
                                    "relay: closing flooding control frames role={} room={} conn={}",
                                    role_label(query.role),
                                    query.room_id,
                                    conn_id
                                ),
                            );
                            break;
                        }
                    }
                    Err(error) => {
                        relaycat_log(
                            "WARN",
                            format!(
                                "relay: websocket receive failed role={} room={} conn={} error={}",
                                role_label(query.role),
                                query.room_id,
                                conn_id,
                                error
                            ),
                        );
                        break;
                    }
                }
            }
        }
    }
}

/// A simple request-count token bucket: one token per request, refilled
/// continuously at `refill_per_sec` up to `capacity`.
#[derive(Debug)]
struct TokenBucket {
    tokens: f64,
    capacity: f64,
    refill_per_sec: f64,
    last_refill: Instant,
}

impl TokenBucket {
    fn new(capacity: f64, refill_per_sec: f64) -> Self {
        Self {
            tokens: capacity,
            capacity,
            refill_per_sec,
            last_refill: Instant::now(),
        }
    }

    fn try_acquire_at(&mut self, now: Instant) -> bool {
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.last_refill = now;
        self.tokens = (self.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Returns a previously-acquired token to the bucket (capped at capacity).
    /// Used to undo a global acquisition when a later, narrower check denies the
    /// same request, so the global budget is only spent on requests that pass
    /// every tier.
    fn refund(&mut self) {
        self.tokens = (self.tokens + 1.0).min(self.capacity);
    }

    /// True once the bucket has fully refilled, i.e. it has seen no traffic for
    /// at least a full window. Used to drop idle per-room entries.
    fn is_idle_at(&self, now: Instant) -> bool {
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        (self.tokens + elapsed * self.refill_per_sec) >= self.capacity
    }
}

/// Two-tier rate limiter for the rtasr-url endpoint: a process-wide bucket that
/// caps total upstream signing cost, plus an independent per-room bucket so a
/// single room cannot consume the whole global budget. Per-room entries are
/// created lazily (only for already-authorized rooms) and pruned once idle.
#[derive(Debug)]
struct RtasrRateLimiter {
    global: Mutex<TokenBucket>,
    per_room: DashMap<String, TokenBucket>,
    per_room_capacity: f64,
    per_room_refill_per_sec: f64,
}

impl RtasrRateLimiter {
    fn new(global_per_minute: f64, per_room_per_minute: f64) -> Self {
        Self {
            global: Mutex::new(TokenBucket::new(global_per_minute, global_per_minute / 60.0)),
            per_room: DashMap::new(),
            per_room_capacity: per_room_per_minute,
            per_room_refill_per_sec: per_room_per_minute / 60.0,
        }
    }

    /// Consumes one token for `room_id`. Returns `false` when either the global
    /// or the per-room budget is exhausted.
    ///
    /// The global budget is tried first so a request denied by the global cap
    /// never creates a per-room entry. If the global token is granted but the
    /// per-room budget is then exhausted, the global token is refunded — a room
    /// throttled by its own cap must not be able to drain the shared global
    /// budget away from other rooms.
    fn try_acquire(&self, room_id: &str) -> bool {
        let now = Instant::now();
        {
            let mut global = self.global.lock().expect("rtasr global limiter poisoned");
            if !global.try_acquire_at(now) {
                return false;
            }
        }
        let acquired = {
            let mut bucket = self.per_room.entry(room_id.to_string()).or_insert_with(|| {
                TokenBucket::new(self.per_room_capacity, self.per_room_refill_per_sec)
            });
            bucket.try_acquire_at(now)
        };
        if !acquired {
            self.global
                .lock()
                .expect("rtasr global limiter poisoned")
                .refund();
        }
        acquired
    }

    /// Drops per-room buckets that have fully refilled (no recent requests) so
    /// the map does not grow unbounded across the lifetime of the process.
    fn prune_idle(&self) {
        let now = Instant::now();
        self.per_room.retain(|_, bucket| !bucket.is_idle_at(now));
    }
}

#[derive(Debug)]
struct InboundRateLimiter {
    message_tokens: f64,
    byte_tokens: f64,
    max_messages_per_second: f64,
    max_bytes_per_second: f64,
    last_refill: Instant,
}

impl InboundRateLimiter {
    fn new(max_messages_per_second: u32, max_bytes_per_second: usize) -> Self {
        Self {
            message_tokens: f64::from(max_messages_per_second),
            byte_tokens: max_bytes_per_second as f64,
            max_messages_per_second: f64::from(max_messages_per_second),
            max_bytes_per_second: max_bytes_per_second as f64,
            last_refill: Instant::now(),
        }
    }

    fn allow(&mut self, bytes: usize) -> bool {
        self.allow_at(bytes, Instant::now())
    }

    fn allow_at(&mut self, bytes: usize, now: Instant) -> bool {
        self.refill(now);
        if self.message_tokens < 1.0 || self.byte_tokens < bytes as f64 {
            return false;
        }
        self.message_tokens -= 1.0;
        self.byte_tokens -= bytes as f64;
        true
    }

    fn refill(&mut self, now: Instant) {
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.last_refill = now;
        self.message_tokens = (self.message_tokens + elapsed * self.max_messages_per_second)
            .min(self.max_messages_per_second);
        self.byte_tokens =
            (self.byte_tokens + elapsed * self.max_bytes_per_second).min(self.max_bytes_per_second);
    }
}

fn encode_outbound_frame(frame: &OuterFrame) -> anyhow::Result<Vec<u8>> {
    let bytes = encode_frame(frame).context("failed to encode outbound relay frame")?;
    if !binary_frame_within_limit(bytes.len(), MAX_BINARY_FRAME_BYTES) {
        relaycat_log(
            "WARN",
            &format!(
                "relay: refusing oversized outbound frame bytes={}",
                bytes.len()
            ),
        );
        anyhow::bail!(
            "outbound relay frame exceeds {} bytes: {}",
            MAX_BINARY_FRAME_BYTES,
            bytes.len()
        );
    }
    Ok(bytes)
}

async fn send_frame(socket: &mut WebSocket, frame: &OuterFrame) -> anyhow::Result<()> {
    let bytes = encode_outbound_frame(frame)?;
    socket.send(Message::Binary(bytes.into())).await?;
    Ok(())
}

async fn send_error(socket: &mut WebSocket, message: &str) -> anyhow::Result<()> {
    send_frame(
        socket,
        &OuterFrame::Error {
            message: message.to_string(),
        },
    )
    .await
}

/// `DEBUG`-level lines are verbose diagnostics (e.g. per-join salt presence) and
/// are suppressed unless `RELAYCAT_DEBUG` is set to a truthy value, so normal
/// operation stays quiet while the detail is one env var away when triaging.
fn relaycat_debug_enabled() -> bool {
    matches!(
        std::env::var("RELAYCAT_DEBUG")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn relaycat_log(level: &str, message: impl AsRef<str>) {
    if level == "DEBUG" && !relaycat_debug_enabled() {
        return;
    }
    eprintln!(
        "{}",
        format_relaycat_log_line(&local_timestamp(), level, message.as_ref())
    );
}

fn format_relaycat_log_line(timestamp: &str, level: &str, message: &str) -> String {
    format!("[{timestamp}] {level} relaycat: {message}")
}

/// Escapes an untrusted value before it is interpolated into a log line so a
/// crafted `room_id` cannot inject control characters / newlines to forge log
/// entries. Control characters are rendered as escape sequences and the result
/// is capped so an oversized value cannot flood the log.
fn escape_log_value(value: &str) -> String {
    const MAX_CHARS: usize = 128;
    let mut escaped: String = value.chars().take(MAX_CHARS).flat_map(char::escape_debug).collect();
    if value.chars().nth(MAX_CHARS).is_some() {
        escaped.push('…');
    }
    escaped
}

fn local_timestamp() -> String {
    #[cfg(not(unix))]
    {
        return current_unix_seconds().to_string();
    }

    #[cfg(unix)]
    {
        let mut now: libc::time_t = 0;
        unsafe {
            libc::time(&mut now);
        }

        let mut local = MaybeUninit::<libc::tm>::uninit();
        let local_ptr = unsafe { libc::localtime_r(&now, local.as_mut_ptr()) };
        if local_ptr.is_null() {
            return now.to_string();
        }

        let mut buffer = [0 as libc::c_char; 20];
        let written = unsafe {
            libc::strftime(
                buffer.as_mut_ptr(),
                buffer.len(),
                c"%Y-%m-%d %H:%M:%S".as_ptr(),
                local_ptr,
            )
        };
        if written == 0 {
            return now.to_string();
        }

        unsafe { CStr::from_ptr(buffer.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    }
}

fn connected_log(room_id: &str, role: Role, conn_id: ConnId) -> String {
    format!(
        "relay: {} connected room={} conn={}",
        role_label(role),
        room_id,
        conn_id
    )
}

fn rejected_log(room_id: &str, role: Role, conn_id: ConnId, reason: &str) -> String {
    format!(
        "relay: rejected role={} room={} conn={} reason={}",
        role_label(role),
        room_id,
        conn_id,
        reason
    )
}

fn disconnected_log(room_id: &str, role: Role, conn_id: ConnId, duration: Duration) -> String {
    format!(
        "relay: {} disconnected room={} conn={} duration_ms={}",
        role_label(role),
        room_id,
        conn_id,
        duration.as_millis()
    )
}

fn relay_stats_log(
    hub: HubStats,
    process: ProcessStats,
    rooms_expired_last_interval: u64,
) -> String {
    format!(
        "relay stats: cpu_percent={:.1} memory_rss_mb={:.1} rooms_total={} cli_connected_total={} app_connected_total={} cli_paired={} cli_idle={} app_paired={} app_without_cli={} rooms_expired_last_interval={} slow_consumer_evictions_total={} slow_consumer_evictions_cli={} slow_consumer_evictions_app={} slow_consumer_discarded_bytes={} outbound_queue_high_water={}",
        process.cpu_percent,
        process.memory_rss_mb,
        hub.rooms_total,
        hub.cli_connected_total,
        hub.app_connected_total,
        hub.cli_paired,
        hub.cli_idle,
        hub.app_paired,
        hub.app_without_cli,
        rooms_expired_last_interval,
        hub.slow_consumer_evictions_total,
        hub.slow_consumer_evictions_cli,
        hub.slow_consumer_evictions_app,
        hub.slow_consumer_discarded_bytes,
        hub.outbound_queue_high_water
    )
}

fn role_label(role: Role) -> &'static str {
    match role {
        Role::Cli => "cli",
        Role::App => "app",
    }
}
