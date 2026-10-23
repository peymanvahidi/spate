#!/bin/bash
# Builds spate-host, installs it under ~/Library/Application Support/Spate/bin and registers
# it as Brave's native messaging host `com.spate.host` for the Spate extension.
# Idempotent: safe to run again after every rebuild.
#
# Environment:
#   SPATE_EXTENSION_ID  override the extension ID (default: derived from extension/manifest.json "key")
#   SPATE_SKIP_BUILD=1  install an already-built target/release/spate-host
#   CARGO_TARGET_DIR    honoured when locating the built binary
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HOST_NAME="com.spate.host"
BIN_DIR="$HOME/Library/Application Support/Spate/bin"
BIN="$BIN_DIR/spate-host"
MANIFEST_DIR="$HOME/Library/Application Support/BraveSoftware/Brave-Browser/NativeMessagingHosts"
MANIFEST="$MANIFEST_DIR/$HOST_NAME.json"

# Extension ID = first 32 hex chars of SHA-256(DER public key), mapped 0-f → a-p.
extension_id() {
  if [[ -n "${SPATE_EXTENSION_ID:-}" ]]; then
    echo "$SPATE_EXTENSION_ID"
    return
  fi
  local key
  key="$(sed -n 's/^[[:space:]]*"key":[[:space:]]*"\([^"]*\)".*/\1/p' "$ROOT/extension/manifest.json")"
  if [[ -z "$key" ]]; then
    echo "error: no \"key\" in extension/manifest.json" >&2
    exit 1
  fi
  printf '%s' "$key" | base64 -D | shasum -a 256 | cut -c1-32 | tr '0-9a-f' 'a-p'
}

EXT_ID="$(extension_id)"
if [[ ! "$EXT_ID" =~ ^[a-p]{32}$ ]]; then
  echo "error: invalid extension id '$EXT_ID'" >&2
  exit 1
fi

if [[ "${SPATE_SKIP_BUILD:-0}" != "1" ]]; then
  echo "==> Building spate-host (release)"
  (cd "$ROOT" && cargo build --release --locked -p spate-host)
fi
BUILT="${CARGO_TARGET_DIR:-$ROOT/target}/release/spate-host"
if [[ ! -x "$BUILT" ]]; then
  echo "error: $BUILT not found" >&2
  exit 1
fi

echo "==> Installing $BIN"
mkdir -p "$BIN_DIR"
# Copy then rename, so a host process Brave is running keeps its old inode.
TMP_BIN="$(mktemp "$BIN_DIR/.spate-host.XXXXXX")"
cp "$BUILT" "$TMP_BIN"
chmod 755 "$TMP_BIN"
mv -f "$TMP_BIN" "$BIN"

echo "==> Writing $MANIFEST"
mkdir -p "$MANIFEST_DIR"
# JSON-escape the path (backslashes and quotes).
ESC_BIN="$(printf '%s' "$BIN" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g')"
TMP_MANIFEST="$(mktemp "$MANIFEST_DIR/.$HOST_NAME.XXXXXX")"
cat > "$TMP_MANIFEST" <<EOF
{
  "name": "$HOST_NAME",
  "description": "Spate download accelerator",
  "path": "$ESC_BIN",
  "type": "stdio",
  "allowed_origins": ["chrome-extension://$EXT_ID/"]
}
EOF
chmod 644 "$TMP_MANIFEST"
mv -f "$TMP_MANIFEST" "$MANIFEST"

echo "==> Done. Extension ID: $EXT_ID"
echo "    Load $ROOT/extension in brave://extensions (Developer mode → Load unpacked)."
