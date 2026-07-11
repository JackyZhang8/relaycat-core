use super::*;

/// Build the [`TargetCommand`] for a confirmed selection and either pair over
/// the relay or run locally in a PTY.
pub(crate) async fn run_session(launch: Launch, language: CliLanguage) -> Result<SessionOutcome> {
    let target = match &launch.tool {
        ToolKind::Builtin(kind) => {
            TargetCommand::for_kind(kind, launch.project.clone(), launch.relay.clone())?
        }
        ToolKind::Custom {
            name,
            program,
            args,
        } => TargetCommand::for_custom(
            name,
            program.clone(),
            args.clone(),
            launch.project.clone(),
            launch.relay.clone(),
        )?,
    };

    match target.relay.clone() {
        Some(relay_options) => {
            // `prepare_secure_pairing` prints the pairing URL + QR; we then wait
            // in place for the app to join before handing off to the PTY relay.
            let prepared = match relay::prepare_secure_pairing(target, relay_options).await {
                Ok(prepared) => prepared,
                Err(err) if relay::is_cancelled_by_user(&err) => {
                    return Ok(SessionOutcome::BackToTools);
                }
                Err(err) => {
                    eprintln!("\n{err:#}\n");
                    eprintln!("{}", relay_error_prompt_for_language(language));
                    let action = relay::wait_for_pairing_control_action().await?;
                    return Ok(session_outcome_for_pairing_control(&launch, action));
                }
            };
            eprintln!("\n{}\n", pairing_wait_message_for_language(language));
            match relay::run_secure_pairing_prepared_with_launcher_controls(prepared).await? {
                relay::PairingRunOutcome::Completed => Ok(SessionOutcome::Completed),
                relay::PairingRunOutcome::Control(action) => {
                    Ok(session_outcome_for_pairing_control(&launch, action))
                }
                relay::PairingRunOutcome::PairingFailed(err) => {
                    eprintln!("\n{err:#}\n");
                    eprintln!("{}", relay_error_prompt_for_language(language));
                    let action = relay::wait_for_pairing_control_action().await?;
                    Ok(session_outcome_for_pairing_control(&launch, action))
                }
            }
        }
        None => {
            pty::run_interactive(target)?;
            Ok(SessionOutcome::Completed)
        }
    }
}

pub(crate) fn session_outcome_for_pairing_control(
    launch: &Launch,
    action: relay::PairingControlAction,
) -> SessionOutcome {
    match action {
        relay::PairingControlAction::BackOneLevel => SessionOutcome::BackToRelay(launch.clone()),
        relay::PairingControlAction::BackToTools => SessionOutcome::BackToTools,
    }
}

pub(crate) fn relay_error_prompt_for_language(language: CliLanguage) -> &'static str {
    language.t(
        "Press Esc to go back and edit the relay URL, or Ctrl-C to return to tools.",
        "按 Esc 返回修改 relay 地址，按 Ctrl-C 回到工具选择。",
    )
}

pub(crate) fn pairing_wait_message_for_language(language: CliLanguage) -> &'static str {
    language.t(
        "Waiting:\n  Scan the QR in the RelayCat app to join.\n\nControls:\n  Esc    edit relay URL\n  Ctrl-C return to tools",
        "等待:\n  请用 RelayCat app 扫描二维码并加入。\n\n操作:\n  Esc    修改 relay 地址\n  Ctrl-C 返回工具选择",
    )
}

pub(crate) fn spawn_update_notice_check() -> mpsc::Receiver<UpdateNotice> {
    let (tx, rx) = mpsc::channel();
    tokio::spawn(async move {
        if let Some(notice) = update::check_notice_for_tui().await {
            let _ = tx.send(notice);
        }
    });
    rx
}

pub(crate) fn poll_update_notice(
    rx: &mpsc::Receiver<UpdateNotice>,
    update_notice: &mut Option<UpdateNotice>,
) {
    if update_notice.is_some() {
        return;
    }
    if let Ok(notice) = rx.try_recv() {
        *update_notice = Some(notice);
    }
}
