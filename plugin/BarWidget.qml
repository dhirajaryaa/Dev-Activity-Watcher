import QtQuick
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Ui

// Dev Activity Watcher — local-only developer app usage tracking.
//
// The bar shows today's total tracked time (dimmed while you are idle or
// the screen is locked). Clicking opens a panel with today's per-app
// breakdown and the weekly top apps. Only developer-relevant apps are
// tracked: editors, AI agents, browsers, and terminals.
//
// Data flow: the atwd daemon writes ~/.local/state/atw/status.json after
// every state change; this widget watches that file with Quickshell's
// FileView (inotify-based) — no polling loop, no sockets, no network.
BarWidget {
  id: root
  moduleName: "dhirajarya.atw"

  // ---- Daemon status ----
  property var status: null

  FileView {
    id: statusFile
    path: Quickshell.env("HOME") + "/.local/state/atw/status.json"
    watchChanges: true
    printErrors: false
    // `text()` is stale inside onFileChanged itself, so route every read
    // through reload() → onLoaded (same pattern as the shell's Color.qml).
    onFileChanged: reload()
    onLoaded: root.parseStatus(text())
    onLoadFailed: root.status = null
  }

  function parseStatus(content) {
    try {
      var parsed = JSON.parse(String(content || ""))
      root.status = parsed && typeof parsed === "object" ? parsed : null
    } catch (e) {
      root.status = null
    }
  }

  function refresh() {
    statusFile.reload()
  }

  readonly property string displayState: status ? String(status.state || "active") : "active"
  readonly property string displayApp: status && status.active ? String(status.active) : ""
  readonly property int displaySeconds: status ? Math.floor(Number(status.today_total_seconds) || 0) : 0

  // ---- Widget settings (shell.json entry, edited via the panel) ----
  readonly property bool showIdleDot: setting("showIdleDot", true)

  function setSetting(key, value) {
    var entry = { id: root.moduleName }
    for (var k in root.settings) if (k !== "id") entry[k] = root.settings[k]
    entry[key] = value
    // Applied locally first so the change is visible immediately; the
    // shell.json write comes back through the bar as the same value.
    root.settings = entry
    if (root.bar && root.bar.shell && typeof root.bar.shell.updateEntryInline === "function")
      root.bar.shell.updateEntryInline(root.moduleName, entry)
  }

  function formatDuration(seconds) {
    var s = Math.max(0, Math.floor(Number(seconds) || 0))
    var h = Math.floor(s / 3600)
    var m = Math.floor((s % 3600) / 60)
    if (h > 0) return h + "h " + (m < 10 ? "0" + m : m) + "m"
    if (m > 0) return m + "m"
    return s + "s"
  }

  readonly property string barText: root.vertical
    ? "\uf2b2"
    : "\uf2b2 " + formatDuration(displaySeconds)
  readonly property string barTooltip: displayApp !== ""
    ? displayApp + " — " + displayState
    : "Dev Activity Watcher"

  // ---- Panel plumbing (shape contract for shell.summon/hide/toggle) ----
  readonly property bool opened: panelLoader.item ? panelLoader.item.opened === true : false
  readonly property bool popoutSwitchClosing: panelLoader.item ? panelLoader.item.popoutSwitchClosing === true : false

  function open() {
    if (panelLoader.item) panelLoader.item.open()
  }

  function close() {
    if (panelLoader.item) panelLoader.item.close()
  }

  function togglePanel() {
    if (panelLoader.item) panelLoader.item.toggle()
  }

  function closeForPopoutSwitch() {
    if (panelLoader.item) panelLoader.item.closeForPopoutSwitch()
  }

  function injectPanel() {
    var target = panelLoader.item
    if (!target) return
    if ("bar" in target) target.bar = root.bar
    if ("settings" in target) target.settings = root.settings
    if ("anchorItem" in target) target.anchorItem = button
    if ("hostWidget" in target) target.hostWidget = root
  }

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  onBarChanged: injectPanel()
  onSettingsChanged: injectPanel()

  Loader {
    id: panelLoader
    active: true
    source: Qt.resolvedUrl("Panel.qml")
    visible: false
    onLoaded: {
      root.injectPanel()
      Qt.callLater(root.injectPanel)
    }
  }

  IpcHandler {
    target: "dhirajarya.atw"

    function open(): void { root.open() }
    function close(): void { root.close() }
    function show(): void { root.open() }
    function hide(): void { root.close() }
    function toggle(): void { root.togglePanel() }
    function refresh(): void { root.refresh() }
  }

  WidgetButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    text: root.barText
    tooltipText: root.barTooltip
    dimmed: root.showIdleDot && root.displayState !== "active"
    hasVisualContent: text !== ""

    onPressed: function(buttonCode) {
      root.togglePanel()
    }
  }
}
