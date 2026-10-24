#!/usr/bin/env bash

set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <staple|validate> <artifact>" >&2
  exit 2
fi
case "$1" in
  staple|validate) ;;
  *) echo "unsupported stapler action: $1" >&2; exit 2 ;;
esac

# stapler uses EX_UNAVAILABLE (68) when Apple ticket delivery is unavailable.
# Keep other failures fatal, and never upload an artifact without validation.
for attempt in 1 2 3 4 5; do
  status=0
  xcrun stapler "$1" "$2" || status=$?
  if [ "$status" -eq 0 ]; then
    exit 0
  fi
  if [ "$status" -ne 68 ] || [ "$attempt" -eq 5 ]; then
    exit "$status"
  fi
  delay=$((attempt * 10))
  echo "::warning::Apple stapler $1 unavailable (68), attempt $attempt/5; retrying in ${delay}s." >&2
  sleep "$delay"
done
