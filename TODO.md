# Omarchy Activity — TODO

## Phase 0 — Research ✅ (2026-11-04)
- [x] Verify Omarchy / Hyprland / Quickshell versions
- [x] Verify Hyprland event socket exists (`.socket2.sock`)
- [x] Verify real plugin manifest contract + `omarchy plugin validate`
- [x] Verify idle mechanism used by Omarchy shell (Quickshell IdleMonitor → ext-idle-notify-v1)
- [x] Verify systemd user service convention (`graphical-session.target`)
- [x] Identify missing Rust toolchain
- [ ] Await approval of architecture + scope

## Phase 1 — Rust core engine
- [ ] Install Rust toolchain (method pending approval)
- [ ] Cargo project `omarchy-activity` (binary: `omarchy-activityd` + CLI subcommands `status`, `cleanup`)
- [ ] Config: `~/.config/omarchy-activity/config.toml` (idle_timeout, flush_interval, exclusions, per-app display names)
- [ ] Hyprland event socket listener (hand-rolled, reconnect with backoff)
- [ ] Startup snapshot via `hyprctl activewindow -j`
- [ ] Application classification (configurable mapping class → display name/category)
- [ ] In-memory aggregation + segment state machine
- [ ] SQLite (WAL, indexes, migrations) at `~/.local/share/omarchy-activity/`
- [ ] Flush on app change, periodic flush (default ~7 min), flush on SIGTERM
- [ ] Crash recovery: close orphaned open segments on startup
- [ ] Structured logging (tracing) to `~/.local/state/omarchy-activity/`
- [ ] Unit tests: classification, duration math, midnight boundary, app switching

## Phase 2 — Idle detection
- [ ] ext-idle-notify-v1 Wayland client (event-driven, no polling)
- [ ] Pause counting on idle, resume on activity
- [ ] Handle screen lock (Hyprland `lock` event) and suspend/wake (wall-vs-monotonic drift)
- [ ] Tests: idle transitions, no double counting

## Phase 3 — Statistics & retention
- [ ] Daily stats query (derived from segments, indexed)
- [ ] Weekly summary
- [ ] 7-day retention cleanup (startup + daily tick, in transaction)
- [ ] Manual `cleanup` subcommand
- [ ] App exclusions
- [ ] Status files for UI: `status.json` (now) + `today.json` (daily breakdown)

## Phase 4 — Omarchy shell plugin (QML)
- [ ] `manifest.json` (bar-widget kind, settings schema) — passes `omarchy plugin validate`
- [ ] Bar widget: current app, active/idle dot, today total
- [ ] Expanded panel: today breakdown, weekly summary, last flush time
- [ ] Settings in panel: tracking on/off, idle timeout, exclusions, flush interval
- [ ] Reads daemon state via `FileView watchChanges` (no polling, no HTTP)
- [ ] `IpcHandler` target for manual refresh
- [ ] Real integration test inside omarchy-shell

## Phase 5 — Packaging
- [ ] `install.sh` (binary → `~/.local/bin`, systemd unit, plugin copy + enable + `omarchy bar put`, config never overwritten)
- [ ] `uninstall.sh` (disable/remove unit, remove plugin + bar entry, optional data purge prompt)
- [ ] Upgrade & rollback notes in README
- [ ] README + privacy doc (what is stored, how to inspect/delete)

## Phase 6 — Benchmarking & hardening
- [ ] Release build resource measurement (idle CPU, RAM)
- [ ] Offline/restart/suspend/recovery integration tests on Hyprland
- [ ] DB corruption drill (move aside + recreate + log)
- [ ] DB size growth check over a week
