# Omarchy Activity (atw) — TODO

## Phase 0 — Research ✅ (2026-11-04)
- [x] Verify Omarchy 4.0.6 / Hyprland 0.56.2 / Quickshell 0.3.2
- [x] Verify Hyprland event socket (`.socket2.sock`), command socket syntax (no newline!)
- [x] Verify real plugin manifest contract + `omarchy plugin validate`
- [x] Verify idle mechanism used by Omarchy shell (Quickshell IdleMonitor → ext-idle-notify-v1)
- [x] Verify systemd user service convention (`graphical-session.target`)
- [x] Scope locked: local-only storage, 7-day retention, no cloud sync

## Phase 1 — Rust core engine ✅ (2026-11-04)
- [x] Install Rust toolchain (`omarchy pkg add rust`, Rust 1.99.1 via pkexec)
- [x] Cargo project `atw` (binaries: `atwd` daemon + `atw` CLI: `run|status|cleanup`)
- [x] Config: `~/.config/atw/config.toml` (idle timeout, flush interval, exclusions, display names, retention)
- [x] Hyprland event socket listener (hand-rolled, reconnect with backoff)
- [x] Startup snapshot via command socket with read timeout
- [x] Application classification (configurable class → display name/category, exclusions)
- [x] In-memory aggregation + segment state machine
- [x] SQLite (WAL, indexes, `user_version` migrations) at `~/.local/share/atw/activity.db`
- [x] Flush on app change, periodic flush (default 7 min), flush on SIGTERM; open segment upserted in place
- [x] Status invariant: status.json counts only unflushed time (regression tested)
- [x] Structured logging (tracing, stderr → journald)
- [x] 37 unit tests green; live smoke test on Hyprland passed (status === DB sum)

## Phase 2 — Idle detection ✅ (2026-11-04)
- [x] ext-idle-notify-v1 Wayland client (event-driven, no polling; inhibitor-respecting)
- [x] Pause counting on idle, resume on activity; resume re-opens last app
- [x] Suspend/wake detection via wall-vs-monotonic clock drift (no extra deps)
- [x] Hyprland `lock`/`unlock` events parsed and wired (verified on real session pending lock test)
- [x] 43 unit tests green; live idle test passed (retroactive cut verified, stub row removed)

## Phase 3 — Statistics & retention ✅ (2026-11-04)
- [x] Daily schedule: retention tick at local midnight (in addition to startup)
- [x] Weekly summary in status.json (`week` map, top 12)
- [x] Display names configurable via config `display_names` (done in Phase 1)
- [x] Optional: project name from VS Code title (config-gated, basename only)

## Phase 4 — Omarchy shell plugin (QML) ✅ (2026-11-04)
- [x] `manifest.json` (bar-widget, settings schema) — passes `omarchy plugin validate`
- [x] Bar widget: current app, active/idle dot, today total
- [x] Expanded panel: header + state chip, hero total, today breakdown, weekly summary, privacy facts
- [x] Pixel-perfect alignment pass: header/hero centered in their own rows, rows keep name-left/time-right, toggle row height fixed
- [x] Plugin renamed **Dev Activity Watcher** (manifest name/displayName; id stays `dhirajarya.atw`)
- [x] QML font tokens: only `Style.font.body`/`title` used (running shell resolves others as undefined); explicit `fontSize` on section headers
- [x] Read daemon state via `FileView { watchChanges: true }` (inotify, no polling/HTTP)
- [x] `IpcHandler` target for manual refresh (`omarchy shell dhirajarya.atw toggle`)
- [x] Developer-apps-only tracking: `tracked_categories` allowlist (editor/browser/terminal/agent) + `tracked_apps` override; `excluded_apps` still wins
- [x] 58 unit tests green
- [x] Live verification: journal clean after reload; status.json contains only dev apps
- [x] Popup anchored directly to the bar widget (`centerOnBar: false`) — appears under the icon, not screen-centered

## Phase 5 — Packaging ✅ (2026-11-04)
- [x] `packaging/install.sh` (release build → `~/.local/bin/atwd`, systemd unit install + `enable --now`, plugin copy + enable)
- [x] `packaging/uninstall.sh` (`--purge` optionally deletes recorded data; config kept either way)
- [x] `packaging/atwd.service` (voxtype pattern + hardening: `NoNewPrivileges`, `ProtectSystem=strict`, `PrivateNetwork=true`, `IPAddressDeny=any` — the daemon cannot reach the network even by accident)
- [x] `README.md` (what it is, install/uninstall, config table, CLI)
- [x] `PRIVACY.md` (exact schema, how to inspect with sqlite3, how to delete)

## Phase 6 — Benchmarking & hardening ✅ (2026-11-04)
- [x] Release-build resource measurement: **0.0% CPU** over a 10 s idle window
      (0 jiffies — event-driven, zero polling), **5.6 MB RSS**, **2.5 MB binary**
- [x] Restart recovery verified live: daemon restarted repeatedly today
      (manual + systemd); every start re-queries the active window and resumes
      tracking with correct totals (status === DB invariant held)
- [x] Offline/suspend: suspend/resume transitions unit-tested (clock-drift
      detector); network reachability impossible by unit policy
      (`PrivateNetwork=true`, `IPAddressDeny=any`)
- [x] DB growth: one row per segment (~100 B); a heavy day of ~200 app switches
      ≈ 20 KB/day, bounded by the 7-day retention (16 KB after a full test day)

## Definition of Done
- [x] Local-only SQLite storage, no network anywhere (enforced at the unit level)
- [x] 7-day retention, automatic daily + manual `atw cleanup`
- [x] Event-driven Hyprland tracking, batched writes, retroactive idle cut
- [x] Wayland idle detection, suspend/resume handling
- [x] Developer-apps-only allowlist tracking
- [x] Omarchy shell plugin on the bar, pixel-perfect panel, direct popup anchoring
- [x] Installed as a product: `~/.local/bin/atwd` + enabled `atwd.service` + enabled plugin
- [x] Docs: README, PRIVACY, DEVELOPMENT log, TODO
