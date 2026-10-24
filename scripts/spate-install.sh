#!/bin/bash
# Installs or removes Spate for the current user. Called by `make install` / `make uninstall`;
# expects the release build to exist already (`make build`).
#
#   scripts/spate-install.sh install     quit a running Spate, install the app, host, CLI
#   scripts/spate-install.sh uninstall   quit Spate and remove what `install` put in place
#
# Environment:
#   PREFIX_HOME        install under this directory instead of $HOME (tests). The app is then
#                      never relaunched with `open`, since Launch Services would start it with
#                      the real home.
#   PURGE=1            uninstall also deletes ~/Library/Application Support/Spate,
#                      ~/Library/Logs/Spate and ~/Downloads/Spate
#   SPATE_NO_LAUNCH=1  don't relaunch Spate after an upgrade
#   CARGO_TARGET_DIR   where the build is (default: <repo>/target)
#
# No sudo, no shell rc edits, no xattr changes.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HOME_DIR="${PREFIX_HOME:-$HOME}"
TARGET="${CARGO_TARGET_DIR:-$ROOT/target}"

APP_DIR="$HOME_DIR/Applications"
APP="$APP_DIR/Spate.app"
APP_EXE="$APP/Contents/MacOS/spate-app"
DATA_DIR="$HOME_DIR/Library/Application Support/Spate"
BIN_DIR="$DATA_DIR/bin"
CLI_BIN="$BIN_DIR/spate"
HOST_BIN="$BIN_DIR/spate-host"
CLI_LINK_DIR="$HOME_DIR/.local/bin"
CLI_LINK="$CLI_LINK_DIR/spate"
MANIFEST="$HOME_DIR/Library/Application Support/BraveSoftware/Brave-Browser/NativeMessagingHosts/com.spate.host.json"
LAUNCH_AGENT="$HOME_DIR/Library/LaunchAgents/Spate.plist"
EXT_ID="akcgnbcpioinnkkmoobjdgnnmebipnil"

BUILT_APP="$TARGET/release/bundle/macos/Spate.app"
BUILT_CLI="$TARGET/release/spate"
BUILT_HOST="$TARGET/release/spate-host"

real_home() { [[ "$HOME_DIR" == "$HOME" ]]; }

say() { printf '==> %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

# PIDs of running Spate apps this install owns: the one in $APP, and (for a real install) any
# other Spate.app bundle, which would share the data dir and socket with the new one.
spate_pids() {
  ps -axo pid=,command= | awk -v exe="$APP_EXE" -v any="$(real_home && echo 1 || echo 0)" '
    {
      pid = $1; sub(/^[ \t]*[0-9]+[ \t]+/, "")
      if (index($0, exe) == 1 || (any == "1" && $0 ~ /\/Spate\.app\/Contents\/MacOS\/spate-app( |$)/)) print pid
    }'
}

# SIGTERM makes Spate pause its downloads (state saved) and exit. Prints "1" if it was running.
quit_spate() {
  local pids
  pids="$(spate_pids)"
  if [[ -z "$pids" ]]; then
    return 0
  fi
  say "Quitting Spate (pid $(echo $pids)): running downloads are paused and kept" >&2
  # shellcheck disable=SC2086
  kill -TERM $pids 2>/dev/null || true
  local i
  for i in $(seq 1 60); do
    sleep 0.5
    pids="$(spate_pids)"
    [[ -z "$pids" ]] && { echo 1; return 0; }
  done
  die "Spate (pid $(echo $pids)) did not quit within 30 s; quit it from the menu bar and run this again"
}

# Copy then rename, so a process using the old file keeps its inode.
install_file() {
  local src="$1" dst="$2" tmp
  tmp="$(mktemp "$(dirname "$dst")/.$(basename "$dst").XXXXXX")"
  cp "$src" "$tmp"
  chmod 755 "$tmp"
  mv -f "$tmp" "$dst"
}

do_install() {
  [[ -d "$BUILT_APP" ]] || die "$BUILT_APP not found (run 'make build')"
  [[ -x "$BUILT_CLI" ]] || die "$BUILT_CLI not found (run 'make build')"
  [[ -x "$BUILT_HOST" ]] || die "$BUILT_HOST not found (run 'make build')"

  local was_running
  was_running="$(quit_spate)"

  # The app: copy beside the old one, sign, then swap.
  say "Installing $APP"
  mkdir -p "$APP_DIR"
  local new="$APP_DIR/.Spate.app.new" old="$APP_DIR/.Spate.app.old"
  rm -rf "$new" "$old"
  ditto "$BUILT_APP" "$new"
  codesign --force --deep -s - "$new"
  if [[ -e "$APP" ]]; then
    mv "$APP" "$old"
  fi
  mv "$new" "$APP"
  rm -rf "$old"

  # Native messaging host + Brave manifest (stable path: $BIN_DIR/spate-host).
  SPATE_SKIP_BUILD=1 PREFIX_HOME="$HOME_DIR" SPATE_EXTENSION_ID="${SPATE_EXTENSION_ID:-}" \
    CARGO_TARGET_DIR="$TARGET" "$ROOT/scripts/install-native-host.sh" | grep -v '^    Load ' || true
  [[ -x "$HOST_BIN" && -f "$MANIFEST" ]] || die "native host install failed"

  # CLI: a copy next to the host, linked from ~/.local/bin.
  say "Installing $CLI_LINK -> $CLI_BIN"
  mkdir -p "$BIN_DIR" "$CLI_LINK_DIR"
  install_file "$BUILT_CLI" "$CLI_BIN"
  ln -sfn "$CLI_BIN" "$CLI_LINK"

  relaunch "$was_running"

  echo
  echo "Spate is installed."
  case ":$PATH:" in
    *":$CLI_LINK_DIR:"*) ;;
    *) echo "Note: $CLI_LINK_DIR is not on your PATH. Add it to use 'spate' directly, e.g."
       echo "      export PATH=\"$CLI_LINK_DIR:\$PATH\"   (in ~/.zshrc)"
       echo ;;
  esac
  cat <<EOF
