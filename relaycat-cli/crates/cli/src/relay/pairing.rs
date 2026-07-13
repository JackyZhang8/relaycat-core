use super::*;

/// When set, the human-facing pairing block (header, log path, pairing URL,
/// QR PNG path and ASCII QR) is suppressed from stdout/stderr. The desktop GUI
/// hosts the relay session inside an embedded terminal and renders the QR /
/// pairing URL / hints in a native popup instead, so printing them into the PTY
/// only clutters the GUI's background terminal. The pairing URL is still
/// surfaced to the GUI host via [`emit_pairing_url_for_gui`].
static GUI_PAIRING_QUIET: AtomicBool = AtomicBool::new(false);

/// OSC identifier used to hand the pairing URL to the GUI host without
/// rendering it: unknown OSC sequences are silently consumed by xterm.js (and
/// most terminals), so the URL never shows up in the embedded terminal while
/// the GUI's PTY reader can still scrape it for the pairing popup.
const GUI_PAIRING_URL_OSC: &str = "9779";

/// Enable quiet pairing output for the current process. Called by the desktop
/// GUI relay child (see [`crate::gui_bridge::run_relay_child`]) so the QR /
/// pairing URL / hints go to the native popup only, never the background term.
pub fn set_gui_pairing_quiet() {
    GUI_PAIRING_QUIET.store(true, Ordering::Relaxed);
}

pub(crate) fn gui_pairing_quiet() -> bool {
    GUI_PAIRING_QUIET.load(Ordering::Relaxed)
}

/// Emit the pairing URL wrapped in a private OSC sequence so the GUI host can
/// scrape it without it being rendered in the embedded terminal.
fn emit_pairing_url_for_gui(url: &str) {
    print!("\x1b]{GUI_PAIRING_URL_OSC};{url}\x07");
    let _ = io::stdout().flush();
}

/// OSC identifier used to tell the GUI host the negotiated child PTY size
/// (`"<cols>;<rows>"`). Like [`GUI_PAIRING_URL_OSC`] it is a private code that
/// xterm.js ignores unless a handler is registered; the GUI registers one so it
/// can shrink its terminal grid to the negotiated size and letterbox the extra
/// desktop-window space, keeping the desktop layout identical to the phone.
const GUI_REMOTE_SIZE_OSC: &str = "9780";

/// Push the negotiated child PTY `(cols, rows)` to the GUI host over the pipe
/// bridge. No-op off the Windows GUI pipe bridge (real terminals and the
/// macOS/Linux PTY-hosted relay child have no GUI grid to resize).
///
/// The whole sequence is written under a single stdout lock so it can never
/// interleave with the PTY-output copy thread mid-escape (both use the global
/// stdout lock per `write_all`).
pub(crate) fn emit_remote_size_for_gui(cols: u16, rows: u16) {
    if !crate::gui_bridge::gui_bridge_pipe_mode() || cols == 0 || rows == 0 {
        return;
    }
    write_remote_size_osc(cols, rows);
}

/// Tell the GUI host to stop pinning/letterboxing its terminal to a negotiated
/// grid and just fit the desktop window again. Sent when the relay enters local
/// (Ctrl-G) mode, where the child PTY is sized to the full desktop window, so
/// the desktop terminal must use its full width rather than the phone grid.
/// No-op off the Windows GUI pipe bridge. Encoded as `"0;0"` on the same private
/// OSC the GUI already handles.
pub(crate) fn clear_remote_size_for_gui() {
    if !crate::gui_bridge::gui_bridge_pipe_mode() {
        return;
    }
    write_remote_size_osc(0, 0);
}

/// Write the remote-size OSC under a single stdout lock so it can never
/// interleave with the PTY-output copy thread mid-escape.
fn write_remote_size_osc(cols: u16, rows: u16) {
    let seq = format!("\x1b]{GUI_REMOTE_SIZE_OSC};{cols};{rows}\x07");
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    let _ = handle.write_all(seq.as_bytes());
    let _ = handle.flush();
}

