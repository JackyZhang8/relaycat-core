use std::time::Duration;

use semver::Version;
use serde::Deserialize;
use url::Url;

const SUPPORTED_RELAY_PROTOCOLS: &[u16] = &[1];

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct RelayHealthResponse {
    pub(crate) status: String,
    pub(crate) version: String,
    #[serde(default)]
    pub(crate) protocol_version: Option<u16>,
    #[serde(default)]
    pub(crate) min_cli_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RelayCompatibilityStatus {
    Compatible,
    Incompatible,
    Unverified,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RelayCompatibilityCheck {
    pub(crate) status: RelayCompatibilityStatus,
    pub(crate) action: Option<String>,
    pub(crate) server_version: Option<String>,
    pub(crate) min_cli_version: Option<String>,
    pub(crate) reason: String,
}

impl RelayCompatibilityCheck {
    fn compatible(server_version: &str, min_cli_version: Option<String>) -> Self {
        Self {
            status: RelayCompatibilityStatus::Compatible,
            action: None,
            server_version: Some(server_version.to_string()),
            min_cli_version,
            reason: "compatible".to_string(),
        }
    }

    fn incompatible(
        action: &str,
        server_version: &str,
        min_cli_version: Option<String>,
        reason: String,
    ) -> Self {
        Self {
            status: RelayCompatibilityStatus::Incompatible,
            action: Some(action.to_string()),
            server_version: Some(server_version.to_string()),
            min_cli_version,
            reason,
        }
    }

    fn unverified(reason: String) -> Self {
        Self {
            status: RelayCompatibilityStatus::Unverified,
            action: None,
            server_version: None,
            min_cli_version: None,
            reason,
        }
    }
}

pub(crate) fn evaluate_relay_health(
    health: &RelayHealthResponse,
    cli_version: &str,
) -> RelayCompatibilityCheck {
    let protocol = health.protocol_version.unwrap_or(1);
    let min_supported = *SUPPORTED_RELAY_PROTOCOLS
        .iter()
        .min()
        .expect("protocol list is nonempty");
    let max_supported = *SUPPORTED_RELAY_PROTOCOLS
        .iter()
        .max()
        .expect("protocol list is nonempty");

    if !SUPPORTED_RELAY_PROTOCOLS.contains(&protocol) {
        let action = if protocol > max_supported {
            "upgrade_cli"
        } else if protocol < min_supported {
            "upgrade_server"
        } else {
            "upgrade_cli"
        };
        return RelayCompatibilityCheck::incompatible(
            action,
            &health.version,
            health.min_cli_version.clone(),
            format!("unsupported relay protocol {protocol}"),
        );
    }

    if let Some(minimum) = health.min_cli_version.as_deref() {
        let current = Version::parse(cli_version);
        let required = Version::parse(minimum);
        if matches!((current, required), (Ok(current), Ok(required)) if current < required) {
            return RelayCompatibilityCheck::incompatible(
                "upgrade_cli",
                &health.version,
                health.min_cli_version.clone(),
                format!("CLI {cli_version} is below required {minimum}"),
            );
        }
    }

    RelayCompatibilityCheck::compatible(&health.version, health.min_cli_version.clone())
}

pub(crate) fn relay_health_url(relay_url: &str) -> Result<Url, String> {
    let mut url = Url::parse(relay_url).map_err(|error| error.to_string())?;
    let target_scheme = match url.scheme() {
        "wss" => "https",
        "ws" => "http",
        "https" => "https",
        "http" => "http",
        scheme => return Err(format!("unsupported relay URL scheme {scheme}")),
    };
    url.set_scheme(target_scheme)
        .map_err(|_| "failed to map relay URL scheme".to_string())?;

    let path = url.path().trim_end_matches('/');
    let base = path.strip_suffix("/ws").unwrap_or(path);
    let health_path = if base.is_empty() {
        "/".to_string()
    } else {
        format!("{base}/")
    };
    url.set_path(&health_path);
    url.set_fragment(None);
    Ok(url)
}

pub(crate) async fn probe_relay_compatibility(
    relay_url: &str,
    cli_version: &str,
) -> RelayCompatibilityCheck {
    let health_url = match relay_health_url(relay_url) {
        Ok(url) => url,
        Err(error) => return RelayCompatibilityCheck::unverified(error),
    };
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
    {
        Ok(client) => client,
        Err(error) => return RelayCompatibilityCheck::unverified(error.to_string()),
    };
    let response = match client.get(health_url).send().await {
        Ok(response) => response,
        Err(error) => return RelayCompatibilityCheck::unverified(error.to_string()),
    };
    if !response.status().is_success() {
        return RelayCompatibilityCheck::unverified(format!(
            "health endpoint returned {}",
            response.status()
        ));
    }
    match response.json::<RelayHealthResponse>().await {
        Ok(health) => evaluate_relay_health(&health, cli_version),
        Err(error) => RelayCompatibilityCheck::unverified(error.to_string()),
    }
}
