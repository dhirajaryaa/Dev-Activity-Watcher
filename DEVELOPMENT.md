# Omarchy Activity — Development Log

Project root: `~/Work/atw/` (all project files live here, never in /tmp).

Local-only, privacy-first activity tracker for Omarchy Linux (Hyprland + Quickshell).
Rust daemon + Omarchy shell plugin. No cloud sync. No network. No telemetry.

## Phase 0 — Research & Environment Inspection (2026-11-04)

Everything below was verified against the actual installed system. Nothing was modified.

### Environment

| Component | Finding |
|---|---|
| Omarchy | 4.0.6-1 (`omarchy version`) |
| Hyprland | 0.56.2 |
| Quickshell | 0.3.2 (`quickshell -n -p /usr/share/omarchy/shell`) |
| Session | Wayland via uwsm; `DESKTOP_SESSION=omarchy`, `XDG_SESSION_TYPE=wayland` |
| Rust toolchain | **NOT installed** (`rustc`, `cargo` missing) |
| hypridle | Not installed and not running |
| Existing user plugins | `stappmus.activity-monitor` (third-party bar widget, enabled) |
| systemd user session | `graphical-session.target` is active |

### Hyprland IPC (verified)

- Command socket: `$XDG_RUNTIME_DIR/hypr/<sig>/.socket.sock`
- **Event socket: `$XDG_RUNTIME_DIR/hypr/<sig>/.socket2.sock`** (line-based events: `activewindow>>class,title`, `openwindow`, `closewindow`, `workspace>>`, lock events, …)
- `hyprctl activewindow -j` works for startup snapshot / reconnect recovery.
- Sockets are dotfiles — `ls` needs `-a` to show them.

### Omarchy shell plugin framework (verified, not invented)

- User plugins live in `~/.config/omarchy/plugins/<plugin-id>/` with a `manifest.json`.
- Manifest shape confirmed from two real, working plugins:
  - `stappmus.activity-monitor` (bar-widget with settings schema)
  - `/usr/share/omarchy/shell/plugins/services/idle/manifest.json` (service kind)
- Manifest fields: `schemaVersion: 1`, `id`, `name`, `version`, `author`, `license`,
  `description`, `kinds`, `entryPoints`, plus per-kind block (e.g. `barWidget`
  with `displayName`, `category`, `defaultSection`, `defaults`, `schema`).
- Plugin kinds seen in `omarchy plugin list`: `bar-widget`, `service`, `panel`, `overlay`, `bar`.
- Service plugins use `keepLoaded: true` and `entryPoints.service`.
- Verified CLI surface (no invented commands):
  - `omarchy plugin add|clone|enable|disable|list|remove|update|validate`
  - `omarchy plugin enable <id> --section right`
  - `omarchy bar put <id> --after <id>` / `omarchy bar set <id> <key> <value>`
  - `omarchy plugin validate <folder>` — validates manifest against the real schema.
- Plugin QML runs inside the omarchy-shell process. Conventions observed:
  - `import qs.Commons`, `import qs.Ui`, `Panel` base component with `moduleName` / `ipcTarget`.
  - `Quickshell.Io.FileView { watchChanges: true }` = inotify file watching (used by omarchy.idle).
  - `IpcHandler { target: "..." }` exposes functions callable via `omarchy-shell <target> <fn>`.
  - Hot-reload on save under `~/.config/omarchy/plugins/`.
- Idle detection in Omarchy itself: `IdleMonitor` from Quickshell
  (`Quickshell` module), backed by Wayland `ext-idle-notify-v1`, `respectInhibitors: true`.
  User's current shell.json: `idle.lock = 300`, `idle.screensaver = 150`.

### systemd user service convention (verified)

Existing example `~/.config/systemd/user/voxtype.service`:
`PartOf=graphical-session.target`, `After=graphical-session.target`,
`Restart=on-failure`, `WantedBy=graphical-session.target`. We follow the same pattern.

### Scope decisions (from project brief update)

- **Local-only storage** (SQLite). No cloud sync, no HTTP client, no API contract, no telemetry.
- 7-day retention, automatic cleanup (daily) + manual cleanup command.
- Event-driven tracking; batched SQLite writes (5–10 min) + flush on app change / shutdown.
- Idle (AFK) excluded from usage; default threshold 5 min.
- Browser extension / WakaTime / portfolio integration: **out of scope for MVP**.

