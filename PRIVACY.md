# Privacy

Dev Activity Watcher is local-only by construction, not by policy.

## What is stored

One SQLite table of usage segments — nothing else:

| column | content | example |
|---|---|---|
| `date` | local calendar day | `2026-11-04` |
| `application_class` | Hyprland window class | `code` |
| `display_name` | friendly name | `Visual Studio Code` |
| `category` | editor / browser / terminal / agent / other | `editor` |
| `started_at`, `ended_at` | unix timestamps | `1793…`, `1793…` |
| `duration_seconds` | segment length | `600` |

With `editor_project_names = true` the *project folder's basename* is appended
to the display name ("Visual Studio Code · atw"). File names, window titles,
paths, and keystrokes are **never** stored.

## What is never done

- No network access — the daemon has no HTTP client, and the systemd unit
  enforces `PrivateNetwork=true` + `IPAddressDeny=any` so it *cannot* phone
  home even if it tried.
- No telemetry, no crash reporting, no accounts, no sync.
- No keylogging — input is only observed as "active vs idle" via the
  compositor's idle notification.

## Where it lives

- Database: `~/.local/share/atw/activity.db`
- Live status: `~/.local/state/atw/status.json`
- Config: `~/.config/atw/config.toml`

## How to inspect it

```bash
sqlite3 ~/.local/share/atw/activity.db \
  'SELECT date, display_name, SUM(duration_seconds)/60 AS minutes
   FROM activity GROUP BY date, display_name ORDER BY date DESC;'
```

(Install `sqlite3` if it is missing — the database itself is a plain file.)

## How to delete it

- Automatic: rows older than `retention_days` (7) are deleted at every local
  midnight and on daemon start.
- Manual, right now: `atw cleanup`
- Everything: `rm -rf ~/.local/share/atw ~/.local/state/atw`
  (or `packaging/uninstall.sh --purge`)
