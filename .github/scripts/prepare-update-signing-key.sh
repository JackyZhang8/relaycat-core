#!/usr/bin/env bash
# Normalizes $SIGNING_KEY (Ed25519 private key) into an unencrypted PKCS#8 PEM
# at "$1". Accepts a PEM pasted with any line layout (including literal "\n"),
# base64 of a PEM file, base64 PKCS#8 DER, or a base64 raw 32-byte seed.
set -euo pipefail

out="$1"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

die() {
  echo "::error::RELAYCAT_CLI_UPDATE_SIGNING_KEY: $1 Generate it with: openssl genpkey -algorithm ed25519 -out relaycat-update-key.pem, then paste the whole file."
  exit 1
}

reject_known_wrong_formats() {
  case "$1" in
    *"BEGIN OPENSSH PRIVATE KEY"*) die "this is an OpenSSH (ssh-keygen) key, which openssl cannot read." ;;
    *"ENCRYPTED PRIVATE KEY"*) die "this key is passphrase-protected; store it unencrypted." ;;
    *"BEGIN PUBLIC KEY"*) die "this is a public key, not the private key." ;;
    *"BEGIN RSA PRIVATE KEY"* | *"BEGIN EC PRIVATE KEY"*) die "this is not an Ed25519 key." ;;
  esac
}

decode_base64() {
  local body
  body="$(printf '%s' "$1" | sed -e 's/-----BEGIN PRIVATE KEY-----//g' -e 's/-----END PRIVATE KEY-----//g' | tr -d " \t\r\n\"'")"
  [ -n "$body" ] || die "the value is empty."
  printf '%s' "$body" | base64 -d > "$2" 2>/dev/null || die "the value is neither a PEM key nor valid base64."
}

value="$(printf '%s' "${SIGNING_KEY:-}" | tr -d '\r')"
value="${value//\\n/$'\n'}"
reject_known_wrong_formats "$value"

der="$work/key.der"
decode_base64 "$value" "$der"

# base64 of an entire PEM file
if grep -q -- '-----BEGIN' "$der" 2>/dev/null; then
  inner="$(tr -d '\r' < "$der")"
  reject_known_wrong_formats "$inner"
  decode_base64 "$inner" "$der"
fi

# raw 32-byte Ed25519 seed -> PKCS#8 DER
if [ "$(stat -c%s "$der")" -eq 32 ]; then
  { printf '\x30\x2e\x02\x01\x00\x30\x05\x06\x03\x2b\x65\x70\x04\x22\x04\x20'; cat "$der"; } > "$work/seed.der"
  mv "$work/seed.der" "$der"
fi

(umask 077 && openssl pkey -inform DER -in "$der" -out "$out" 2>/dev/null) \
  || die "openssl cannot parse the decoded key (expected an Ed25519 PKCS#8 private key)."
openssl pkey -in "$out" -noout -text 2>/dev/null | grep -q 'ED25519' || die "this is not an Ed25519 key."