### Open decisions

1. Rust install method: `omarchy pkg add rust` (needs sudo) vs rustup (user-level, no sudo).
2. Plugin id: proposal `dhirajarya.activity` (matches `<username>.<name>` convention).
3. UI shape: one bar-widget whose expanded panel contains Activity + Settings
   (pattern used by stappmus.activity-monitor) instead of separate panel kind.

---

## Phase 1 — Rust Core Engine (2026-11-04)

Approved decisions: project named **atw** everywhere (crate `atw`, daemon `atwd`,
plugin `dhirajarya.atw`, config dir `~/.config/atw`); Rust installed via
`omarchy pkg add rust` (Rust 1.99.1, installed with pkexec approval).

### What was built

| File | Role (plain language) |
|---|---|
| `src/hypr.rs` | Talks to Hyprland: streams window events from `.socket2.sock`, reconnects with backoff (1s→60s), re-queries the active window after every reconnect so tracking resyncs. Hand-rolled client (~150 lines) instead of the `hyprland` crate — the event protocol is a few text lines, and this avoids version-drift risk. |
| `src/tracker.rs` | The heart: a state machine turning events into usage *segments* (app X from time A to time B). Pure functions with an injected clock → fully unit-testable. |
| `src/classify.rs` | Window class → friendly name + category (editor/browser/terminal/media). Built-in table + user overrides in config. Exclusion list supported. |
| `src/db.rs` | SQLite (bundled, WAL mode). One row per usage segment; the open segment is **upserted in place** at each flush so there are never duplicate rows. Segments crossing midnight are split so `date` is always correct. Migrations via `PRAGMA user_version`. |
| `src/status.rs` | Writes `~/.local/state/atw/status.json` atomically (temp file + rename) — this is what the QML plugin will read with inotify watching. |
| `src/config.rs` | TOML config at `~/.config/atw/config.toml`; created once with documented defaults, never overwritten. |
| `src/lib.rs` | Orchestrates everything: one owner task holds tracker + DB; all inputs (Hyprland events, flush tick, SIGTERM/SIGINT) arrive over one channel. No locks, no polling loops. |
| `src/bin/atw.rs` / `src/bin/atwd.rs` | `atw run|status|cleanup` CLI; `atwd` is the daemon entry used by systemd. |

### Key design decisions (why)

1. **Idle is cut retroactively.** Wayland idle notifications fire `timeout`
   seconds *after* the user stopped. We close the segment at
   `now − idle_timeout`, so AFK time is never counted as app usage.
2. **Open segment = one DB row, updated in place.** Flush every ~7 min
   UPDATEs the same row (INSERT on first flush). Closing an already-flushed
   segment issues a finalizing UPDATE — no duplicates ever.
3. **Zero-duration finalize deletes the stub row**, keeping the DB clean.
4. **Crash safety without a recovery table:** data is only lost between
   flushes; on restart the tracker seeds from DB + a fresh window snapshot.
5. **Corruption drill:** if the DB can't be opened/quick-checked, it is
   renamed to `activity.corrupt-<timestamp>` (never deleted) and a fresh DB
   is created.
6. **No window titles are persisted.** Only class, display name, category,
   timestamps. Titles stay in memory only (and are unused for now).
7. **Midnight split uses binary search over the date function** so the split
   logic is testable with a deterministic UTC date function, independent of
   the host timezone.

### Test results (2026-11-04)

- `cargo test`: **37 passed, 0 failed** (classification, duration math, idle/lock
  transitions, midnight splits, DB upsert/finalize/retention, corruption drill,
  status roundtrip, event parsing).
- Live smoke test on the real Hyprland session:
  - Daemon startup: **12 ms** to connect to the event socket (no polling).
  - Real alt-tab flows (agent → Brave → foot) produced correct per-app segments:
    e.g. `foot|6s`, `brave-browser|1s`, `org.omarchy.agent|2s` rows with correct dates.
  - Graceful SIGTERM: closes the open segment, flushes, WAL-checkpoints, exits.
  - **Invariant verified: `status.json` totals === SQLite SUM** after flush
    (22s vs 22s, no double counting).

### Important discoveries during Phase 1

