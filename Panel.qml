import QtQuick
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Ui

// Dev Activity Watcher — expanded view.
//
// Default view is TODAY only. The small "History" button swaps the section
// to the last 7 days (on demand — no history clutter by default).
//
// Cloud sync is optional and off the main path: a toggle (default ON), and
// a first-time setup where the user pastes the database URL + token right
// here (no config file editing). "Sync now" asks the daemon to sync
// immediately; otherwise it runs every 10 minutes on its own.
//
// Alignment conventions (matching the clock panel): the header and the hero
// total are centered within their own full-width rows; section labels head
// their lists from the left; list rows keep the activity-monitor shape —
// name left, time right. Nothing is centered by centering the window.
Panel {
  id: root
  moduleName: "dev-activity-watcher.atw"
  ipcTarget: "dev-activity-watcher.atw"
  manageIpc: false

  property var anchorItem: null
  property var hostWidget: null
  readonly property var barIdentity: hostWidget || root

  readonly property var status: hostWidget ? hostWidget.status : null
  readonly property string state: status ? String(status.state || "active") : "active"
  readonly property int todaySeconds: status ? Math.floor(Number(status.today_total_seconds) || 0) : 0

  readonly property var todayList: sortedEntries(status ? status.today : null)
  readonly property var weekList: sortedEntries(status ? status.week : null)
  readonly property real todayMax: todayList.length > 0 ? todayList[0].seconds : 1

  // ---- History (shown only when the user clicks History) ----
  property bool historyMode: false
  readonly property var historyList: status && status.history ? status.history : []

  // ---- Cloud sync ----
  readonly property var syncInfo: status && status.sync ? status.sync : null
  readonly property bool syncConfigured: syncInfo ? syncInfo.configured === true : false
  readonly property string savedUrl: syncInfo && syncInfo.url ? String(syncInfo.url) : ""
  // Optimistic local value so the switch throws immediately; the daemon's
  // refreshed status takes over when the panel is reopened.
  property var syncOnOverride: null
  readonly property bool syncOn: syncOnOverride !== null
    ? syncOnOverride
    : (syncInfo ? syncInfo.enabled === true : true)
  property string urlInput: ""
  property string tokenInput: ""
  property string connectError: ""
  property string noteText: ""
  property bool connecting: false
  property bool syncingNow: false
  // Configured users can reopen the form to change URL/token ("Change").
  property bool editingCloud: false
  readonly property bool showCloudForm: syncOn && (!syncConfigured || editingCloud)

  // ---- Delete (purge one app everywhere) ----
  // Hover shows a bigger ✕; clicking arms the row with a plain
  // "Delete … everywhere?" question that STAYS until the user picks
  // Delete or Cancel — no auto-reverting timer to race against. After a
  // successful purge the daemon restarts so the numbers and status file
  // settle immediately.
  property string confirmDelete: ""
  property string deleteNote: ""

  Timer {
    id: clearDeleteNote
    interval: 8000
    onTriggered: root.deleteNote = ""
  }

  // If the purge command ever hangs (a spawn failure reports as a stuck
  // running state), recover the row UI after a generous grace period.
  Timer {
    id: purgeWatchdog
    interval: 20000
    onTriggered: {
      if (root.deleting === "") return
      root.deleting = ""
      root.deleteNote = "Delete timed out — run \u201Catw delete-app\u201D in a terminal"
      root.clearDeleteNote.restart()
    }
  }

  function deleteApp(name) {
    if (runPurge.running) return // one purge at a time
    confirmDelete = ""
    deleting = name
    runPurge.appName = name
    purgeWatchdog.restart()
    runPurge.running = true
  }

  // Set while the purge command runs, so the row shows feedback instead of
  // a delete affordance (double-click protection).
  property string deleting: ""

  // Never reopen into a half-finished edit or a stale success note.
  onOpenedChanged: {
    if (opened) {
      syncOnOverride = null
      editingCloud = false
      connectError = ""
      noteText = ""
      urlInput = ""
      tokenInput = ""
      confirmDelete = ""
      deleteNote = ""
      deleting = ""
    }
  }

  readonly property string atwBin: Quickshell.env("HOME") + "/.local/bin/atw"

  function sortedEntries(map) {
    var entries = []
    if (map) {
      for (var key in map) {
        var seconds = Math.floor(Number(map[key]) || 0)
        if (seconds > 0) entries.push({ name: key, seconds: seconds })
      }
    }
    entries.sort(function(a, b) { return b.seconds - a.seconds })
    return entries
  }

  function formatDuration(seconds) {
    return hostWidget ? hostWidget.formatDuration(seconds) : Math.floor(Number(seconds) || 0) + "s"
  }

  function dayLabel(dateStr) {
    var parts = String(dateStr || "").split("-")
    if (parts.length !== 3) return String(dateStr || "")
    var d = new Date(Number(parts[0]), Number(parts[1]) - 1, Number(parts[2]))
    var now = new Date()
    var today = new Date(now.getFullYear(), now.getMonth(), now.getDate())
    var diff = Math.round((today.getTime() - d.getTime()) / 86400000)
    if (diff === 0) return "Today"
    if (diff === 1) return "Yesterday"
    var names = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]
    return names[d.getDay()] + " " + (d.getDate() < 10 ? "0" : "") + d.getDate()
  }

  // Top two apps of a day, e.g. "VS Code 3h · Foot 40m".
  function topApps(map) {
    var list = sortedEntries(map)
    var parts = []
    for (var i = 0; i < Math.min(2, list.length); i++)
      parts.push(list[i].name + " " + formatDuration(list[i].seconds))
    return parts.join(" · ")
  }

  function shortTime(rfc) {
    var d = new Date(String(rfc || ""))
    if (isNaN(d.getTime())) return ""
    var h = d.getHours(), m = d.getMinutes()
    return (h < 10 ? "0" : "") + h + ":" + (m < 10 ? "0" : "") + m
  }

  function syncLine() {
    if (!syncOn) return "Off — stays local"
    if (!syncConfigured) return "Paste your database URL and token"
    if (syncingNow) return "Syncing…"
    var info = syncInfo || {}
    var pending = Math.floor(Number(info.pending) || 0)
    if (info.last_error) return "Sync failed — will retry"
    if (info.last_sync && shortTime(info.last_sync) !== "") {
      var line = "Synced " + shortTime(info.last_sync)
      return pending > 0 ? line + " · " + pending + " pending" : line
    }
    return pending > 0 ? pending + " pending" : "Not synced yet"
  }

  function databaseLabel() {
    // The scheme adds no information here; the host is what identifies the
    // database. Wrapped to two lines by the label so it stays fully visible.
    return root.savedUrl.replace(/^[a-z]+:\/\//, "")
  }

  function openCloudEditor() {
    urlInput = root.savedUrl
    tokenInput = ""
    connectError = ""
    noteText = ""
    editingCloud = true
  }

  function cancelCloudEditor() {
    editingCloud = false
    urlInput = ""
    tokenInput = ""
    connectError = ""
    noteText = ""
  }

  function connect() {
    var url = urlInput.trim()
    if (url === "") {
      connectError = "Paste the database URL"
      return
    }
    // The token is only required the first time; when changing an existing
    // setup an empty field means "keep the saved token".
    if (!syncConfigured && tokenInput.trim() === "") {
      connectError = "Paste the database token"
      return
    }
    connectError = ""
    noteText = ""
    connecting = true
    // Saving the config is the goal; the connection check afterwards only
    // reports whether the values work.
    saveUrl.running = true
  }

  // ---- Command plumbing (all through the installed `atw` CLI) ----
  Process {
    id: saveUrl
    command: [root.atwBin, "config", "set", "sync.url", root.urlInput.trim()]
    onExited: function(code) {
      if (code !== 0) {
        root.connecting = false
        root.connectError = "Could not save the database URL"
        return
      }
      // A blank token means "keep the existing one" when editing.
      if (root.tokenInput.trim() !== "") saveToken.running = true
      else checkConnection.running = true
    }
  }

  Process {
    id: saveToken
    stdinEnabled: true
    command: [root.atwBin, "config", "set", "sync.token", "-"]
    onStarted: {
      // Piped on stdin so the token never appears in the process list.
      // Kept in the field until the connection check succeeds.
      write(root.tokenInput.trim() + "\n")
    }
    onExited: function(code) {
      if (code !== 0) {
        root.connecting = false
        root.connectError = "Could not save the token"
        return
      }
      checkConnection.running = true
    }
  }

  Process {
    id: checkConnection
    command: [root.atwBin, "sync", "--check"]
    onExited: function(code) {
      if (code === 0) {
        root.noteText = "Saved & connected"
        root.connectError = ""
        root.tokenInput = ""
        root.editingCloud = false
      } else {
        // The config is saved either way; only the connection failed.
        root.noteText = ""
        root.connectError = "Saved, but the connection failed — check the URL and token"
      }
      root.syncOnOverride = true
      setEnabled.running = true
    }
  }

  Process {
    id: setEnabled
    command: [root.atwBin, "config", "set", "sync.enabled", root.syncOn ? "true" : "false"]
    onExited: function(code) {
      if (code !== 0) {
        root.syncOnOverride = null
        root.connecting = false
        root.connectError = "Could not save the sync setting"
        return
      }
      restartDaemon.running = true
    }
  }

  Process {
    id: restartDaemon
    command: ["systemctl", "--user", "restart", "atwd"]
    onExited: function(code) {
      root.connecting = false
      // Only leave the form after a successful connection; on failure keep
      // the values visible so they can be corrected and saved again.
      if (root.connectError === "") root.editingCloud = false
      if (root.hostWidget) root.hostWidget.refresh()
    }
  }

  Process {
    id: syncNow
    command: ["sh", "-c", "systemctl --user kill --signal=USR1 atwd 2>/dev/null || " + root.atwBin + " sync"]
    onExited: function(code) {
      // The daemon writes status.json when SyncDone arrives; the FileView
      // watcher in BarWidget refreshes the panel automatically.
      root.syncingNow = false
      if (root.hostWidget) root.hostWidget.refresh()
    }
  }

  Process {
    id: runPurge
    // Wrapped in sh with an existence check: a missing binary becomes a
    // clean non-zero exit (the panel shows the failure note) instead of a
    // spawn error that leaves the row stuck on "Deleting…".
    // The name reaches the CLI as a separate argv entry — never inside a
    // quoted string — so any app name is safe.
    property string appName: ""
    command: ["sh", "-c",
      "if [ -x \"$HOME/.local/bin/atw\" ]; then " +
      "exec \"$HOME/.local/bin/atw\" delete-app \"$1\"; " +
      "else echo \"atw binary missing\" >&2; exit 127; fi",
      "sh", appName]
    onExited: function(code) {
      root.purgeWatchdog.stop()
      var name = root.deleting
      root.deleting = ""
      if (code === 0) {
        root.deleteNote = "Deleted \u201C" + name + "\u201D — today, history and cloud cleaned"
        // The daemon rewrites status.json right at startup, so the
        // refreshed totals appear the moment the restart completes.
        root.restartAfterPurge.running = true
      } else {
        root.deleteNote = "Delete failed — \u201Catw delete-app\u201D exited " + code
      }
      root.clearDeleteNote.restart()
    }
  }

  Process {
    id: restartAfterPurge
    command: ["systemctl", "--user", "restart", "atwd"]
    onExited: function(code) {
      if (code === 0 && root.hostWidget) root.hostWidget.refresh()
    }
  }

  // Guarded so the widget renders before the bar is injected (the bar-widget
  // contract instantiates this panel bare).
  readonly property color foreground: bar ? bar.barForeground : Color.popups.text
  readonly property color muted: Color.muted
  readonly property color accent: Color.accent
  readonly property color urgent: Color.urgent
  readonly property string contentFontFamily: bar ? bar.fontFamily : Style.font.family

  readonly property string stateLabel: state === "idle" ? "idle" : (state === "locked" ? "locked" : "active")
  readonly property color stateColor: state === "idle" ? muted : (state === "locked" ? urgent : accent)

  KeyboardPanel {
    id: panel
    anchorItem: root.anchorItem
    owner: root.barIdentity
    bar: root.bar
    open: root.opened
    centerOnBar: false
    focusTarget: keyCatcher
    contentWidth: panel.fittedContentWidth(Style.space(430))
    contentHeight: panel.fittedContentHeight(contentColumn.implicitHeight)

    PanelKeyCatcher {
      id: keyCatcher
      anchors.fill: parent
      onCloseRequested: root.close()

      Flickable {
        anchors.fill: parent
        contentWidth: width
        contentHeight: contentColumn.implicitHeight
        clip: true
        boundsBehavior: Flickable.StopAtBounds
        interactive: contentHeight > height

        Column {
          id: contentColumn
          width: parent.width
          spacing: Style.space(14)

          // ---- Header: title + live state chip, centered as one unit.
          //      Wrapped in a full-width Item because Column children must
          //      not carry anchors themselves.
          Item {
            width: parent.width
            height: headerRow.implicitHeight

            Row {
              id: headerRow
              anchors.horizontalCenter: parent.horizontalCenter
              anchors.verticalCenter: parent.verticalCenter
              spacing: Style.space(10)

              Text {
                anchors.verticalCenter: parent.verticalCenter
                text: "Dev Activity Watcher"
                color: root.foreground
                font.family: root.contentFontFamily
                font.pixelSize: Style.font.title
                font.bold: true
              }

              Rectangle {
                width: stateChip.implicitWidth + Style.space(14)
                height: stateChip.implicitHeight + Style.space(6)
                radius: height / 2
                color: Qt.alpha(root.stateColor, 0.14)
                anchors.verticalCenter: parent.verticalCenter

                Text {
                  id: stateChip
                  anchors.centerIn: parent
                  text: root.stateLabel
                  color: root.stateColor
                  font.family: root.contentFontFamily
                  font.pixelSize: Style.font.body
                }
              }
            }
          }

          // ---- Hero: today's total, centered like the clock panel's hero.
          Text {
            width: parent.width
            horizontalAlignment: Text.AlignHCenter
            text: root.hostWidget ? root.hostWidget.formatDuration(root.todaySeconds) + " today" : "—"
            color: root.foreground
            font.family: root.contentFontFamily
            font.pixelSize: Style.font.title
            font.bold: true
          }

          // ---- Today / History section header. History is opt-in: the
          //      default view is today only, and the button is a real
          //      bordered control so it reads as an action.
          Item {
            width: parent.width
            height: Math.max(sectionHeader.implicitHeight, viewToggle.implicitHeight)

            PanelSectionHeader {
              id: sectionHeader
              anchors.left: parent.left
              anchors.verticalCenter: parent.verticalCenter
              text: root.historyMode ? "Last 7 days" : "Today"
              fontSize: Style.font.body
              foreground: root.muted
            }

            Button {
              id: viewToggle
              anchors.right: parent.right
              anchors.verticalCenter: parent.verticalCenter
              text: root.historyMode ? "Today" : "History"
              fontSize: Style.font.body
              foreground: root.foreground
              accent: root.accent
              bordered: true
              onClicked: root.historyMode = !root.historyMode
            }
          }

          // ---- Today, per app (default view)
          Column {
            visible: !root.historyMode
            width: parent.width
            spacing: Style.space(10)

            Repeater {
              model: root.todayList

              delegate: Column {
                id: appRow
                required property var modelData
                readonly property bool hovering: rowHover.containsMouse
                readonly property bool armed: root.confirmDelete === modelData.name
                readonly property bool working: root.deleting === modelData.name
                width: parent.width
                spacing: Style.space(4)

                Item {
                  width: parent.width
                  height: Math.max(rowName.implicitHeight, rowTime.implicitHeight)

                  MouseArea {
                    id: rowHover
                    anchors.fill: parent
                    hoverEnabled: true
                    // Never eats clicks: the row itself has no click action,
                    // and the delete/confirm zones sit above it.
                    acceptedButtons: Qt.NoButton
                  }

                  Text {
                    id: rowName
                    anchors.left: parent.left
                    anchors.verticalCenter: parent.verticalCenter
                    // The fixed right-side gutter always leaves room for the
                    // delete affordance and the confirm buttons, so row text
                    // never jumps when the row arms.
                    width: parent.width - rowTime.implicitWidth - Style.space(46)
                    text: appRow.armed
                        ? "Delete \u201C" + appRow.modelData.name + "\u201D everywhere?"
                        : appRow.modelData.name
                    color: appRow.armed ? root.urgent : root.foreground
                    font.family: root.contentFontFamily
                    font.pixelSize: Style.font.body
                    elide: Text.ElideRight
                  }

                  Text {
                    id: rowTime
                    visible: !appRow.armed && !appRow.working
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    text: root.formatDuration(appRow.modelData.seconds)
                    color: root.muted
                    font.family: root.contentFontFamily
                    font.pixelSize: Style.font.body
                  }

                  // The delete affordance: a pill-sized ✕ that lights up over
                  // the row. It sits left of the duration on the right side,
                  // and the "everywhere?" question is the only thing the user
                  // has to read — no jargon on the confirm step.
                  Item {
                    id: deleteZone
                    visible: !appRow.armed && !appRow.working
                    opacity: appRow.hovering ? 1 : 0
                    Behavior on opacity {
                      NumberAnimation { duration: 120 }
                    }
                    width: Style.space(24)
                    height: parent.height
                    anchors.right: parent.right
                    anchors.rightMargin: rowTime.implicitWidth + Style.space(12)
                    anchors.verticalCenter: parent.verticalCenter
                    property bool zoneHover: zoneMouse.containsMouse

                    Rectangle {
                      anchors.fill: parent
                      radius: height / 2
                      color: Qt.alpha(root.urgent, deleteZone.zoneHover ? 0.22 : 0.10)
                    }

                    Text {
                      anchors.centerIn: parent
                      text: "\u2715"
                      color: root.urgent
                      font.family: root.contentFontFamily
                      font.pixelSize: Style.font.body * 1.1
                      font.bold: true
                    }

                    MouseArea {
                      id: zoneMouse
                      anchors.fill: parent
                      hoverEnabled: true
                      cursorShape: Qt.PointingHandCursor
                      onClicked: {
                        // The row asks once and STAYS asked until the user
                        // picks Delete or Cancel — no dropping off.
                        root.confirmDelete = appRow.modelData.name
                      }
                    }
                  }

                  Text {
                    id: confirmButton
                    visible: appRow.armed && !appRow.working
                    anchors.right: cancelButton.left
                    anchors.rightMargin: Style.space(14)
                    anchors.verticalCenter: parent.verticalCenter
                    text: "Delete"
                    color: root.urgent
                    font.family: root.contentFontFamily
                    font.pixelSize: Style.font.body
                    font.bold: true

                    MouseArea {
                      anchors.fill: parent
                      cursorShape: Qt.PointingHandCursor
                      onClicked: root.deleteApp(appRow.modelData.name)
                    }
                  }

                  Text {
                    id: cancelButton
                    visible: appRow.armed && !appRow.working
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    text: "Cancel"
                    color: root.muted
                    font.family: root.contentFontFamily
                    font.pixelSize: Style.font.body

                    MouseArea {
                      anchors.fill: parent
                      cursorShape: Qt.PointingHandCursor
                      onClicked: root.confirmDelete = ""
                    }
                  }

                  Text {
                    id: workingLabel
                    visible: appRow.working
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    text: "Deleting…"
                    color: root.muted
                    font.family: root.contentFontFamily
                    font.pixelSize: Style.font.body
                  }
                }

                Rectangle {
                  width: parent.width * Math.min(1, modelData.seconds / root.todayMax)
                  height: Style.space(4)
                  radius: height / 2
                  color: root.accent
                  opacity: 0.6
                  visible: !appRow.working
                }
              }
            }

            Text {
              visible: root.deleteNote !== ""
              width: parent.width
              text: root.deleteNote
              color: root.deleteNote.indexOf("failed") >= 0
                      || root.deleteNote.indexOf("timed out") >= 0
                   ? root.urgent : root.foreground
              font.family: root.contentFontFamily
              font.pixelSize: Style.font.body
              opacity: 0.85
              wrapMode: Text.WordWrap
            }

            Text {
              visible: root.todayList.length === 0
              width: parent.width
              horizontalAlignment: Text.AlignHCenter
              text: "No activity recorded yet."
              color: root.muted
              font.family: root.contentFontFamily
              font.pixelSize: Style.font.body
            }
          }

          // ---- History, day by day (only when the user asked for it)
          Column {
            visible: root.historyMode
            width: parent.width
            spacing: Style.space(12)

            Repeater {
              model: root.historyList

              delegate: Column {
                required property var modelData
                width: parent.width
                spacing: Style.space(3)

                Item {
                  width: parent.width
                  height: Math.max(dayName.implicitHeight, dayTime.implicitHeight)

                  Text {
                    id: dayName
                    anchors.left: parent.left
                    anchors.verticalCenter: parent.verticalCenter
                    width: parent.width - dayTime.implicitWidth - Style.space(10)
                    text: root.dayLabel(modelData.date)
                    color: root.foreground
                    font.family: root.contentFontFamily
                    font.pixelSize: Style.font.body
                  }

                  Text {
                    id: dayTime
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    text: root.formatDuration(modelData.total_seconds)
                    color: root.foreground
                    font.family: root.contentFontFamily
                    font.pixelSize: Style.font.body
                  }
                }

                Text {
                  width: parent.width
                  text: root.topApps(modelData.apps)
                  color: root.muted
                  font.family: root.contentFontFamily
                  font.pixelSize: Style.font.body
                  elide: Text.ElideRight
                  visible: text !== ""
                }
              }
            }

            Text {
              visible: root.historyList.length === 0
              width: parent.width
              horizontalAlignment: Text.AlignHCenter
              text: "No history yet."
              color: root.muted
              font.family: root.contentFontFamily
              font.pixelSize: Style.font.body
            }
          }

          // ---- Widget settings
          PanelSectionHeader {
            text: "Widget"
            fontSize: Style.font.body
            foreground: root.muted
          }

          Column {
            width: parent.width
            spacing: Style.space(8)

            // Full row height so the taller switch does not overflow the
            // row box and crowd the next element.
            Item {
              width: parent.width
              height: Math.max(dimLabel.implicitHeight, idleToggle.implicitHeight)

              Text {
                id: dimLabel
                anchors.left: parent.left
                anchors.verticalCenter: parent.verticalCenter
                text: "Dim while idle"
                color: root.foreground
                font.family: root.contentFontFamily
                font.pixelSize: Style.font.body
              }

              ToggleSwitch {
                id: idleToggle
                anchors.right: parent.right
                anchors.verticalCenter: parent.verticalCenter
                checked: root.hostWidget ? root.hostWidget.showIdleDot : true
                foreground: root.foreground
                accent: root.accent
                onToggled: {
                  if (root.hostWidget)
                    root.hostWidget.setSetting("showIdleDot", !root.hostWidget.showIdleDot)
                }
              }
            }
          }

          // ---- Cloud sync: toggle + first-time paste-in setup
          Item {
            width: parent.width
            height: Math.max(cloudHeader.implicitHeight, cloudToggle.implicitHeight)

            PanelSectionHeader {
              id: cloudHeader
              anchors.left: parent.left
              anchors.verticalCenter: parent.verticalCenter
              text: "Cloud sync"
              fontSize: Style.font.body
              foreground: root.muted
            }

            ToggleSwitch {
              id: cloudToggle
              anchors.right: parent.right
              anchors.verticalCenter: parent.verticalCenter
              checked: root.syncOn
              foreground: root.foreground
              accent: root.accent
              onToggled: {
                root.syncOnOverride = !root.syncOn
                setEnabled.running = true
              }
            }
          }

          Column {
            width: parent.width
            spacing: Style.space(8)

            // Configured summary: what is saved, plus a way to change it.
            Item {
              visible: root.syncConfigured && !root.showCloudForm
              width: parent.width
              height: Math.max(dbLabel.implicitHeight, changeButton.implicitHeight)

              Text {
                id: dbLabel
                anchors.left: parent.left
                anchors.verticalCenter: parent.verticalCenter
                width: parent.width - changeButton.implicitWidth - Style.space(8)
                text: root.databaseLabel() !== "" ? root.databaseLabel() : "Database configured"
                color: root.muted
                font.family: root.contentFontFamily
                font.pixelSize: Math.max(10, Style.font.body - 1)
                wrapMode: Text.WrapAnywhere
                maximumLineCount: 2
                elide: Text.ElideRight
              }

              Button {
                id: changeButton
                anchors.right: parent.right
                anchors.top: parent.top
                text: "Change"
                foreground: root.foreground
                accent: root.accent
                bordered: true
                fontSize: Math.max(10, Style.font.body - 1)
                horizontalPadding: Style.space(6)
                verticalPadding: Style.space(2)
                onClicked: root.openCloudEditor()
              }
            }

            // First time / edit: paste the database URL + token here.
            TextField {
              visible: root.showCloudForm
              width: parent.width
              placeholderText: "Database URL (libsql://… or https://…)"
              text: root.urlInput
              onTextChanged: root.urlInput = text
              foreground: root.foreground
              accent: root.accent
              font.pixelSize: Style.font.body
            }

            TextField {
              visible: root.showCloudForm
              width: parent.width
              password: true
              placeholderText: root.syncConfigured
                ? "New token (leave blank to keep)"
                : "Database token"
              text: root.tokenInput
              onTextChanged: root.tokenInput = text
              foreground: root.foreground
              accent: root.accent
              font.pixelSize: Style.font.body
            }

            Item {
              visible: root.showCloudForm
              width: parent.width
              height: Math.max(saveButton.implicitHeight, cancelButton.implicitHeight)

              Button {
                id: saveButton
                anchors.left: parent.left
                anchors.verticalCenter: parent.verticalCenter
                text: root.connecting ? "Saving…" : "Save"
                enabled: !root.connecting
                foreground: root.foreground
                accent: root.accent
                bordered: true
                fontSize: Style.font.body
                onClicked: root.connect()
              }

              Button {
                id: cancelButton
                anchors.left: saveButton.right
                anchors.leftMargin: Style.space(10)
                anchors.verticalCenter: parent.verticalCenter
                visible: root.syncConfigured
                text: "Cancel"
                foreground: root.muted
                accent: root.accent
                bordered: false
                fontSize: Style.font.body
                onClicked: root.cancelCloudEditor()
              }
            }

            // Configured: last-sync line + manual sync.
            Item {
              visible: root.syncConfigured && root.syncOn && !root.showCloudForm
              width: parent.width
              height: Math.max(syncLineText.implicitHeight, syncNowButton.implicitHeight)

              Text {
                id: syncLineText
                anchors.left: parent.left
                anchors.verticalCenter: parent.verticalCenter
                width: parent.width - syncNowButton.implicitWidth - Style.space(10)
                text: root.syncLine()
                color: root.muted
                font.family: root.contentFontFamily
                font.pixelSize: Math.max(10, Style.font.body - 1)
                elide: Text.ElideRight
              }

              Button {
                id: syncNowButton
                anchors.right: parent.right
                anchors.verticalCenter: parent.verticalCenter
                text: root.syncingNow ? "Syncing…" : "Sync now"
                enabled: !root.syncingNow
                foreground: root.foreground
                accent: root.accent
                bordered: true
                fontSize: Math.max(10, Style.font.body - 1)
                horizontalPadding: Style.space(6)
                verticalPadding: Style.space(2)
                onClicked: {
                  root.syncingNow = true
                  syncNow.running = true
                }
              }
            }

            Text {
              visible: root.connectError !== ""
              width: parent.width
              text: root.connectError
              color: root.urgent
              font.family: root.contentFontFamily
              font.pixelSize: Style.font.body
              wrapMode: Text.WordWrap
            }

            Text {
              visible: root.noteText !== ""
              width: parent.width
              text: root.noteText
              color: root.accent
              font.family: root.contentFontFamily
              font.pixelSize: Style.font.body
              wrapMode: Text.WordWrap
            }

            Text {
              width: parent.width
              text: root.syncOn
                ? (root.syncConfigured
                    ? "Apps and durations only — never titles or paths."
                    : "App names and durations only — never titles or paths.")
                : "Off — everything stays on this machine."
              color: root.muted
              font.family: root.contentFontFamily
              font.pixelSize: Math.max(10, Style.font.body - 1)
              wrapMode: Text.WordWrap
            }
          }
        }
      }
    }
  }
}
