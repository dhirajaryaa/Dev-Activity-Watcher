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

## Phase 2 — Idle detection (next)
- [ ] ext-idle-notify-v1 Wayland client (event-driven, no polling)
- [ ] Pause counting on idle, resume on activity; resume re-opens last app
- [ ] Screen lock (Hyprland `lock` event) + suspend/wake (wall-vs-monotonic drift)
- [ ] Verify lock events + lock (`omarchy system lock`) + manual lock shortcut

## Phase 3 — Statistics & retention
- [ ] Daily schedule: retention tick at local midnight (in addition to startup)
- [ ] Weekly summary query
- [ ] Improve display name for unknown classes (config `display_names`)
- [ ] Optional: project name from VS Code window title (config-gated, basename only)

## Phase 4 — Omarchy shell plugin (QML)
- [ ] `manifest.json` (bar-widget, settings schema) — passes `omarchy plugin validate`
- [ ] Bar widget: current app, active/idle dot, today total
- [ ] Expanded panel: today breakdown, weekly summary, data/uploads disclosure
- [ ] Settings: tracking on/off, idle timeout, exclusions, flush interval
- [ ] Read daemon state via `FileView { watchChanges: true }` (inotify, no polling/HTTP)
- [ ] `IpcHandler` target for manual refresh
- [ ] Real integration test inside omarchy-shell

## Phase 5 — Packaging
- [ ] `install.sh` (binary → `~/.local/bin`, systemd unit `atwd.service`, plugin copy + enable + `omarchy bar put`)
- [ ] `uninstall.sh` (stop/disable unit, remove plugin + bar entry, optional data purge with prompt)
- [ ] Upgrade & rollback notes; README; privacy doc (what is stored, how to inspect/delete)

## Phase 6 — Benchmarking & hardening
- [ ] Release-build resource measurement (idle CPU, RAM)
- [ ] Restart/offline/suspend/recovery integration tests
- [ ] DB size growth check over a week of use
