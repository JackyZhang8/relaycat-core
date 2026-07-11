#!/usr/bin/env bash
set -euo pipefail

MODE="${1:-debug}"
if [[ "$MODE" != "debug" && "$MODE" != "--release" && "$MODE" != "release" ]]; then
  echo "usage: ./build.sh [debug|release|--release]" >&2
  exit 2
fi

if [[ "$MODE" == "debug" ]]; then
  TARGET_DIR="target/debug"
  cargo build -p relaycat-cli
else
  TARGET_DIR="target/release"
  cargo build --release -p relaycat-cli
fi

echo
echo "built:"
echo "  ${TARGET_DIR}/relaycat"
