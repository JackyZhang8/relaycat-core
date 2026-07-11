use relaycat_cli::update::{
    CliUpdateManifest, DownloadInstallPlan, compare_versions, select_download_for_target,
};

const MANIFEST: &str = r#"{
  "schema": 1,
  "app": "relaycat",
  "platform": "cli",
  "channel": "stable",
  "latest_version": "0.1.2",
  "min_supported_version": "0.1.0",
  "mandatory": false,
  "released_at": "2026-06-07T00:00:00Z",
  "notes": {
    "zh-Hans": ["修复连接稳定性"],
    "en": ["Improved connection stability"]
  },
  "downloads": [
    {
      "target": "aarch64-apple-darwin",
      "url": "https://example.com/relaycat-0.1.2-aarch64-apple-darwin",
      "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
      "kind": "binary"
    },
    {
      "target": "x86_64-pc-windows-msvc",
      "url": "https://example.com/relaycat-0.1.2-x86_64-pc-windows-msvc.exe",
      "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
      "kind": "binary"
    }
  ],
  "homepage_url": "https://example.com/releases/v0.1.2"
}"#;

#[test]
fn parses_cli_manifest_and_selects_target_download() {
    let manifest: CliUpdateManifest = serde_json::from_str(MANIFEST).unwrap();

    assert_eq!(manifest.latest_version, "0.1.2");
    assert_eq!(
        select_download_for_target(&manifest, "aarch64-apple-darwin")
            .unwrap()
            .url,
        "https://example.com/relaycat-0.1.2-aarch64-apple-darwin"
    );
    assert!(select_download_for_target(&manifest, "x86_64-unknown-linux-gnu").is_none());
}

#[test]
fn compares_semver_like_versions_numerically() {
    assert!(compare_versions("0.2.0", "0.1.9").is_gt());
    assert!(compare_versions("0.1.10", "0.1.2").is_gt());
    assert!(compare_versions("0.10.0", "0.2.9").is_gt());
    assert!(compare_versions("1.0.0", "1.0.0").is_eq());
    assert!(compare_versions("1.0.0-beta.1", "1.0.0-alpha.9").is_gt());
    assert!(compare_versions("1.0.0.0.1", "1.0.0").is_gt());
}

#[test]
fn install_plan_replaces_unix_binaries_but_downloads_windows_binary() {
    let mac_plan = DownloadInstallPlan::for_target("aarch64-apple-darwin", "binary");
    assert!(mac_plan.can_replace_current_exe);

    let windows_plan = DownloadInstallPlan::for_target("x86_64-pc-windows-msvc", "binary");
    assert!(!windows_plan.can_replace_current_exe);

    let archive_plan = DownloadInstallPlan::for_target("aarch64-apple-darwin", "archive");
    assert!(!archive_plan.can_replace_current_exe);
}
