#!/usr/bin/env bash

set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <RelayCat.app> <RelayCat.app.tar.gz>" >&2
  exit 2
fi

: "${APPLE_SIGNING_IDENTITY:?APPLE_SIGNING_IDENTITY is required}"
: "${APPLE_ID:?APPLE_ID is required}"
: "${APPLE_PASSWORD:?APPLE_PASSWORD is required}"
: "${APPLE_TEAM_ID:?APPLE_TEAM_ID is required}"
: "${TAURI_SIGNING_PRIVATE_KEY:?TAURI_SIGNING_PRIVATE_KEY is required}"
: "${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:?TAURI_SIGNING_PRIVATE_KEY_PASSWORD is required}"

source_app="$1"
updater_archive="$2"

if [ ! -d "$source_app" ]; then
  echo "macOS app bundle not found: $source_app" >&2
  exit 1
fi

case "$updater_archive" in
  *.app.tar.gz) ;;
  *)
    echo "updater archive must end in .app.tar.gz: $updater_archive" >&2
    exit 1
    ;;
esac

temp_root="${RUNNER_TEMP:-${TMPDIR:-/tmp}}"
work_dir="$(mktemp -d "$temp_root/relaycat-macos-updater.XXXXXX")"
trap 'rm -rf "$work_dir"' EXIT

app_name="$(basename "$source_app")"
prepared_app="$work_dir/prepared/$app_name"
notary_zip="$work_dir/notarization.zip"
verify_dir="$work_dir/verify"

mkdir -p "$(dirname "$prepared_app")" "$verify_dir" "$(dirname "$updater_archive")"
ditto "$source_app" "$prepared_app"

# Tauri's tar extractor does not restore macOS extended metadata. Clear it
# before signing so the signature covers only data that survives extraction.
xattr -cr "$prepared_app"
codesign \
  --force \
  --deep \
  --preserve-metadata=identifier,entitlements,requirements \
  --options runtime \
  --timestamp \
  --sign "$APPLE_SIGNING_IDENTITY" \
  "$prepared_app"
codesign --verify --deep --strict --verbose=2 "$prepared_app"

# The clean signature has a new cdhash, so submit that exact App for
# notarization before producing the updater archive.
ditto -c -k --sequesterRsrc --keepParent "$prepared_app" "$notary_zip"
xcrun notarytool submit "$notary_zip" \
  --apple-id "$APPLE_ID" \
  --password "$APPLE_PASSWORD" \
  --team-id "$APPLE_TEAM_ID" \
  --wait
xcrun stapler staple "$prepared_app"
xcrun stapler validate "$prepared_app"

# Do not add AppleDouble files or copyfile metadata: the updater extracts this
# archive with Rust's tar implementation and must get the same signed bytes.
rm -f "$updater_archive" "${updater_archive}.sig"
COPYFILE_DISABLE=1 tar -czf "$updater_archive" -C "$(dirname "$prepared_app")" "$app_name"
npx tauri signer sign "$updater_archive"
if [ ! -s "${updater_archive}.sig" ]; then
  echo "Tauri updater signature was not created: ${updater_archive}.sig" >&2
  exit 1
fi

# Validate the exact artifact that will be uploaded, not only the source App.
tar -xzf "$updater_archive" -C "$verify_dir"
extracted_app="$verify_dir/$app_name"
codesign --verify --deep --strict --verbose=2 "$extracted_app"
xcrun stapler validate "$extracted_app"
spctl --assess --type execute --verbose=2 "$extracted_app"

echo "Validated macOS updater archive: $updater_archive"
