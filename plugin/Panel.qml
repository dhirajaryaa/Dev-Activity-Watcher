import QtQuick
import Quickshell
import qs.Commons
import qs.Ui

// Dev Activity Watcher — expanded view: today's per-app breakdown, the
// weekly top apps, and the privacy facts (local-only storage, 7-day
// retention, where to configure).
//
// Alignment conventions (matching the clock panel): the header and the hero
// total are centered within their own full-width rows; section labels head
// their lists from the left; list rows keep the activity-monitor shape —
// name left, time right. Nothing is centered by centering the window.
//
// BarWidget.qml owns the bar item and hands this panel the button to anchor
// against; the status data lives on the host widget so bar and panel share
// one FileView.
Panel {
  id: root
  moduleName: "dhirajarya.atw"
  ipcTarget: "dhirajarya.atw"
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

          // ---- Today, per app
          PanelSectionHeader {
            text: "Today"
            fontSize: Style.font.body
            foreground: root.muted
          }

          Column {
            width: parent.width
            spacing: Style.space(10)

            Repeater {
              model: root.todayList

              delegate: Column {
                required property var modelData
                width: parent.width
                spacing: Style.space(4)

                Item {
                  width: parent.width
                  height: Math.max(rowName.implicitHeight, rowTime.implicitHeight)

                  Text {
                    id: rowName
                    anchors.left: parent.left
                    anchors.verticalCenter: parent.verticalCenter
                    width: parent.width - rowTime.implicitWidth - Style.space(10)
                    text: modelData.name
                    color: root.foreground
                    font.family: root.contentFontFamily
                    font.pixelSize: Style.font.body
                    elide: Text.ElideRight
                  }

                  Text {
                    id: rowTime
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    text: root.formatDuration(modelData.seconds)
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
                }
              }
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

          // ---- Week, top apps
          PanelSectionHeader {
            text: "Last 7 days"
            fontSize: Style.font.body
            foreground: root.muted
          }

          Column {
            width: parent.width
            spacing: Style.space(6)

            Repeater {
              model: root.weekList

              delegate: Item {
                required property var modelData
                width: parent.width
                height: Math.max(weekName.implicitHeight, weekTime.implicitHeight)

                Text {
                  id: weekName
                  anchors.left: parent.left
                  anchors.verticalCenter: parent.verticalCenter
                  width: parent.width - weekTime.implicitWidth - Style.space(10)
                  text: modelData.name
                  color: root.foreground
                  font.family: root.contentFontFamily
                  font.pixelSize: Style.font.body
                  elide: Text.ElideRight
                }

                Text {
                  id: weekTime
                  anchors.right: parent.right
                  anchors.verticalCenter: parent.verticalCenter
                  text: root.formatDuration(modelData.seconds)
                  color: root.muted
                  font.family: root.contentFontFamily
                  font.pixelSize: Style.font.body
                }
              }
            }

            Text {
              visible: root.weekList.length === 0
              width: parent.width
              horizontalAlignment: Text.AlignHCenter
              text: "Nothing recorded in the last 7 days."
              color: root.muted
              font.family: root.contentFontFamily
              font.pixelSize: Style.font.body
            }
          }

          // ---- Settings (widget display options)
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

            Text {
              width: parent.width
              text: "Tracking behaviour (idle timeout, tracked apps, retention) lives in ~/.config/atw/config.toml — restart atwd after editing: systemctl --user restart atwd"
              color: root.muted
              font.family: root.contentFontFamily
              font.pixelSize: Style.font.body
              wrapMode: Text.WordWrap
            }
          }

          // ---- Privacy facts
          PanelSectionHeader {
            text: "Privacy"
            fontSize: Style.font.body
            foreground: root.muted
          }

          Text {
            width: parent.width
            text: "Everything stays on this machine. Only developer apps — editors, AI agents, browsers, terminals — are tracked. Activity is stored in ~/.local/share/atw/activity.db and nothing is ever sent anywhere — no network, no telemetry. Records older than 7 days are deleted automatically (atw cleanup forces it now)."
            color: root.muted
            font.family: root.contentFontFamily
            font.pixelSize: Style.font.body
            wrapMode: Text.WordWrap
          }
        }
      }
    }
  }
}
