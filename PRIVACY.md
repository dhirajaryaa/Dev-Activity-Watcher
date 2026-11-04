# Privacy

Dev Activity Watcher is local-first: tracking never requires an account or a
network connection. Cloud sync is optional, off the tracking path, and only
ever talks to the database **you** own.

## What is stored locally

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

## What is uploaded (only if you enable sync)

When cloud sync is on and configured, closed segments are sent to the Turso
database whose URL you pasted in the panel:

| uploaded | example |
|---|---|
| segment UUID | random v4/uuid7, not derived from hardware |
| device id | random UUID generated once per machine (`~/.local/state/atw/device-id`) |
| app display name | `Visual Studio Code` |
| app class | `code` |
| category | `editor` |
| start / end (utc) and duration | `1793…`, `1793…`, `600` |
| local activity date | `2026-11-04` |

Not uploaded: window titles, file names, project paths, keystrokes, hostnames,
usernames, IP-based identity. The only network destination is the single URL
you configured; there is no telemetry or crash reporting anywhere in the
binary.

## What is never done

- No telemetry, no crash reporting, no accounts, no third-party analytics.
- No keylogging — input is only observed as "active vs idle" via the
  compositor's idle notification.
- No listening sockets. The daemon is a client only.

## Network access and the systemd unit

Earlier releases shipped `PrivateNetwork=true` + `IPAddressDeny=any`, which
made network access impossible. That conflicts with an optional sync feature
the user can switch on at runtime, so the unit now keeps the filesystem and
privilege hardening (`ProtectSystem=strict`, `ProtectHome=read-only`,
`NoNewPrivileges`, `RestrictNamespaces`, `LockPersonality`) and restricts
sockets to `AF_UNIX AF_INET AF_INET6`. The daemon never opens a listening
socket; it only connects when sync is enabled **and** a URL/token exist.
If you never configure cloud sync, it makes no outbound connections at all.

## Cloud retention and deletion

- Raw cloud segments older than `cloud_retention_days` (90) are deleted once a
  day; pre-aggregated daily stats are kept.
- Local rows older than `retention_days` (7) are deleted at midnight. With
  sync active, only rows the cloud has acknowledged are deleted.
- Turn sync off in the panel for a fully local app.
- Delete the cloud database from Turso's dashboard, or run
  `DROP TABLE segments; DROP TABLE daily_app; DROP TABLE daily_unique; ...`
  with a write-capable token.
- Delete local data: `atw cleanup`, or
  `rm -rf ~/.local/share/atw ~/.local/state/atw`
  (or `packaging/uninstall.sh --purge`).

The token is stored in `~/.config/atw/config.toml` in plain text (mode 600 is
recommended; the installer creates the directory). If that is not acceptable,
set `token_cmd` to a command that prints the token (e.g. from a keyring) and
clear `token`; the command's output is used instead and never logged.

## Where it lives

- Database: `~/.local/share/atw/activity.db`
- Live status: `~/.local/state/atw/status.json`
- Device id: `~/.local/state/atw/device-id`
- Sync record: `~/.local/state/atw/sync.json`
- Config: `~/.config/atw/config.toml`

## How to inspect it

```bash
sqlite3 ~/.local/share/atw/activity.db \
  'SELECT date, display_name, SUM(duration_seconds)/60 AS minutes
   FROM activity GROUP BY date, display_name ORDER BY date DESC;'
```

For the cloud side (read-only token recommended), see
[`docs/PORTFOLIO.md`](docs/PORTFOLIO.md).
