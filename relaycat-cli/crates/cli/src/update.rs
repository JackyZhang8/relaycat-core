use std::{
    cmp::Ordering,
    collections::HashMap,
    env, fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{args::UpdateArgs, config, i18n::CliLanguage};

pub const DEFAULT_CLI_MANIFEST_URL: &str = "https://www.relaycat.cn/update/cli.json";
pub const FALLBACK_CLI_MANIFEST_URL: &str = "https://relaycat.app/update/cli.json";

#[derive(Debug, Clone, Deserialize)]
pub struct CliUpdateManifest {
    pub schema: u32,
    pub app: String,
    pub platform: String,
    #[serde(default)]
    pub channel: Option<String>,
    pub latest_version: String,
    #[serde(default)]
    pub min_supported_version: Option<String>,
    #[serde(default)]
    pub mandatory: bool,
    #[serde(default)]
    pub released_at: Option<String>,
    #[serde(default)]
    pub notes: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub downloads: Vec<CliUpdateDownload>,
    #[serde(default)]
    pub homepage_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CliUpdateDownload {
    pub target: String,
    pub url: String,
    pub sha256: String,
    #[serde(default = "default_download_kind")]
    pub kind: String,
    #[serde(default)]
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadInstallPlan {
    pub can_replace_current_exe: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateNotice {
    pub latest_version: String,
    pub current_version: String,
    pub mandatory: bool,
}

fn default_download_kind() -> String {
    "binary".to_string()
}

pub fn select_download_for_target<'a>(
    manifest: &'a CliUpdateManifest,
    target: &str,
) -> Option<&'a CliUpdateDownload> {
    manifest
        .downloads
        .iter()
        .find(|download| download.target == target)
}

pub fn compare_versions(left: &str, right: &str) -> Ordering {
    let left_parts = version_parts(left);
    let right_parts = version_parts(right);
    let max_len = left_parts.len().max(right_parts.len());
    for index in 0..max_len {
        match (left_parts.get(index), right_parts.get(index)) {
            (Some(left), Some(right)) => {
                let ordering = left.cmp(right);
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
            (Some(extra), None) => {
                return compare_extra_version_tail(extra, &left_parts[index + 1..], true);
            }
            (None, Some(extra)) => {
                return compare_extra_version_tail(extra, &right_parts[index + 1..], false);
            }
            (None, None) => return Ordering::Equal,
        }
    }
    Ordering::Equal
}

fn compare_extra_version_tail(
    first: &VersionPart,
    rest: &[VersionPart],
    extra_is_left: bool,
) -> Ordering {
    for extra in std::iter::once(first).chain(rest.iter()) {
        if extra.is_zero() {
            continue;
        }
        return match (extra.is_text(), extra_is_left) {
            (true, true) => Ordering::Less,
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Greater,
            (false, false) => Ordering::Less,
        };
    }
    Ordering::Equal
}

impl DownloadInstallPlan {
    pub fn for_target(target: &str, kind: &str) -> Self {
        Self {
            can_replace_current_exe: kind == "binary" && !target.contains("windows"),
        }
    }
}

pub async fn check_notice_for_tui() -> Option<UpdateNotice> {
    let target = current_target_triple();
    let manifest_url = DEFAULT_CLI_MANIFEST_URL.to_string();
    tokio::task::spawn_blocking(move || {
        let primary = fetch_url_to_string_with_timeout(&manifest_url, 2)
            .and_then(|text| notice_from_manifest_text(&text, env!("CARGO_PKG_VERSION"), &target));
        match primary {
            Ok(Some(notice)) => Ok(Some(notice)),
            // A reachable manifest may still lack a usable download for this
            // target (e.g. not yet published on this mirror); try the fallback
            // before concluding there is no update.
            Ok(None) | Err(_) => fetch_url_to_string_with_timeout(FALLBACK_CLI_MANIFEST_URL, 2)
                .and_then(|text| {
                    notice_from_manifest_text(&text, env!("CARGO_PKG_VERSION"), &target)
                })
                .or(primary),
        }
    })
    .await
    .ok()
    .and_then(Result::ok)
    .flatten()
}

pub async fn run_update_command(args: &UpdateArgs, language: CliLanguage) -> Result<()> {
    let manifest_url = args.manifest_url.clone();
    let download_only = args.download_only;
    tokio::task::spawn_blocking(move || {
        run_update_command_blocking(&manifest_url, download_only, language)
    })
    .await
    .context("update task failed")?
}

fn run_update_command_blocking(
    manifest_url: &str,
    download_only: bool,
    language: CliLanguage,
) -> Result<()> {
    println!(
        "{} {}",
        language.t("Checking update manifest:", "正在检查升级清单："),
        manifest_url
    );
    let text = match fetch_url_to_string(manifest_url) {
        Ok(text) => text,
        Err(err) if manifest_url == DEFAULT_CLI_MANIFEST_URL => {
            println!(
                "{} {}",
                language.t("Primary manifest unreachable, trying:", "主升级清单不可达，改用："),
                FALLBACK_CLI_MANIFEST_URL
            );
            fetch_url_to_string(FALLBACK_CLI_MANIFEST_URL).map_err(|_| err)?
        }
        Err(err) => return Err(err),
    };
    let manifest: CliUpdateManifest =
        serde_json::from_str(&text).context("invalid update manifest")?;
    validate_cli_manifest(&manifest)?;

    let current_version = env!("CARGO_PKG_VERSION");
    if compare_versions(&manifest.latest_version, current_version) != Ordering::Greater {
        println!(
            "{} {}",
            language.t("RelayCat is already up to date:", "RelayCat 已是最新版本："),
            current_version
        );
        return Ok(());
    }

    let target = current_target_triple();
    let download = select_download_for_target(&manifest, &target)
        .with_context(|| format!("no download for target {target}"))?;
    let update_dir = update_download_dir()?;
    fs::create_dir_all(&update_dir)
        .with_context(|| format!("failed to create {}", update_dir.display()))?;
    let file_name = download_file_name(&download.url, &target);
    let output = update_dir.join(file_name);

    println!(
        "{} {} -> {}",
        language.t("Downloading", "正在下载"),
        manifest.latest_version,
        output.display()
    );
    download_url_to_path(&download.url, &output)?;
    verify_sha256(&output, &download.sha256)?;
    verify_download_size(&output, download.size_bytes)?;
    println!("{}", language.t("Checksum verified.", "校验通过。"));

    let plan = DownloadInstallPlan::for_target(&download.target, &download.kind);
    if download_only || !plan.can_replace_current_exe {
        println!(
            "{} {}",
            language.t("Downloaded update:", "升级文件已下载："),
            output.display()
        );
        println!(
            "{}",
            language.t(
                "Install it manually, or publish a direct non-Windows binary to enable self-replacement.",
                "请手动安装；非 Windows 直连二进制可启用自动替换。",
            )
        );
        return Ok(());
    }

    replace_current_exe(&output)?;
    println!(
        "{} {}",
        language.t("RelayCat updated to", "RelayCat 已升级到"),
        manifest.latest_version
    );
    Ok(())
}

fn notice_from_manifest_text(
    text: &str,
    current_version: &str,
    target: &str,
) -> Result<Option<UpdateNotice>> {
    let manifest: CliUpdateManifest =
        serde_json::from_str(text).context("invalid update manifest")?;
    validate_cli_manifest(&manifest)?;
    if compare_versions(&manifest.latest_version, current_version) != Ordering::Greater {
        return Ok(None);
    }
    if select_download_for_target(&manifest, target).is_none() {
        return Ok(None);
    }
    let below_minimum = manifest
        .min_supported_version
        .as_deref()
        .map(|min| compare_versions(current_version, min) == Ordering::Less)
        .unwrap_or(false);
    Ok(Some(UpdateNotice {
        latest_version: manifest.latest_version,
        current_version: current_version.to_string(),
        mandatory: manifest.mandatory || below_minimum,
    }))
}

fn validate_cli_manifest(manifest: &CliUpdateManifest) -> Result<()> {
    if manifest.schema != 1 {
        bail!("unsupported update manifest schema {}", manifest.schema);
    }
    if manifest.app != "relaycat" {
        bail!("manifest app must be relaycat");
    }
    if manifest.platform != "cli" {
        bail!("manifest platform must be cli");
    }
    Ok(())
}

fn current_target_triple() -> String {
    let arch = match env::consts::ARCH {
        "aarch64" => "aarch64",
        "x86_64" => "x86_64",
        "x86" => "i686",
        other => other,
    };
    let os = match env::consts::OS {
        "macos" => "apple-darwin",
        "linux" => "unknown-linux-gnu",
        "windows" => "pc-windows-msvc",
        other => other,
    };
    format!("{arch}-{os}")
}

fn fetch_url_to_string(url: &str) -> Result<String> {
    fetch_url_to_string_with_timeout(url, 30)
}

fn fetch_url_to_string_with_timeout(url: &str, max_time_secs: u64) -> Result<String> {
    let output = curl_command(url, max_time_secs)
        .arg("--output")
        .arg("-")
        .output()
        .with_context(|| format!("failed to run curl for {url}"))?;
    if !output.status.success() {
        bail!(
            "curl failed for {url}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout).context("manifest is not valid UTF-8")
}

fn download_url_to_path(url: &str, output: &Path) -> Result<()> {
    let status = curl_command(url, 300)
        .arg("--output")
        .arg(output)
        .status()
        .with_context(|| format!("failed to run curl for {url}"))?;
    if !status.success() {
        bail!("failed to download {url}");
    }
    Ok(())
}

fn curl_command(url: &str, max_time_secs: u64) -> Command {
    let mut command = Command::new("curl");
    command
        .arg("--fail")
        .arg("--location")
        .arg("--silent")
        .arg("--show-error")
        .arg("--max-time")
        .arg(max_time_secs.to_string())
        .arg(url);
    command
}

fn update_download_dir() -> Result<PathBuf> {
    Ok(config::relaycat_config_dir()?.join("update"))
}

fn download_file_name(url: &str, target: &str) -> String {
    let name = url
        .rsplit('/')
        .next()
        .map(sanitize_download_file_name)
        .unwrap_or_default();
    if name.is_empty() {
        format!("relaycat-{target}")
    } else {
        name
    }
}

fn sanitize_download_file_name(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches(|ch| matches!(ch, '.' | '-' | '_'))
        .to_string()
}

fn verify_sha256(path: &Path, expected_hex: &str) -> Result<()> {
    let actual = sha256_file_hex(path)?;
    if !actual.eq_ignore_ascii_case(expected_hex) {
        bail!("checksum mismatch: expected {expected_hex}, got {actual}");
    }
    Ok(())
}

fn verify_download_size(path: &Path, expected_size: Option<u64>) -> Result<()> {
    let Some(expected_size) = expected_size else {
        return Ok(());
    };
    let actual_size = fs::metadata(path)
        .with_context(|| format!("failed to stat {}", path.display()))?
        .len();
    if actual_size != expected_size {
        bail!("download size mismatch: expected {expected_size}, got {actual_size}");
    }
    Ok(())
}

fn sha256_file_hex(path: &Path) -> Result<String> {
    let mut file =
        fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn replace_current_exe(downloaded: &Path) -> Result<()> {
    let current = env::current_exe().context("failed to locate current executable")?;
    let backup = current.with_extension("old");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(downloaded)
            .with_context(|| format!("failed to stat {}", downloaded.display()))?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(downloaded, permissions)
            .with_context(|| format!("failed to make {} executable", downloaded.display()))?;
    }

    let _ = fs::remove_file(&backup);
    fs::rename(&current, &backup).with_context(|| {
        format!(
            "failed to move {} to {}",
            current.display(),
            backup.display()
        )
    })?;
    if let Err(err) = fs::copy(downloaded, &current) {
        let _ = fs::rename(&backup, &current);
        return Err(err).with_context(|| format!("failed to replace {}", current.display()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&current)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&current, permissions)?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum VersionPart {
    Number(u64),
    Text(String),
}

impl VersionPart {
    fn is_zero(&self) -> bool {
        matches!(self, VersionPart::Number(0))
    }

    fn is_text(&self) -> bool {
        matches!(self, VersionPart::Text(_))
    }
}

impl Ord for VersionPart {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (VersionPart::Number(left), VersionPart::Number(right)) => left.cmp(right),
            (VersionPart::Text(left), VersionPart::Text(right)) => left.cmp(right),
            (VersionPart::Number(_), VersionPart::Text(_)) => Ordering::Greater,
            (VersionPart::Text(_), VersionPart::Number(_)) => Ordering::Less,
        }
    }
}

impl PartialOrd for VersionPart {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn version_parts(version: &str) -> Vec<VersionPart> {
    version
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(|part| {
            part.parse::<u64>()
                .map(VersionPart::Number)
                .unwrap_or_else(|_| VersionPart::Text(part.to_ascii_lowercase()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_file_name_rejects_path_traversal_segments() {
        assert_eq!(
            download_file_name("https://example.com/releases/..", "aarch64-apple-darwin"),
            "relaycat-aarch64-apple-darwin"
        );
        assert_eq!(
            download_file_name(
                "https://example.com/releases/relaycat\\evil",
                "aarch64-apple-darwin",
            ),
            "relaycat-evil"
        );
    }

    #[test]
    fn verify_download_size_rejects_manifest_mismatch() {
        let dir = tempfile_dir("relaycat-update-size");
        let path = dir.join("relaycat");
        fs::write(&path, b"abc").expect("write update");

        assert!(verify_download_size(&path, Some(3)).is_ok());
        let err = verify_download_size(&path, Some(4)).expect_err("size mismatch");
        assert!(err.to_string().contains("download size mismatch"));
        assert!(verify_download_size(&path, None).is_ok());
    }

    fn tempfile_dir(prefix: &str) -> PathBuf {
        let path = env::temp_dir().join(format!(
            "{}-{}-{}",
            prefix,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }
}
