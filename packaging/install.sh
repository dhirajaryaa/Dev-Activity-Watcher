#!/usr/bin/env bash
# Dev Activity Watcher (atw) — installer.
#
# Builds the release binaries, installs them to ~/.local/bin, installs the
# systemd user units, installs the Omarchy shell plugin, and enables them.
# Tracking is local-first: data stays in ~/.local/share/atw. Cloud sync is
# optional and only talks to the Turso database you configure in the panel.
set -euo pipefail

ATW_HOME="${ATW_HOME:-$HOME/Work/atw}"
PLUGIN_ID="dhirajarya.atw"
PLUGIN_SRC="$ATW_HOME/plugin"
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

echo
echo "Installed. The bar widget appears after the shell reloads (a few seconds)."
echo "  daemon : systemctl --user status atwd"
echo "  config : ~/.config/atw/config.toml  (restart atwd after editing)"
echo "  data   : ~/.local/share/atw/activity.db  (7-day local retention)"
echo "  cloud  : optional — open the panel and paste your Turso URL + token"
