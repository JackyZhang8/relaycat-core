use std::path::Path;
use std::{net::SocketAddr, str::FromStr};

use anyhow::Context;
use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "relaycat-relay")]
#[command(about = "RelayCat websocket relay")]
struct Args {
    #[arg(long, default_value = "127.0.0.1:8787")]
    listen: String,
    #[arg(long)]
    config: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let addr = SocketAddr::from_str(&args.listen)
        .with_context(|| format!("invalid --listen address {}", args.listen))?;

    let state = match args.config {
        Some(path) => {
            let config = relaycat_relay::config::RelayConfig::load(&path)?;
            eprintln!(
                "xfyun.rtasr.enabled: {}",
                config
                    .xfyun
                    .rtasr
                    .as_ref()
                    .map(|rtasr| rtasr.enabled)
                    .unwrap_or(false)
            );
            relaycat_relay::server::AppState::with_config(config)
        }
        None => {
            let default_path = Path::new("config.yaml");
            if default_path.exists() {
                let config = relaycat_relay::config::RelayConfig::load(default_path)?;
                eprintln!(
                    "xfyun.rtasr.enabled: {}",
                    config
                        .xfyun
                        .rtasr
                        .as_ref()
                        .map(|rtasr| rtasr.enabled)
                        .unwrap_or(false)
                );
                relaycat_relay::server::AppState::with_config(config)
            } else {
                relaycat_relay::server::AppState::default()
            }
        }
    };

    relaycat_relay::server::serve_with_state(addr, state).await
}
