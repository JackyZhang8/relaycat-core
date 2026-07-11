#!/usr/bin/env bash
# Build a double-clickable RelayCat.app that opens Terminal.app and runs
# `relaycat tui`.
#
# Usage:
#   packaging/macos/build-app.sh                  # use `relaycat` from $PATH
#   packaging/macos/build-app.sh /path/to/relaycat
#
# NOTE: produces an UNSIGNED bundle. For distribution, codesign + notarize:
#   codesign --deep --force --sign "Developer ID Application: NAME" dist/RelayCat.app
#   xcrun notarytool submit ... ; xcrun stapler staple dist/RelayCat.app
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$here/../.." && pwd)"

bin="${1:-}"
if [[ -z "$bin" ]]; then
    if [[ -x "$repo_root/target/release/relaycat" ]]; then
        bin="$repo_root/target/release/relaycat"
    else
        bin="$(command -v relaycat || true)"
    fi
fi
if [[ -z "$bin" || ! -x "$bin" ]]; then
    echo "error: relaycat binary not found; build it or pass its path." >&2
    exit 1
fi

app="$repo_root/dist/RelayCat.app"
contents="$app/Contents"
rm -rf "$app"
mkdir -p "$contents/MacOS" "$contents/Resources"

# Embed the binary so the bundle is self-contained.
cp "$bin" "$contents/Resources/relaycat"
chmod +x "$contents/Resources/relaycat"

cat > "$contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>            <string>RelayCat</string>
    <key>CFBundleDisplayName</key>     <string>RelayCat</string>
    <key>CFBundleIdentifier</key>      <string>ai.relaycat.launcher</string>
    <key>CFBundleVersion</key>         <string>0.1.0</string>
    <key>CFBundleShortVersionString</key><string>0.1.0</string>
    <key>CFBundlePackageType</key>     <string>APPL</string>
    <key>CFBundleExecutable</key>      <string>RelayCat</string>
    <key>LSMinimumSystemVersion</key>  <string>10.13</string>
</dict>
</plist>
PLIST

# Launcher: open Terminal.app and run the embedded binary's TUI.
cat > "$contents/MacOS/RelayCat" <<'LAUNCH'
#!/usr/bin/env bash
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
bin="$here/../Resources/relaycat"
osascript -e "tell application \"Terminal\"
    activate
    do script \"'$bin' tui\"
end tell"
LAUNCH
chmod +x "$contents/MacOS/RelayCat"

echo "built $app"
echo "double-click it (or: open '$app') to launch the RelayCat TUI."
