use base64::{Engine as _, engine::general_purpose::STANDARD};
use hmac::{Hmac, Mac};
use serde::Serialize;
use sha1::Sha1;

use crate::config::XfyunRtasrConfig;

type HmacSha1 = Hmac<Sha1>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RtasrUrlResponse {
    pub url: String,
    pub expires_at: u64,
    pub max_duration_seconds: u64,
}

pub fn signed_rtasr_url(config: &XfyunRtasrConfig, now_unix_seconds: u64) -> RtasrUrlResponse {
    let utc = format_utc_timestamp(now_unix_seconds);
    let params = [
        ("accessKeyId", config.api_key.as_str()),
        ("appId", config.app_id.as_str()),
        ("audio_encode", config.audio_encode.as_str()),
        ("lang", config.lang.as_str()),
        ("recognized_language", config.recognized_language.as_str()),
        ("samplerate", config.samplerate.as_str()),
        ("utc", utc.as_str()),
    ];
    let canonical = params
        .iter()
        .map(|(key, value)| {
            format!(
                "{}={}",
                percent_encode_query_value(key),
                percent_encode_query_value(value)
            )
        })
        .collect::<Vec<_>>()
        .join("&");
    let mut mac = HmacSha1::new_from_slice(config.api_secret.as_bytes())
        .expect("HMAC accepts any key length");
    mac.update(canonical.as_bytes());
    let signa = percent_encode_query_value(&STANDARD.encode(mac.finalize().into_bytes()));
    let mut query = params
        .iter()
        .map(|(key, value)| format!("{key}={}", percent_encode_query_value(value)))
        .collect::<Vec<_>>()
        .join("&");
    query.push_str("&signature=");
    query.push_str(&signa);
    let separator = if config.endpoint.contains('?') {
        '&'
    } else {
        '?'
    };
    let url = format!("{}{separator}{query}", config.endpoint);

    RtasrUrlResponse {
        url,
        expires_at: now_unix_seconds + config.url_ttl_seconds,
        max_duration_seconds: config.max_duration_seconds,
    }
}

fn format_utc_timestamp(unix_seconds: u64) -> String {
    let days = (unix_seconds / 86_400) as i64;
    let seconds_of_day = unix_seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}+0000")
}

fn civil_from_days(days_since_unix_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_unix_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if m <= 2 { 1 } else { 0 };
    (year, m as u32, d as u32)
}

fn percent_encode_query_value(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signs_rtasr_url_with_xfyun_algorithm() {
        let config = XfyunRtasrConfig {
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
            url_ttl_seconds: 30,
        };

        let response = signed_rtasr_url(&config, 1_700_000_000);

        assert_eq!(response.expires_at, 1_700_000_030);
        assert_eq!(response.max_duration_seconds, 60);
        assert_eq!(
            response.url,
            "wss://office-api-ast-dx.iflyaisol.com/ast/communicate/v1?accessKeyId=key&appId=app&audio_encode=pcm_s16le&lang=autodialect&recognized_language=cn&samplerate=16000&utc=2023-11-14T22%3A13%3A20%2B0000&signature=FJVJqdzVqK%2FWwLfdnFngsVHImO0%3D"
        );
    }

    #[test]
    fn formats_utc_timestamp_without_external_time_dependency() {
        assert_eq!(format_utc_timestamp(0), "1970-01-01T00:00:00+0000");
        assert_eq!(
            format_utc_timestamp(1_700_000_000),
            "2023-11-14T22:13:20+0000"
        );
    }

    #[test]
    fn percent_encodes_reserved_query_bytes() {
        assert_eq!(percent_encode_query_value("abc+/=\n"), "abc%2B%2F%3D%0A");
    }
}
