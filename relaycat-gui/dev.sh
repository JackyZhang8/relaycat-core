#!/usr/bin/env bash
#
# dev.sh — run relaycat-gui in development mode.
#
# Installs the frontend deps if needed, then launches `tauri dev` with hot
# reload. Relay mode needs no separate `relaycat` binary: the GUI links the
# relaycat-cli source and re-executes itself to host relay sessions.
#
# Usage:
#   ./dev.sh

set -euo pipefail

GUI_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$GUI_DIR"

# Install frontend dependencies on first run.
if [ ! -d node_modules ]; then
  echo "==> Installing frontend dependencies…"
  npm install
fi

# Launch the Tauri dev window (Vite dev server + Rust backend, hot reload).
echo "==> Starting relaycat-gui (tauri dev)…"
exec npx tauri dev "$@"
