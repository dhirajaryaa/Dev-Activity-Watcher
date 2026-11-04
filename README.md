# Dev Activity Watcher (atw)

A local-only, privacy-first activity tracker for [Omarchy Linux](https://omarchy.org).
It watches which **developer apps** you use — editors (VS Code, Zed, JetBrains),
AI agents (Cursor, Windsurf, Claude, the Omarchy agent), browsers, and terminals —
and shows today's per-app time in your bar. Nothing else is tracked, nothing
ever leaves the machine.

```
 ▼  2h 15m        ← today's tracked total, dimmed while idle/locked
```

Click it for a panel with today's breakdown, the weekly top apps, and the
privacy facts.

## How it works

- **`atwd`** (Rust daemon) listens to Hyprland's event socket — no polling.
  Idle/AFK is detected through Wayland `ext-idle-notify-v1` (5 min default),
  suspend/resume through clock drift. Usage is written to SQLite in batches
  (~7 min) and on every app change, so AFK time is never counted.
- **The plugin** (Omarchy shell bar widget) reads a status file the daemon
  refreshes on every state change — inotify, no HTTP, no sockets of its own.
- **Only developer apps are recorded.** The allowlist is configurable:
  media players, file managers, games, and anything unclassified are ignored.

## Install

```bash
cd ~/Work/atw
packaging/install.sh
```

That builds the release binary, installs `atwd` to `~/.local/bin`, enables the
systemd user unit, and installs + enables the shell plugin
(`dhirajarya.atw`, shown as **Dev Activity Watcher**).

## Uninstall

```bash
packaging/uninstall.sh            # keeps your recorded data
packaging/uninstall.sh --purge    # also deletes the database
```

## Configuration

`~/.config/atw/config.toml` (restart the daemon after editing:
`systemctl --user restart atwd`):

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `true` | Master switch; `false` makes the daemon exit. |
| `idle_timeout_seconds` | `300` | Seconds without input before you count as AFK. |
| `flush_interval_seconds` | `420` | How often the open segment is written to SQLite. |
| `retention_days` | `7` | Rows older than this are deleted daily at midnight. |
| `tracked_categories` | editor, browser, terminal, agent | Which app categories count. |
| `tracked_apps` | `[]` | Extra window classes to track, e.g. `["postman"]`. |
| `excluded_apps` | `[]` | Never track these, even if their category is listed. |
| `editor_project_names` | `false` | Append the project folder's basename to editor names ("Visual Studio Code · atw"). File names/paths are never stored. |
| `display_names` | built-ins | Friendly names per window class. |

## CLI

```bash
atw status    # print the same JSON the panel reads
atw cleanup   # force the 7-day retention pass now
```

## Privacy

See [PRIVACY.md](PRIVACY.md) — the short version: everything stays in
`~/.local/share/atw/activity.db` on this machine. No network, no telemetry,
no accounts, no sync. Records older than 7 days are deleted automatically.
The daemon's systemd unit even sets `PrivateNetwork=true` so it *cannot*
reach the network.

## Development

See [DEVELOPMENT.md](DEVELOPMENT.md) for the full build log, design
decisions (in plain language), and the test history; [TODO.md](TODO.md)
tracks the phase plan.