1. **Hyprland IPC requests must NOT end with a newline.**
   `j/activewindow\n` is rejected with `unknown request`; `j/activewindow`
   (no `\n`) works. This matches community reports for 0.56 (`echo` vs
   `printf` behaviour with socat). A 120 ms read-timeout additionally guards
   the snapshot read, since the server does not always close promptly.
2. Status invariant: `today_pieces` may only count seconds **not already
   stored** in the DB. The open segment is therefore counted from the last
   flush anchor (`(row_id, flushed_at)`), and a closed-but-stale row is
   rewritten via DELETE + re-INSERT of date-split pieces. Regression tests
   cover both directions.
3. The event socket (`socket2`) streams the classic line protocol
   (`activewindow>>class,title`, `openwindow`, `closewindow`, …). The
   command socket syntax differs from old Hyprland docs — verified empirically.

---

## Phase 2 — Idle Detection (2026-11-04)

### What was built

| File | Role |
|---|---|
| `src/idle.rs` | Wayland client binding `ext-idle-notify-v1` (the same protocol Omarchy's shell uses). Dedicated OS thread with a blocking event loop; compositor pushes `idled`/`resumed` events → `Input::Idle`/`Input::Active`. Zero polling. Reconnects with backoff if the compositor restarts. |
| `src/suspend.rs` | Suspend/resume detection with **no extra dependencies**: `CLOCK_MONOTONIC` (Rust `Instant`) freezes during sleep while the wall clock advances, so a growing wall−monotonic offset reveals a suspend. Segments are cut at the last moment the machine was known to be awake. |

### Design decisions (why)

1. **Inhibitor-respecting idle.** We use `get_idle_notification` (not the v2
   `get_input_idle_notification`): video players and "stay awake" toggles
   hold an idle inhibitor, and the compositor then never reports idle —
   screen-on time counts as activity, matching the desktop's own semantics.
2. **Retroactive cut on idle.** The compositor fires `idled` exactly
   `timeout` seconds after the last input; the segment is closed at
   `now − idle_timeout`, so AFK time is never counted.
3. **Suspend semantics.** On drift detection: close the open segment at the
   last awake moment, then — only if the user was *active* before sleeping —
   re-open the last app at resume time. If they were idle/locked before,
   nothing re-opens until a real event, so post-resume AFK is not attributed.
   A 10 s threshold ignores small NTP adjustments.
4. **Dedicated thread for Wayland.** The protocol is push-only; a blocking
   `blocking_dispatch` loop on its own thread is simpler and cheaper than
   async Wayland integration. Events cross into the async world via
   `blocking_send` on the existing channel.

### API notes (wayland-client 0.31.15 / wayland-protocols 0.32.13)

- `Dispatch::event` takes `state: &mut State` as an explicit first
  parameter — not a `&mut self` receiver.
- The notification requires a `wl_seat`: bind both `ext_idle_notifier_v1`
  (≤ v2) and `wl_seat` globals; they arrive in arbitrary order, so the
  notification is created once both are present.
- Events are named **`idled`** and `resumed` (the XML renamed `fired`).
- The event loop method is `blocking_dispatch` (plain `dispatch` does not
  exist; `dispatch_pending` is the non-blocking variant).

### Test results (2026-11-04)

- `cargo test`: **43 passed, 0 failed** (adds suspend-detector math and
  suspend/resume tracker transitions).
- Live test (10 s idle timeout, isolated dirs): `idle monitor armed` on the
  real compositor; `Input::Idle` arrived exactly 10 s after the last input;
  the retroactive cut removed the 5-second stub row that had been flushed
  mid-idle — final DB total **0 s**, status `state: "idle"`. Correct.

---

## Phase 3 — Statistics & Retention (2026-11-04)

### What was built

1. **Midnight retention tick.** A task sleeps until the next *local*
   midnight, then sends `Input::Retention`; the daemon deletes rows older
   than the retention window and rewrites the status file. Together with the
   startup pass, retention runs even if the machine is always on (or always
   off). DST-safe: if local midnight is ambiguous, it retries in an hour.
2. **Weekly summary.** `Db::week_totals` (top 12 apps over the last 7 days)
   is merged with unflushed in-memory time and written into `status.json` as
   the `week` map — the QML panel reads it with zero extra queries.
3. **Editor project names (config-gated, off by default).**
   `tracking.editor_project_names = true` appends the project folder's
   *basename* from VS Code-style titles ("file - folder - Visual Studio
   Code" → "Visual Studio Code · folder"). File names and paths are never
   stored. Switching projects inside the same editor correctly cuts a new
   segment (display name is part of the segment identity now).

### Test results (2026-11-04)

- `cargo test`: **47 passed, 0 failed**.
- Live: status.json now carries `week` alongside `today`; totals still match
  the DB exactly.

---

## Phase 4 — Omarchy Shell Plugin (2026-11-04)

The plugin is named **Dev Activity Watcher** (manifest `name` /
`displayName`). The technical id stays `dhirajarya.atw` — it is what
`shell.json` and the IPC target reference, and renaming it would require
re-enabling the plugin for no visible benefit.

### What was built

| File | Role |
|---|---|
| `plugin/manifest.json` | Bar-widget plugin contract: id `dhirajarya.atw`, settings schema for `showIdleDot`, default section `right`. Passes `omarchy plugin validate`. |
| `plugin/BarWidget.qml` | Bar item: nerd-font glyph + today's total, dimmed while idle/locked (configurable). Watches `~/.local/state/atw/status.json` with `FileView { watchChanges: true }` (inotify — no polling, no HTTP). Owns the panel plumbing (`open/close/toggle`, `IpcHandler` target `dhirajarya.atw`). |
| `plugin/Panel.qml` | Expanded panel: header + live state chip, hero total, today's per-app rows with proportional bars, weekly top apps, widget toggle, privacy facts. |

### Pixel-perfect alignment pass

The user asked for element-level centering ("center what should be centered,
not the whole window"). Verified against the clock panel's conventions
(`/usr/share/omarchy/shell/plugins/panels/clock/Panel.qml`):

- **Header** (title + state chip) and **hero total** are centered *within
  their own full-width rows* — the clock panel's hero pattern
  (`Item { width: parent.width }` + `anchors.horizontalCenter`, because
  Column children must not carry anchors).
- **Section labels** stay left-aligned (they head left-aligned lists, like
  every Omarchy panel).
- **List rows** keep the activity-monitor shape: name left, time right,
  both `anchors.verticalCenter`-aligned to each other, row height =
  `Math.max` of both texts so nothing overflows.
- **Empty-state texts** are centered.
- The toggle row's height now accounts for the switch being taller than the
  label (`Math.max(dimLabel.implicitHeight, idleToggle.implicitHeight)`), so
  the switch no longer overflows its row box.

### QML font tokens (running-shell constraint)

The running shell (PID 1717) only resolves `Style.font.body` and
`Style.font.title` from plugin code — `heading`, `caption`, `small`,
`headline` assign `undefined` (journal: "Unable to assign [undefined] to
int"). The plugin therefore uses only those two tokens, and passes
`fontSize: Style.font.body` explicitly to every `PanelSectionHeader` so the
component's own `caption` default can never produce an undefined assignment.
`Font.AllLowercase` was also undefined and was removed.

### Developer-apps-only tracking (allowlist)

The user asked to track **only day-to-day developer apps** — not every app.
The classifier moved from a blocklist model to an allowlist model:

- New `Category::Agent` for AI coding agents/assistants (Cursor, Windsurf,
  Trae, Claude, ChatGPT, Gemini…). Everything `jetbrains-*` classifies as
  Editor via a prefix rule.
- `tracking.tracked_categories` (default `["editor", "browser", "terminal",
  "agent"]`) — the categories that count. Media players, file managers,
  games, and unclassified apps are ignored.
- `tracking.tracked_apps` — individual classes to track even when their
  category is not listed (e.g. `["postman"]`).
- `tracking.excluded_apps` still wins over the allowlist (e.g. exclude
  `chromium` while keeping Firefox).
- The tracker gates segments on `AppInfo.tracked` (allowlist ∧ ¬excluded)
  instead of `!excluded`. Non-tracked focus changes still close the open
  segment, so time never leaks across an untracked app.

### Installation state

- Plugin installed at `~/.config/omarchy/plugins/dhirajarya.atw/`, enabled
  via `omarchy plugin enable dhirajarya.atw --section right`, IPC verified
  (`omarchy shell dhirajarya.atw toggle`).
- Config installed at `~/.config/atw/config.toml` with the allowlist.

### Test results (2026-11-04)

- `cargo test`: **58 passed, 0 failed** (11 new: allowlist gating, category
  narrowing, exclusion-wins, agent classification, jetbrains prefix,
  tracker-level non-dev behaviour).

---

## Phase 5 — Packaging (2026-11-04)

| File | Role |
|---|---|
| `packaging/atwd.service` | systemd user unit (voxtype pattern: `PartOf`/`After=graphical-session.target`, `Restart=on-failure`, `XDG_RUNTIME_DIR=%t`) plus hardening — `NoNewPrivileges`, `ProtectSystem=strict`, `ReadWritePaths` limited to the three atw dirs, `PrivateNetwork=true`, `IPAddressDeny=any`. The daemon cannot reach the network even if a future bug tried. |
| `packaging/install.sh` | Builds release, installs `~/.local/bin/atwd`, installs + enables the unit, copies the plugin into `~/.config/omarchy/plugins/dhirajarya.atw/`, enables it in the right bar section if not already enabled. Idempotent. |
| `packaging/uninstall.sh` | Stops/disables the unit, removes binary + unit + plugin. Data is kept unless `--purge`. |
| `README.md` / `PRIVACY.md` | User docs; the privacy doc documents the exact DB schema (verified against `.schema`), how to inspect with sqlite3, and every deletion path. |

## Phase 6 — Benchmarks & hardening (2026-11-04)

Measured on the running release daemon (systemd-managed):

| Metric | Value |
|---|---|
| Idle CPU (10 s window) | **0 jiffies → 0.0%** of one core |
| RAM (RSS) | **5.6 MB** (peak 4 MB reported by systemd earlier in the session) |
| Binary size | **2.5 MB** |
| DB size | 16 KB after a full test day; growth ≈ one ~100 B row per app segment, bounded by 7-day retention |

Hardening verified live: the daemon was restarted repeatedly during
development (manual runs + systemd); every start re-queried the active
window via the command socket and resumed with correct totals — the
status === DB invariant held every time. Suspend/resume transitions are
unit-tested via the clock-drift detector.

### Final installation state

- `~/.local/bin/atwd` — release binary
- `atwd.service` — enabled, running (`systemctl --user status atwd`)
- Plugin `dhirajarya.atw` ("Dev Activity Watcher") — installed + enabled, right bar section
- Config `~/.config/atw/config.toml` — developer-app allowlist
- Data `~/.local/share/atw/activity.db` — local only, 7-day retention

### Test results (final)

- `cargo test`: **60 passed, 0 failed**.

---

## Phase 7 — Optional cloud sync, multi-device, panel setup (2026-11-04)

The user extended the product: keep the local engine untouched, but add
**optional sync to a Turso database they own**, set up entirely from the panel
(paste URL + token, no Turso CLI, no OAuth), aggregate several machines
correctly, and keep the bar/panel minimal. Cloud is on by default but does
nothing until a URL and token exist.

### Architecture (and why)

| Piece | Choice | Why, in plain language |
|---|---|---|
| Local storage | **rusqlite** (unchanged) | The tracker must work offline and own its schema; no rewrite risk. |
| Cloud client | hand-rolled `/v2/pipeline` over `ureq` + rustls | The `turso` crate mirrors the whole database (it propagates local deletes) and `diesel-libsql` is immature. The Hrana protocol is small: one POST with `BEGIN` + statements, a second with `COMMIT`/`ROLLBACK` + `close`, held together by a baton. We already hand-roll Hyprland; same philosophy. |
| Backends | `CloudBackend` trait: `LocalBackend` (in-memory rusqlite, used by tests) + `HttpBackend` (production) | All merge/retention logic is testable without a network or account. |
| Sync loop | separate task, own SQLite connection (WAL), blocking HTTP inside `spawn_blocking` | A slow network can never stall event handling or the open segment. |
| Merge oracle | `merge.rs` (Rust interval union) + SQL window functions in the cloud transaction, cross-checked by tests | Two independent implementations of the hard part, agreeing on fixtures. |

### Upload transaction (all-or-nothing)

One `/v2/pipeline` request carries `BEGIN` plus:

1. insert the batch id (duplicate → rollback → treat as **already applied**);
2. `INSERT OR IGNORE` every segment (stable UUID primary key);
3. for each affected date: delete and recompute `daily_app` rows from the
   **union of intervals** using a window-function sweep;
4. recompute `daily_unique` (global union + `overlap_seconds`);
5. upsert the device row and `meta`;

then a second request commits (or rolls back) and closes the baton. A crash
mid-transaction is rolled back by the server when the connection times out;
the daemon's next attempt uses the *same deterministic batch id*, so a retry
after "commit succeeded, ack lost" is recognised as a duplicate and only the
local `synced_at` marker is written.

### Idempotency and concurrency

- Batch id = FNV-1a 128-bit over `device_id` + sorted segment UUIDs. Stable
  across retries and upgrades (the algorithm is ours, not a crate's).
- Segment UUIDs are UUID v7 generated at insert time; `INSERT OR IGNORE` makes
  re-uploads harmless.
- Five concurrent devices were simulated in tests: the transaction serialises
  on the server, and recompute-from-raw is commutative, so late batches simply
  recompute the affected day again.
- Local rows are marked synced only after the cloud ack:
  `UPDATE ... WHERE synced_at IS NULL` (a second ack of the same batch is a
  no-op). Rows are never deleted before that ack.

### Correctness rules for multi-device time

- **Never sum overlapping durations.** Per-app totals come from the interval
  union; `combined_seconds` is kept separately for ranking.
- Overlap **between different apps/devices** is attributed to the explicit
  `(overlapping)` category via `daily_unique.overlap_seconds`; it is not
  silently added to any app and not folded into `unique_active_seconds`.
- `unique_active_seconds` is the union of everything, so ≤ 86 400 per day —
  no `MIN(…, 86400)` clamp masking bad data.
- Clock jumps backwards are handled: a segment ending before it starts is
  rejected at the tracker boundary; overlapping segments from a clock jump are
  merged, never double-counted (regression test).

### Retention

- Local: 7 days, but with sync active only rows with `synced_at IS NOT NULL`
  are deleted (unsynced data survives any retention pass).
- Cloud: raw segments older than `cloud_retention_days` (90) are pruned once a
  day on successful contact; `daily_app`/`daily_unique` are preserved, so
  long-range charts keep working after the raw rows are gone.

### Panel and onboarding

- `Panel.qml` now defaults to **Today**; a small **History** button swaps in
  the last 7 days (top two apps per day) on demand.
- **Cloud sync** section: toggle (default ON), and — only until configured —
  URL + token fields and a **Connect** button. Connect writes the config
  through `atw config set` (token piped on stdin, so it never appears in
  `ps`), then restarts `atwd` so the change applies immediately.
- Once configured: last-sync time, pending count, and **Sync now**.
- Bar: right-click cycles icon → icon + hours → icon + hours + minutes,
  persisted as the `barFormat` enum setting; hours are shown as `1h`, not
  `1h 02m`.

### systemd hardening change (explained)

`PrivateNetwork=true` and `IPAddressDeny=any` had to go: they make *any*
outbound connection impossible, which contradicts a runtime-toggleable sync
feature. Everything else stays (`ProtectSystem=strict`,
`ProtectHome=read-only`, `ReadWritePaths` limited to the three atw dirs,
`NoNewPrivileges`, `RestrictNamespaces`, `LockPersonality`,
`RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6`). The daemon opens no
listening socket and connects only when sync is enabled and configured.
`PRIVACY.md` documents this trade-off explicitly.

### New CLI surface

```
atw sync                 # upload pending rows now (also called by atw-sync.service)
atw sync --check         # verify URL/token/schema, upload nothing
atw config set <key> <value|->   # "-" reads the value from stdin
```

`atwd` also handles `SIGUSR1` as "flush + sync now" (used by the panel and by
`systemctl --user kill --signal=USR1 atwd`). `status.json` schema is now
version 2 and carries `history` (7 days, top 4 apps) plus a `sync` block
(`enabled`, `configured`, `pending`, `last_sync`, `last_error`).

### Test results (2026-11-04, final)

- `cargo test`: **104 passed, 0 failed** — includes merge fixtures (overlap,
  late arrival, clock jump), duplicate-batch retry, offline resumability,
  retention cutoff, read-only portfolio queries, and a 5-thread concurrent
  upload test.
- `atw config set` tests cover comment preservation, adding a missing
  `[sync]`, and refusing invalid values without touching the file.

