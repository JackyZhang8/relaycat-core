use anyhow::{Context, Result};
use relaycat_cli::{
    args::{Cli, Command, ConfigArgs, QrArgs},
    command::{SessionKind, TargetCommand},
    config::{self, Config},
    i18n::CliLanguage,
    pairing,
    pairing_store::{
        StoredPairingSession, current_unix_timestamp, load_session, project_dir, session_file_path,
        write_pairing_qr_pngs,
    },
    pty,
    recent_store::{RecentStore, format_recent_list_for_language, recent_file_path},
    relay, tui, update,
};

fn install_tls_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[tokio::main]
async fn main() -> Result<()> {
    install_tls_provider();
    let language = CliLanguage::from_system_locale();
    let cli = Cli::parse_for_language(language);
    let Some(command) = cli.command.as_ref() else {
        return tui::run().await;
    };
    match command {
        Command::Tui(_) => {
            return tui::run().await;
        }
        Command::Qr(args) => {
            return show_pairing_qr(args, language);
        }
        Command::Recent(_) => {
            let path = recent_file_path()?;
            let store = RecentStore::load(&path)?;
            print!("{}", format_recent_list_for_language(&store, language));
            return Ok(());
        }
        Command::Run(args) => {
            let path = recent_file_path()?;
            let store = RecentStore::load(&path)?;
            let target = store.resolve_target(&args.selector)?;
            let relay_options = target.relay.clone().expect("relay");
            return relay::run_secure_pairing(target, relay_options).await;
        }
        Command::Forget(args) => {
            let path = recent_file_path()?;
            let mut store = RecentStore::load(&path)?;
            let removed = store.forget(&args.selector)?;
            store.save(&path)?;
            println!("{} {}", language.t("forgot", "已删除"), removed.id);
            return Ok(());
        }
        Command::Config(args) => {
            return show_config(args, language);
        }
        Command::Update(args) => {
            return update::run_update_command(args, language).await;
        }
        Command::Shell(_)
        | Command::Claude(_)
        | Command::Codex(_)
        | Command::Opencode(_)
        | Command::Gemini(_)
        | Command::Aider(_)
        | Command::Tool(_) => {}
    }
    let target = TargetCommand::from_cli(&cli)?;

    if let Some(relay_options) = target.relay.clone() {
        relay::run_secure_pairing(target, relay_options).await
    } else {
        pty::run_interactive(target)
    }
}

fn show_config(args: &ConfigArgs, language: CliLanguage) -> Result<()> {
    let path = config::config_file_path()?;
    if args.path {
        println!("{}", path.display());
        return Ok(());
    }
    let config = Config::load(&path)?;
    println!(
        "{}: {}",
        language.t("config file", "配置文件"),
        path.display()
    );
    if !path.exists() {
        println!(
            "{}",
            language.t(
                "(file does not exist yet; showing built-in defaults)",
                "（文件尚不存在；显示内置默认配置）",
            )
        );
    }
    println!("{}", config.to_pretty()?);
    Ok(())
}

fn show_pairing_qr(args: &QrArgs, language: CliLanguage) -> Result<()> {
    let kind = SessionKind::new(args.kind.clone())?;
    let project = project_dir(args.project.as_deref())?;
    let session_path = session_file_path(&project, &kind);
    let stored: StoredPairingSession = load_session(&session_path)?.with_context(|| {
        format!(
            "no pairing session for `{}` in {}; start one first, e.g. `relaycat {} --relay <url>`",
            kind.as_str(),
            project.display(),
            kind.as_str()
        )
    })?;

    let now_unix = current_unix_timestamp();
    let expired = stored.is_expired_at(now_unix);
    if expired {
        eprintln!(
            "{}",
            language.t(
                "warning: this pairing session has expired and will be regenerated on the next `relaycat` run",
                "警告：这个配对会话已过期，将在下次运行 `relaycat` 时重新生成",
            )
        );
    }

    let material = stored.material();
    println!("{}", pairing::pairing_url(&material));
    println!("{}", pairing::render_pairing_qr(&material)?);

    if args.png && !expired {
        let path = write_pairing_qr_pngs(&project, &material)?;
        println!(
            "{}: {}",
            language.t("pairing QR PNG", "配对二维码 PNG"),
            path.display()
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn installs_tls_provider() {
        super::install_tls_provider();
    }
}
