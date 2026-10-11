import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// One-line notices at the bottom of the window. Errors stay until dismissed.
Pane {
    id: bar

    property string level: "info"
    property string message: ""

    visible: message.length > 0
    padding: 6

    function show(level, message) {
        bar.level = level
        bar.message = message
        if (level === "error")
            hideTimer.stop()
        else
            hideTimer.restart()
    }

    background: Rectangle {
        color: bar.level === "error" ? "#f8d7da" : bar.level === "warning" ? "#fff3cd" : palette.alternateBase
    }

    RowLayout {
        anchors.fill: parent
        Label {
            objectName: "noticeText"
            text: bar.message
            wrapMode: Text.Wrap
            color: bar.level === "error" || bar.level === "warning" ? "#222" : palette.windowText
            Layout.fillWidth: true
        }
        ToolButton {
            text: "✕"
            focusPolicy: Qt.NoFocus
            onClicked: bar.message = ""
        }
    }

    Timer {
        id: hideTimer
        interval: 8000
        onTriggered: bar.message = ""
    }
}
