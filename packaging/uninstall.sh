#!/usr/bin/env bash
# Dev Activity Watcher (atw) — uninstaller.
#
# Stops and removes the daemon unit, the binary, and the shell plugin.
# Your activity database is NOT touched unless you pass --purge.
set -euo pipefail

PLUGIN_ID="dhirajarya.atw"
PURGE=0
[[ "${1:-}" == "--purge" ]] && PURGE=1

echo "==> Stopping the daemon"
systemctl --user disable --now atwd.service 2>/dev/null || true
pkill -x atwd 2>/dev/null || true

echo "==> Removing unit, binary, and plugin"
rm -f "$HOME/.config/systemd/user/atwd.service"
systemctl --user daemon-reload
rm -f "$HOME/.local/bin/atwd"
omarchy plugin disable "$PLUGIN_ID" 2>/dev/null || true
rm -rf "$HOME/.config/omarchy/plugins/$PLUGIN_ID"

if [[ $PURGE -eq 1 ]]; then
  echo "==> Purging all recorded data (config kept)"
  rm -rf "$HOME/.local/share/atw" "$HOME/.local/state/atw"
  echo "Data deleted. Config remains at ~/.config/atw/config.toml — delete it too if you want a pristine slate."
else
  echo
  echo "Recorded data kept at ~/.local/share/atw/activity.db (7-day retention anyway)."
  echo "Run again with --purge to delete it."
fi

echo "Uninstalled."