Next steps:
  1. Open brave://extensions and turn on Developer mode (top right).
  2. Click "Load unpacked" and choose: $ROOT/extension
     (once only; after an update, click the reload icon on the Spate card instead)
  3. Check that the extension ID shown is $EXT_ID
  4. Launch Spate once: open "$APP"
     It lives in the menu bar and starts at login from then on.
EOF
}

# After an upgrade, start the new version if the old one was running.
relaunch() {
  [[ "$1" == "1" ]] || return 0
  if ! real_home || [[ "${SPATE_NO_LAUNCH:-0}" == "1" ]]; then
    say "Spate was running; start the new version with: open \"$APP\""
    return 0
  fi
  say "Starting the new Spate"
  open "$APP" || say "Could not start it; run: open \"$APP\""
  return 0
}

do_uninstall() {
  quit_spate >/dev/null
  if [[ -e "$APP" ]]; then
    say "Removing $APP"
  fi
  rm -rf "$APP" "$APP_DIR/.Spate.app.new" "$APP_DIR/.Spate.app.old"
  rmdir "$APP_DIR" 2>/dev/null || true
  if [[ -e "$MANIFEST" || -e "$HOST_BIN" ]]; then
    say "Removing the native messaging host and its Brave manifest"
  fi
  rm -f "$MANIFEST" "$HOST_BIN"
  if [[ -L "$CLI_LINK" ]]; then
    say "Removing $CLI_LINK"
    rm -f "$CLI_LINK"
  fi
  rm -f "$CLI_BIN"
  rmdir "$BIN_DIR" 2>/dev/null || true
  if [[ -f "$LAUNCH_AGENT" ]]; then
    say "Removing the login item $LAUNCH_AGENT"
    # It's only loaded in a real login session.
    if real_home; then
      launchctl bootout "gui/$(id -u)/Spate" 2>/dev/null || true
    fi
    rm -f "$LAUNCH_AGENT"
  fi
  # So a later install turns "Start at Login" on again, like a first launch.
  rm -f "$DATA_DIR/.login-item-initialised"

  if [[ "${PURGE:-0}" == "1" ]]; then
    say "PURGE=1: removing history, settings, logs, downloads and WebKit caches"
    rm -rf "$DATA_DIR" "$HOME_DIR/Library/Logs/Spate" "$HOME_DIR/Downloads/Spate" \
      "$HOME_DIR/Library/Caches/com.spate.app" "$HOME_DIR/Library/WebKit/com.spate.app"
  else
    echo "Kept your history and settings ($DATA_DIR),"
    echo "logs ($HOME_DIR/Library/Logs/Spate) and downloads ($HOME_DIR/Downloads/Spate)."
    echo "'make uninstall PURGE=1' removes them too."
  fi
  echo "Remove the Spate extension in brave://extensions yourself."
}

case "${1:-}" in
  install) do_install ;;
  uninstall) do_uninstall ;;
  *) echo "usage: $0 install|uninstall" >&2; exit 2 ;;
esac
