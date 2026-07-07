use std::{fs, path::Path};

use anyhow::Context;
use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct RelayConfig {
    #[serde(default)]
    pub xfyun: XfyunConfig,
    #[serde(default)]
    pub limits: LimitsConfig,
}

/// Tunable transport limits. Every field is optional in the config file and
/// falls back (via `#[serde(default)]` + the custom `Default` impl below) to
/// the same values the relay shipped as hard-coded constants, so existing
/// configs keep their previous behavior.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct LimitsConfig {
    /// Maximum simultaneous WebSocket connections across all rooms.
    pub max_concurrent_connections: usize,
    /// Per-connection inbound message rate (messages/second).
    pub inbound_messages_per_second: u32,
    /// Per-connection inbound byte rate (bytes/second).
    pub inbound_bytes_per_second: usize,
    /// Capacity of each connection's outbound channel; when full the slow
    /// recipient is evicted. Clamped to at least 1 at use to avoid a zero-
    /// capacity channel.
    pub outbound_channel_capacity: usize,
    /// Maximum size of a single binary frame, in bytes.
    pub max_binary_frame_bytes: usize,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_concurrent_connections: crate::server::DEFAULT_MAX_CONCURRENT_CONNECTIONS,
            inbound_messages_per_second: crate::server::DEFAULT_INBOUND_MESSAGES_PER_SECOND,
            inbound_bytes_per_second: crate::server::DEFAULT_INBOUND_BYTES_PER_SECOND,
            outbound_channel_capacity: crate::server::DEFAULT_OUTBOUND_CHANNEL_CAPACITY,
            max_binary_frame_bytes: crate::server::MAX_BINARY_FRAME_BYTES,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct XfyunConfig {
    #[serde(default)]
    pub rtasr: Option<XfyunRtasrConfig>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct XfyunRtasrConfig {
    #[serde(default)]
    pub enabled: bool,
    pub app_id: String,
    pub api_secret: String,
    pub api_key: String,
    #[serde(default = "default_rtasr_endpoint")]
    pub endpoint: String,
    #[serde(default = "default_audio_encode")]
    pub audio_encode: String,
    #[serde(default = "default_language")]
    pub lang: String,
    #[serde(default = "default_recognized_language")]
    pub recognized_language: String,
    #[serde(default = "default_samplerate")]
    pub samplerate: String,
    #[serde(default = "default_max_duration_seconds")]
    pub max_duration_seconds: u64,
    #[serde(default = "default_url_ttl_seconds")]
    pub url_ttl_seconds: u64,
}

impl RelayConfig {
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let text = fs::read_to_string(path)
            .with_context(|| format!("failed to read config {}", path.display()))?;
        serde_yaml::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))
    }

    pub fn rtasr(&self) -> Option<&XfyunRtasrConfig> {
        self.xfyun.rtasr.as_ref().filter(|config| config.enabled)
    }
}

fn default_rtasr_endpoint() -> String {
    "wss://office-api-ast-dx.iflyaisol.com/ast/communicate/v1".to_string()
}

fn default_audio_encode() -> String {
    "pcm_s16le".to_string()
}

fn default_language() -> String {
    "autodialect".to_string()
}

fn default_recognized_language() -> String {
    "cn".to_string()
}

fn default_samplerate() -> String {
    "16000".to_string()
}

fn default_max_duration_seconds() -> u64 {
    60
}

fn default_url_ttl_seconds() -> u64 {
    60
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_xfyun_rtasr_config_with_defaults() {
        let config: RelayConfig = serde_yaml::from_str(
            r#"
xfyun:
  rtasr:
    enabled: true
    app_id: app
    api_secret: secret
    api_key: key
"#,
        )
        .expect("parse yaml");

        assert_eq!(
            config.rtasr(),
            Some(&XfyunRtasrConfig {
                enabled: true,
                app_id: "app".to_string(),
                api_secret: "secret".to_string(),
                api_key: "key".to_string(),
                endpoint: "wss://office-api-ast-dx.iflyaisol.com/ast/communicate/v1".to_string(),
                audio_encode: "pcm_s16le".to_string(),
                lang: "autodialect".to_string(),
                recognized_language: "cn".to_string(),
                samplerate: "16000".to_string(),
                max_duration_seconds: 60,
                url_ttl_seconds: 60,
            })
        );
    }

    #[test]
    fn limits_default_to_shipped_constants_when_absent() {
        let config: RelayConfig = serde_yaml::from_str("xfyun: {}").expect("parse yaml");
        assert_eq!(config.limits, LimitsConfig::default());
        assert_eq!(
            config.limits.max_concurrent_connections,
            crate::server::DEFAULT_MAX_CONCURRENT_CONNECTIONS
        );
        assert_eq!(
            config.limits.inbound_messages_per_second,
            crate::server::DEFAULT_INBOUND_MESSAGES_PER_SECOND
        );
        assert_eq!(
            config.limits.max_binary_frame_bytes,
            crate::server::MAX_BINARY_FRAME_BYTES
        );
    }

    #[test]
    fn limits_partial_override_keeps_other_defaults() {
        let config: RelayConfig = serde_yaml::from_str(
            r#"
limits:
  inbound_bytes_per_second: 8000000
  outbound_channel_capacity: 256
"#,
        )
        .expect("parse yaml");

        // Overridden fields take the configured value...
        assert_eq!(config.limits.inbound_bytes_per_second, 8_000_000);
        assert_eq!(config.limits.outbound_channel_capacity, 256);
        // ...while unspecified fields fall back to the shipped defaults.
        assert_eq!(
            config.limits.max_concurrent_connections,
            crate::server::DEFAULT_MAX_CONCURRENT_CONNECTIONS
        );
        assert_eq!(
            config.limits.inbound_messages_per_second,
            crate::server::DEFAULT_INBOUND_MESSAGES_PER_SECOND
        );
    }

    #[test]
    fn disabled_rtasr_config_is_unavailable() {
        let config: RelayConfig = serde_yaml::from_str(
            r#"
xfyun:
  rtasr:
    enabled: false
    app_id: app
    api_secret: secret
    api_key: key
"#,
        )
        .expect("parse yaml");

        assert_eq!(config.rtasr(), None);
    }
}
