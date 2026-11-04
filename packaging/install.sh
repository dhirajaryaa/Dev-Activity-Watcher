#!/usr/bin/env bash
# Dev Activity Watcher (atw) — installer.
#
# Builds the release binaries, installs them to ~/.local/bin, installs the
# systemd user units, installs the Omarchy shell plugin, and enables them.
# Tracking is local-first: data stays in ~/.local/share/atw. Cloud sync is
# optional and only talks to the Turso database you configure in the panel.
set -euo pipefail

ATW_HOME="${ATW_HOME:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
PLUGIN_ID="dev-activity-watcher.atw"
PLUGIN_SRC="$ATW_HOME"
PLUGIN_DST="$HOME/.config/omarchy/plugins/$PLUGIN_ID"
BIN_DST="$HOME/.local/bin/atwd"
UNIT_DST="$HOME/.config/systemd/user/atwd.service"

echo "==> Building release binary"
cargo build --release --manifest-path "$ATW_HOME/Cargo.toml"

echo "==> Installing daemon to $BIN_DST"
mkdir -p "$HOME/.local/bin"
install -m 755 "$ATW_HOME/target/release/atwd" "$BIN_DST"
install -m 755 "$ATW_HOME/target/release/atw"  "$HOME/.local/bin/atw"

echo "==> Installing systemd user units"
# The service sandbox (ReadWritePaths) requires these paths to exist before
# the daemon can start; the daemon creates them itself, but too late for
# systemd's mount namespace setup.
mkdir -p ~/.local/share/atw ~/.local/state/atw ~/.config/atw
mkdir -p "$HOME/.config/systemd/user"
install -m 644 "$ATW_HOME/packaging/atwd.service" "$UNIT_DST"
install -m 644 "$ATW_HOME/packaging/atw-sync.service" "$HOME/.config/systemd/user/atw-sync.service"
systemctl --user daemon-reload
systemctl --user enable --now atwd.service

echo "==> Installing shell plugin"
mkdir -p "$PLUGIN_DST"
install -m 644 "$PLUGIN_SRC/manifest.json"  "$PLUGIN_DST/manifest.json"
install -m 644 "$PLUGIN_SRC/BarWidget.qml"  "$PLUGIN_DST/BarWidget.qml"
install -m 644 "$PLUGIN_SRC/Panel.qml"      "$PLUGIN_DST/Panel.qml"

if omarchy plugin list 2>/dev/null | grep -q "$PLUGIN_ID"; then
  echo "==> Plugin already enabled"
else
  echo "==> Enabling plugin in the right bar section"
  omarchy plugin enable "$PLUGIN_ID" --section right
fi

# The Omarchy shell starts with Quickshell's file watcher disabled
# (QS_DISABLE_FILE_WATCHER=1) and its plugin rescan does not re-instantiate
# already-mounted widget QML, so freshly written Panel.qml would never reach
# the screen — the visible popup stays the one from before the update.
# When an Omarchy shell instance is running, restarting the shell is the
# reliable way to load the new code. A fresh-boot install picks everything
# up when the shell starts anyway, so there is nothing to do in that case.
if quickshell list -a 2>/dev/null | grep -q "/usr/share/omarchy/shell"; then
  echo "==> Restarting the Omarchy shell so the new QML actually loads"
  omarchy-restart-shell >/dev/null 2>&1 || true
fi

echo
echo "Installed. The bar widget appears after the shell reloads (a few seconds)."
echo "  daemon : systemctl --user status atwd"
echo "  config : ~/.config/atw/config.toml  (restart atwd after editing)"
echo "  data   : ~/.local/share/atw/activity.db  (7-day local retention)"
echo "  cloud  : optional — open the panel and paste your Turso URL + token"
