# Dev Activity Watcher (atw)

A privacy-first activity tracker for [Omarchy Linux](https://omarchy.org).
It watches which **developer apps** you use — editors (VS Code, Zed, JetBrains),
AI agents (Cursor, Windsurf, Claude, the Omarchy agent), browsers, and terminals —
and shows today's per-app time in your bar. Tracking is local by default;
optional cloud sync to **your own** Turso database aggregates multiple
machines without ever sending window titles or file paths.

```
 󱚺  2h        ← today's tracked total, dimmed while idle/locked
```

Click it for a panel with today's breakdown, on-demand 7-day history, and the
Cloud section. Right-click the bar icon to cycle the display format:
icon → icon + hours → icon + hours + minutes.

## How it works

- **`atwd`** (Rust daemon) listens to Hyprland's event socket — no polling.
  Idle/AFK is detected through Wayland `ext-idle-notify-v1` (5 min default),
  suspend/resume through clock drift. Usage is written to SQLite in batches
  (~7 min) and on every app change, so AFK time is never counted.
- **The plugin** (Omarchy shell bar widget) reads a status file the daemon
  refreshes on every state change — inotify, no HTTP, no sockets of its own.
- **Only developer apps are recorded.** The allowlist is configurable:
  media players, file managers, games, and anything unclassified are ignored.
- **Cloud sync is optional and off the tracking hot path.** When configured,
  closed segments upload every 10 minutes to a database *you* own. Local rows
  are deleted only after the cloud acknowledges them; without sync the app
  stays fully local.

## Install

```bash
cd ~/Work/atw
packaging/install.sh
```

That builds the release binaries, installs `atwd` + `atw` to `~/.local/bin`,
enables the systemd user unit, and installs + enables the shell plugin
(`dhirajarya.atw`, shown as **Dev Activity Watcher**).

## Cloud sync (optional, bring your own database)

No accounts, no Turso CLI, no OAuth — paste two values in the panel:

1. Open the panel, find **Cloud sync** (on by default).
2. Paste the database URL (`libsql://…` or `https://…turso.io`) and a
   database-scoped token.
3. Click **Save**. The panel verifies the connection and the URL stays
   visible; **Change** reopens the form later (a blank token keeps the
   saved one).

Sync then runs every 10 minutes (5/10/15/30 configurable) and retries with
exponential backoff after failures; **Sync now** forces an attempt. The panel
shows the last successful sync and how many segments are pending.

How the multi-device merge works, in one paragraph: every segment has a stable
UUID and every upload batch a deterministic ID, so retries and two devices
uploading at once are idempotent. Each batch lands in one transaction that
recomputes the affected day's per-app totals from the **union of intervals** —
overlapping time is never counted twice, and overlap between different apps is
reported as a separate `(overlapping)` category. Device clocks are not
corrected; segments are placed by their absolute timestamps.

## Uninstall

```bash
packaging/uninstall.sh            # keeps your recorded data
packaging/uninstall.sh --purge    # also deletes the database
```

Turning cloud sync off does not delete anything in the cloud; the database is
yours, and the schema/queries in [`docs/PORTFOLIO.md`](docs/PORTFOLIO.md) let
you read it with a read-only token.

## Configuration

`~/.config/atw/config.toml` (restart the daemon after editing:
`systemctl --user restart atwd`; the panel's Cloud section edits this file for
you):

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `true` | Master switch; `false` makes the daemon exit. |
| `idle_timeout_seconds` | `300` | Seconds without input before you count as AFK. |
| `flush_interval_seconds` | `420` | How often the open segment is written to SQLite. |
| `retention_days` | `7` | Local rows older than this are deleted at midnight (only after cloud ack, when sync is active). |
| `tracked_categories` | editor, browser, terminal, agent | Which app categories count. |
| `tracked_apps` | `[]` | Extra window classes to track, e.g. `["postman"]`. |
| `excluded_apps` | `[]` | Never track these, even if their category is listed. |
| `editor_project_names` | `false` | Append the project folder's basename to editor names ("Visual Studio Code · atw"). File names/paths are never stored. |
| `display_names` | built-ins | Friendly names per window class. |

Cloud (`[sync]`):

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `true` | Sync switch (panel toggle). Off = fully local. |
| `url` | `""` | Your Turso/libSQL database URL. |
| `token` | `""` | Database token (pasted in the panel; stored in this owner-only file, mode 0600). |
| `token_cmd` | `""` | Advanced: shell command printing the token (wins over `token`). |
| `interval_seconds` | `600` | 300 / 600 / 900 / 1800. |
| `cloud_retention_days` | `90` | Raw cloud segments older than this are pruned; daily aggregates stay. |

## CLI

```bash
atw status              # print the same JSON the panel reads
atw cleanup             # force the retention pass now
atw sync                # upload pending activity now
atw sync --check        # verify URL + token + schema, upload nothing
atw config set sync.url https://…      # used by the panel
atw config set sync.token -            # reads the token from stdin (no `ps` leak)
```

## Privacy

See [PRIVACY.md](PRIVACY.md) — the short version: without cloud sync,
everything stays in `~/.local/share/atw/activity.db` on this machine. With it,
only app names, categories, and time intervals go to the database you
configured — never window titles, paths, or keystrokes. The daemon has no
listening sockets and no telemetry.

## Build from source

```bash
cargo build --release
cargo test          # 105 unit/integration tests
packaging/install.sh
```

Requirements: Rust (stable), Hyprland, and the Omarchy shell. The release
binaries are small (≈2.5 MB); there are no Python/Node runtimes, no daemon
framework, and no polling loop.

[`docs/PORTFOLIO.md`](docs/PORTFOLIO.md) documents the cloud schema and
read-only queries for dashboards.

## License

MIT — see [LICENSE](LICENSE).