/// Write the pairing URL to the GUI's poll file (see
/// [`crate::pairing_store::gui_pairing_url_path`]). Written to a temp sibling
/// and renamed so the GUI never reads a half-written file. Best-effort: failure
/// just leaves the GUI on the OSC channel.
fn write_gui_pairing_url_file(
    project_dir: &std::path::Path,
    kind: &crate::command::SessionKind,
    url: &str,
) {
    let gui_session_id = crate::gui_bridge::gui_session_id();
    let path = crate::pairing_store::gui_pairing_url_path_for(
        project_dir,
        kind,
        gui_session_id.as_deref(),
    );
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("txt.tmp");
    if std::fs::write(&tmp, url.as_bytes()).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// The pairing-success log line the GUI log tail watches for. Tagged with the
/// GUI tab/session id (when hosted by the GUI) so a tab only reacts to its own
/// relay child pairing, never a sibling tab's in the same shared `cli.log`.
fn secure_session_established_log() -> String {
    match crate::gui_bridge::gui_session_id() {
        Some(id) => format!("secure session established gui_session={id}"),
        None => "secure session established".to_string(),
    }
}

/// A secure pairing that has connected to the relay and emitted its pairing
/// QR/URL, but has not yet seen the mobile app join. Produced by
/// [`prepare_secure_pairing`] and consumed by [`run_secure_pairing_prepared`].
///
/// Splitting pairing into prepare/run lets callers (e.g. the TUI launcher)
/// decide when to display the QR and when to block on the peer joining,
/// without duplicating the material-caching and relay-connection logic.
pub struct PreparedSecurePairing {
    target: TargetCommand,
    material: crate::pairing::PairingMaterial,
    handshake: Arc<CliSecureHandshake>,
    reconnect: Arc<RelayTransportReconnect>,
    ws_writer: WsWriter,
    ws_reader: WsReader,
}

impl PreparedSecurePairing {
    /// The pairing material the mobile app must scan to join this session.
    pub fn material(&self) -> &crate::pairing::PairingMaterial {
        &self.material
    }

    /// The target command associated with this prepared pairing.
    pub fn target(&self) -> &TargetCommand {
        &self.target
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PairingControlAction {
    BackOneLevel,
    BackToTools,
}

#[derive(Debug)]
pub(crate) enum PairingRunOutcome {
    Completed,
    Control(PairingControlAction),
    PairingFailed(anyhow::Error),
}

pub(crate) fn pairing_control_action_for_key(key: KeyEvent) -> Option<PairingControlAction> {
    if key.kind != KeyEventKind::Press {
        return None;
    }
    match key.code {
        KeyCode::Esc => Some(PairingControlAction::BackOneLevel),
        KeyCode::Char('c') | KeyCode::Char('C')
            if key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            Some(PairingControlAction::BackToTools)
        }
        _ => None,
    }
}

pub(crate) fn poll_pairing_control_action(
    timeout: Duration,
) -> Result<Option<PairingControlAction>> {
    if !event::poll(timeout).context("failed to poll pairing control input")? {
        return Ok(None);
    }
    let event = event::read().context("failed to read pairing control input")?;
    let Event::Key(key) = event else {
        return Ok(None);
    };
    Ok(pairing_control_action_for_key(key))
}

pub(crate) async fn next_pairing_control_action() -> Result<PairingControlAction> {
    loop {
        if let Some(action) =
            tokio::task::spawn_blocking(|| poll_pairing_control_action(Duration::from_millis(50)))
                .await
                .context("pairing control input task panicked")??
        {
            return Ok(action);
        }
    }
}

pub(crate) async fn wait_for_pairing_control_action() -> Result<PairingControlAction> {
    let _control_mode = PairingControlModeGuard::new();
    tokio::select! {
        action = next_pairing_control_action() => action,
        _ = tokio::signal::ctrl_c() => Ok(PairingControlAction::BackToTools),
    }
}

pub(crate) fn is_cancelled_by_user(err: &anyhow::Error) -> bool {
    err.chain()
        .any(|cause| cause.to_string() == cancelled_by_user_message())
}

/// Load the cached latest pairing material for `project_dir`/`session_kind`, or
/// generate and persist a fresh one when no usable session exists.
///
/// A cached session is reused only when it targets the same `relay_url` and has
/// not expired. Relay runs should use [`generate_pairing_material_for_run`] so
/// concurrent sessions in the same project do not share a room; this helper is
/// kept for callers that explicitly want the latest persisted session.
pub fn load_or_generate_pairing_material(
    project_dir: &Path,
    relay_url: &str,
    session_kind: &SessionKind,
) -> Result<(KeyPair, crate::pairing::PairingMaterial)> {
    let session_path = session_file_path(project_dir, session_kind);
    let stored = load_session(&session_path)?;
    let now_unix = current_unix_timestamp();
    match stored {
        Some(session) if session.relay_url == relay_url && !session.is_expired_at(now_unix) => {
            Ok((session.key_pair(), session.material()))
        }
        Some(session) if session.relay_url == relay_url => {
            relaycat_log(
                "INFO",
                "cached secure pairing expired; generated a new pairing session",
            );
            generate_and_store_pairing_material(project_dir, &session_path, relay_url, session_kind)
        }
        Some(_) | None => {
            generate_and_store_pairing_material(project_dir, &session_path, relay_url, session_kind)
        }
    }
}

/// Generate fresh pairing material for one relay run and persist it as the
/// latest session so `relaycat qr` can re-display the most recent QR.
pub fn generate_pairing_material_for_run(
    project_dir: &Path,
    relay_url: &str,
    session_kind: &SessionKind,
) -> Result<(KeyPair, crate::pairing::PairingMaterial)> {
    let (cli_keypair, material) =
        generate_pairing_material_for_run_attempt(relay_url, session_kind);
    let session_path = session_file_path(project_dir, session_kind);
    store_pairing_material_for_run(
        project_dir,
        &session_path,
        &material,
        *cli_keypair.private(),
    )?;
    Ok((cli_keypair, material))
}

pub(crate) fn generate_and_store_pairing_material(
    project_dir: &Path,
    session_path: &Path,
    relay_url: &str,
    session_kind: &SessionKind,
) -> Result<(KeyPair, crate::pairing::PairingMaterial)> {
    let (cli_keypair, material) =
        generate_pairing_material_for_run_attempt(relay_url, session_kind);
    store_pairing_material_for_run(project_dir, session_path, &material, *cli_keypair.private())?;
    Ok((cli_keypair, material))
}

pub(crate) fn generate_pairing_material_for_run_attempt(
    relay_url: &str,
    session_kind: &SessionKind,
) -> (KeyPair, crate::pairing::PairingMaterial) {
    let cli_keypair = KeyPair::generate();
    let material = generate_pairing_material_for_public_key_and_kind(
        relay_url,
        cli_keypair.public(),
        session_kind.clone(),
    );
    (cli_keypair, material)
}

pub(crate) fn store_pairing_material_for_run(
    project_dir: &Path,
    session_path: &Path,
    material: &crate::pairing::PairingMaterial,
    cli_private_key: [u8; 32],
) -> Result<()> {
    save_session(
        session_path,
        &StoredPairingSession::new(material.clone(), cli_private_key),
    )?;
    let _ = ensure_gitignore(project_dir);
    Ok(())
}

/// Connect to the relay and emit the pairing QR/URL, returning a
/// [`PreparedSecurePairing`] that still needs the mobile app to join before the
/// PTY relay can start.
pub async fn prepare_secure_pairing(
    target: TargetCommand,
    relay: RelayOptions,
) -> Result<PreparedSecurePairing> {
    let language = CliLanguage::from_system_locale();
    let quiet = gui_pairing_quiet();
    target.validate()?;
    let project_dir = project_dir(target.cwd.as_deref())?;
    if !quiet {
        eprintln!("{}", secure_pairing_header_message(language));
    }
    // Always initialise the log file (the GUI tails it for pairing status),
    // but only print its path in interactive (non-GUI) mode.
    if let Some(log_path) = init_log_file(&project_dir) {
        if !quiet {
            eprintln!("\n{}", relaycat_logs_message(&log_path, language));
        }
    }
    let (cli_keypair, material) =
        generate_pairing_material_for_run_attempt(&relay.url, &target.session_kind);
    let cli_private_key = *cli_keypair.private();
    let session_path = session_file_path(&project_dir, &target.session_kind);
    let ws_url = build_ws_url(&relay.url, &material.room_id, "cli");

    let handshake = Arc::new(CliSecureHandshake::new(
        material.room_id.clone(),
        cli_keypair,
        material.pairing_token.clone(),
    ));
    let reconnect = Arc::new(RelayTransportReconnect::Secure {
        ws_url,
        handshake: handshake.clone(),
    });
    let (ws_writer, ws_reader) = tokio::select! {
        result = reconnect.connect() => result,
        _ = tokio::signal::ctrl_c() => {
            anyhow::bail!(cancelled_by_user_message())
        }
    }
    .with_context(|| initial_relay_unavailable_message(&target))?;

    store_pairing_material_for_run(&project_dir, &session_path, &material, cli_private_key)?;

    relaycat_log("INFO", "secure pairing URL:");
    let pairing_url = crate::pairing::pairing_url(&material);
    if quiet {
        // GUI host: surface the URL invisibly for the native popup and keep the
        // embedded terminal free of QR / URL / hint clutter.
        emit_pairing_url_for_gui(&pairing_url);
        // The OSC above is dropped by Windows' ConPTY parser, so also write the
        // URL to a file the GUI polls — the reliable cross-platform channel.
        write_gui_pairing_url_file(&project_dir, &target.session_kind, &pairing_url);
    } else {
        eprintln!("\n{}", pairing_url_message(&pairing_url, language));
        emit_pairing_png(&project_dir, &material, language);
        #[cfg(not(windows))]
        eprintln!("\n{}", crate::pairing::render_pairing_qr(&material)?);
    }

    Ok(PreparedSecurePairing {
        target,
        material,
        handshake,
        reconnect,
        ws_writer,
        ws_reader,
    })
}

/// Block until the mobile app joins the relay room, then hand off to the secure
/// PTY relay loop.
pub async fn run_secure_pairing_prepared(prepared: PreparedSecurePairing) -> Result<()> {
    let PreparedSecurePairing {
        target,
        material,
        handshake,
        reconnect,
        mut ws_writer,
        mut ws_reader,
    } = prepared;

    loop {
        let message = tokio::select! {
            message = ws_reader.next() => message,
            _ = tokio::signal::ctrl_c() => {
                anyhow::bail!(cancelled_by_user_message())
            }
        };
        if secure_pairing_transport_action(&message) == SecurePairingTransportAction::Reconnect {
            relaycat_log(
                "WARN",
                "relay transport closed before app joined; reconnecting and keeping pairing QR active",
            );
            let (next_writer, next_reader) = reconnect_pairing_transport(&reconnect).await?;
            ws_writer = next_writer;
            ws_reader = next_reader;
            continue;
        }
        let Some(Ok(message)) = message else {
            continue;
        };
        let Message::Binary(bytes) = message else {
            continue;
        };
        let frame = decode_frame(&bytes).context("failed to decode relay frame")?;
        if let OuterFrame::Error { message, code } = &frame {
            anyhow::bail!("{}", relay_error_description(message, *code));
        }
        log_received_peer_joined(&frame);
        if let Some(keys) = handshake.accept_peer_joined(&frame)? {
            relaycat_log("INFO", secure_session_established_log());
            crate::recent_store::remember_recent_target_or_warn(&target);
            return run_secure_pty_relay(
                target,
                material.room_id.clone(),
                keys,
                handshake,
                reconnect,
                ws_writer,
                ws_reader,
            )
            .await;
        }
    }
}

pub(crate) async fn run_secure_pairing_prepared_with_launcher_controls(
    prepared: PreparedSecurePairing,
) -> Result<PairingRunOutcome> {
    let PreparedSecurePairing {
        target,
        material,
        handshake,
        reconnect,
        mut ws_writer,
        mut ws_reader,
    } = prepared;
    let control_mode = PairingControlModeGuard::new();

    loop {
        let message = tokio::select! {
            message = ws_reader.next() => message,
            action = next_pairing_control_action() => {
                return Ok(PairingRunOutcome::Control(action?));
            }
            _ = tokio::signal::ctrl_c() => {
                return Ok(PairingRunOutcome::Control(PairingControlAction::BackToTools));
            }
        };
        if secure_pairing_transport_action(&message) == SecurePairingTransportAction::Reconnect {
            relaycat_log(
                "WARN",
                "relay transport closed before app joined; reconnecting and keeping pairing QR active",
            );
            match reconnect_pairing_transport_with_launcher_controls(&reconnect).await? {
                PairingReconnectOutcome::Connected(next_writer, next_reader) => {
                    ws_writer = next_writer;
                    ws_reader = next_reader;
                }
                PairingReconnectOutcome::Control(action) => {
                    return Ok(PairingRunOutcome::Control(action));
                }
            }
            continue;
        }
        let Some(Ok(message)) = message else {
            continue;
        };
        let Message::Binary(bytes) = message else {
            continue;
        };
        let frame = match decode_frame(&bytes).context("failed to decode relay frame") {
            Ok(frame) => frame,
            Err(err) => return Ok(PairingRunOutcome::PairingFailed(err)),
        };
        if let OuterFrame::Error { message, code } = &frame {
            return Ok(PairingRunOutcome::PairingFailed(anyhow::anyhow!(
                "{}",
                relay_error_description(message, *code)
            )));
        }
        log_received_peer_joined(&frame);
        let keys = match handshake.accept_peer_joined(&frame) {
            Ok(keys) => keys,
            Err(err) => return Ok(PairingRunOutcome::PairingFailed(err)),
        };
        if let Some(keys) = keys {
            relaycat_log("INFO", secure_session_established_log());
            crate::recent_store::remember_recent_target_or_warn(&target);
            drop(control_mode);
            run_secure_pty_relay(
                target,
                material.room_id.clone(),
                keys,
                handshake,
                reconnect,
                ws_writer,
                ws_reader,
            )
            .await?;
            return Ok(PairingRunOutcome::Completed);
        }
    }
}

pub(crate) enum PairingReconnectOutcome {
    Connected(WsWriter, WsReader),
    Control(PairingControlAction),
}

pub(crate) async fn reconnect_pairing_transport_with_launcher_controls(
    reconnect: &RelayTransportReconnect,
) -> Result<PairingReconnectOutcome> {
    let mut attempt = 1_u32;
    loop {
        tokio::select! {
            _ = tokio::time::sleep(RelayTransportReconnectPolicy::retry_delay(attempt)) => {}
            action = next_pairing_control_action() => {
                return Ok(PairingReconnectOutcome::Control(action?));
            }
            _ = tokio::signal::ctrl_c() => {
                return Ok(PairingReconnectOutcome::Control(PairingControlAction::BackToTools));
            }
        }
        let connect_result = tokio::select! {
            result = reconnect.connect() => result,
            action = next_pairing_control_action() => {
                return Ok(PairingReconnectOutcome::Control(action?));
            }
            _ = tokio::signal::ctrl_c() => {
                return Ok(PairingReconnectOutcome::Control(PairingControlAction::BackToTools));
            }
        };
        match connect_result {
            Ok((writer, reader)) => {
                relaycat_log("INFO", "reconnected to relay while waiting for app join");
                return Ok(PairingReconnectOutcome::Connected(writer, reader));
            }
            Err(err) => {
                relaycat_log(
                    "WARN",
                    format!("relay reconnect attempt {attempt} failed before app join: {err:#}"),
                );
                attempt = attempt.saturating_add(1);
            }
        }
    }
}

pub(crate) async fn reconnect_pairing_transport(
    reconnect: &RelayTransportReconnect,
) -> Result<(WsWriter, WsReader)> {
    let mut attempt = 1_u32;
    loop {
        tokio::select! {
            _ = tokio::time::sleep(RelayTransportReconnectPolicy::retry_delay(attempt)) => {}
            _ = tokio::signal::ctrl_c() => {
                anyhow::bail!(cancelled_by_user_message())
            }
        }
        let connect_result = tokio::select! {
            result = reconnect.connect() => result,
            _ = tokio::signal::ctrl_c() => {
                anyhow::bail!(cancelled_by_user_message())
            }
        };
        match connect_result {
            Ok((writer, reader)) => {
                relaycat_log("INFO", "reconnected to relay while waiting for app join");
                return Ok((writer, reader));
            }
            Err(err) => {
                relaycat_log(
                    "WARN",
                    format!("relay reconnect attempt {attempt} failed before app join: {err:#}"),
                );
                attempt = attempt.saturating_add(1);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SecurePairingTransportAction {
    Continue,
    Reconnect,
}

pub(crate) fn secure_pairing_transport_action(
    message: &Option<Result<Message, tokio_tungstenite::tungstenite::Error>>,
) -> SecurePairingTransportAction {
    match message {
        None | Some(Err(_)) => SecurePairingTransportAction::Reconnect,
        Some(Ok(_)) => SecurePairingTransportAction::Continue,
    }
}

pub async fn run_secure_pairing(target: TargetCommand, relay: RelayOptions) -> Result<()> {
    let prepared = prepare_secure_pairing(target, relay).await?;
    run_secure_pairing_prepared(prepared).await
}

/// Write the pairing QR to a room-scoped PNG so it can be re-opened
/// mid-session, and print its path. On Windows the PNG is also opened in the
/// default image viewer (the terminal there can't render the QR).
pub(crate) fn emit_pairing_png(
    project_dir: &Path,
    material: &crate::pairing::PairingMaterial,
    language: CliLanguage,
) {
    let path = match write_pairing_qr_pngs(project_dir, material) {
        Ok(path) => path,
        Err(err) => {
            eprintln!("failed to write pairing QR PNG: {err:#}");
            return;
        }
    };

    eprintln!("\n{}", pairing_qr_png_written_message(&path, language));

    #[cfg(windows)]
    if let Err(err) = ProcessCommand::new("cmd")
        .args(["/C", "start", ""])
        .arg(&path)
        .spawn()
    {
        eprintln!("failed to open pairing QR PNG {}: {err}", path.display());
    }
}

pub(crate) fn pairing_qr_png_written_message(path: &Path, language: CliLanguage) -> String {
    format!(
        "{}:\n  {}",
        language.t("QR PNG", "二维码 PNG"),
        path.display()
    )
}

#[cfg(unix)]
pub(crate) struct PairingControlModeGuard {
    original: Option<libc::termios>,
}

#[cfg(unix)]
impl PairingControlModeGuard {
    fn new() -> Self {
        Self {
            original: enable_local_raw_terminal_mode(),
        }
    }
}

#[cfg(unix)]
impl Drop for PairingControlModeGuard {
    fn drop(&mut self) {
        if let Some(original) = self.original.take() {
            restore_local_terminal_mode(original);
        }
        disable_local_focus_reporting();
    }
}

#[cfg(not(unix))]
pub(crate) struct PairingControlModeGuard;

#[cfg(not(unix))]
impl PairingControlModeGuard {
    fn new() -> Self {
        Self
    }
}

#[cfg(not(unix))]
impl Drop for PairingControlModeGuard {
    fn drop(&mut self) {
        disable_local_focus_reporting();
    }
}
