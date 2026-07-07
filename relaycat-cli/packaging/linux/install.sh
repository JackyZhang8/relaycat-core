#!/usr/bin/env bash
# Install the RelayCat .desktop launcher.
#
# Usage:
#   packaging/linux/install.sh            # per-user (~/.local/share/applications)
#   packaging/linux/install.sh --system   # system-wide (/usr/share/applications)
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$here/../.." && pwd)"
template="$here/relaycat.desktop"

system=0
if [[ "${1:-}" == "--system" ]]; then
    system=1
fi

# Resolve the relaycat binary: prefer a release build, then $PATH.
if [[ -x "$repo_root/target/release/relaycat" ]]; then
    bin="$repo_root/target/release/relaycat"
elif bin="$(command -v relaycat 2>/dev/null)"; then
    :
else
    echo "error: could not find the relaycat binary." >&2
    echo "       build it first: cargo build --release -p relaycat-cli" >&2
    exit 1
fi

if [[ $system -eq 1 ]]; then
    dest_dir="/usr/share/applications"
else
    dest_dir="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
fi
dest="$dest_dir/relaycat.desktop"

mkdir -p "$dest_dir"
sed "s|@RELAYCAT_BIN@|$bin|g" "$template" > "$dest"

if command -v desktop-file-validate >/dev/null 2>&1; then
    desktop-file-validate "$dest"
fi
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$dest_dir" 2>/dev/null || true
fi

echo "installed $dest (Exec=$bin tui)"
