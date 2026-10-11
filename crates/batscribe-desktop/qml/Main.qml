import QtCore
import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import net.batkin.batscribe

ApplicationWindow {
    id: window

    width: 1280
    height: 780
    visible: true
    title: backend.currentTitle ? backend.currentTitle + " — Batscribe" : "Batscribe"

    Backend { id: backend }

    property alias editorPane: editorPane
    property alias detailsPane: detailsPane
    property alias documentsPane: documentsPane
    property alias preferences: preferences
    property alias splitView: split

    // Pane sizes and visibility, kept between launches.
    Settings {
        id: layout
        category: "layout"
        property var splitState
        property bool documentsVisible: true
        property bool detailsVisible: true
    }

    Component.onCompleted: {
        if (layout.splitState)
            split.restoreState(layout.splitState)
        documentsPane.visible = layout.documentsVisible
        detailsPane.visible = layout.detailsVisible
        backend.initialize()
    }
    // Closing waits for a recording to finish and for saves to be confirmed; see exitDialog.
    property bool quitting: false
    property alias exitDialog: exitDialog
    onClosing: (close) => {
        if (!quitting && !backend.prepareToQuit()) {
            close.accepted = false
            exitDialog.open()
            return
        }
        layout.splitState = split.saveState()
        layout.documentsVisible = documentsPane.visible
        layout.detailsVisible = detailsPane.visible
        backend.shutdown()
    }
    function quitNow() {
        quitting = true
        exitDialog.close()
        window.close()
    }

    Dialog {
        id: exitDialog
        objectName: "exitDialog"
        title: qsTr("Finishing before closing")
        modal: true
        closePolicy: Popup.NoAutoClose
        anchors.centerIn: Overlay.overlay
        width: Math.min(460, window.width - 40)

        readonly property bool finishing: backend.recordingState !== "idle"
        readonly property bool ready: !finishing && backend.unsavedCount === 0
        // Close through the normal path, which checks again (prepareToQuit) instead of forcing.
        onReadyChanged: if (ready && visible) window.close()

        ColumnLayout {
            anchors.fill: parent
            spacing: 10
            RowLayout {
                spacing: 10
                BusyIndicator {
                    running: exitDialog.visible && !backend.saveError
                    implicitWidth: 32
                    implicitHeight: 32
                }
                Label {
                    Layout.fillWidth: true
                    wrapMode: Text.WordWrap
                    text: exitDialog.finishing
                          ? qsTr("Finishing dictation: waiting for the last recognized words and corrections. This takes a few seconds at most.")
                          : qsTr("Saving changes…")
                }
            }
            Label {
                visible: backend.saveError.length > 0
                Layout.fillWidth: true
                wrapMode: Text.WordWrap
                color: "#c0392b"
                text: backend.saveError
            }
            Label {
                Layout.fillWidth: true
                wrapMode: Text.WordWrap
                opacity: 0.75
                text: exitDialog.finishing
                      ? qsTr("Quitting now keeps the text you see, but the last words or corrections may be missing.")
                      : qsTr("Quitting now loses changes that haven't been saved.")
            }
            RowLayout {
                Layout.alignment: Qt.AlignRight
                Button {
                    text: qsTr("Keep Batscribe Open")
                    onClicked: exitDialog.close()
                }
                Button {
                    objectName: "quitNow"
                    text: qsTr("Quit Now")
                    onClicked: window.quitNow()
                }
            }
        }

        // Retry saves while waiting (they may have failed).
        Timer {
            interval: 2000
            repeat: true
            running: exitDialog.visible
            onTriggered: backend.flushEdits()
        }
    }

    header: ToolBar {
        RowLayout {
            anchors.fill: parent
            spacing: 4
            ToolButton {
                text: documentsPane.visible ? qsTr("Hide documents") : qsTr("Documents")
                focusPolicy: Qt.NoFocus
                onClicked: documentsPane.visible = !documentsPane.visible
            }
            Label {
                text: backend.currentTitle
                font.bold: true
                elide: Text.ElideRight
                Layout.fillWidth: true
                horizontalAlignment: Text.AlignHCenter
            }
            ToolButton {
                text: qsTr("Preferences")
                focusPolicy: Qt.NoFocus
                onClicked: preferences.open()
            }
            ToolButton {
                text: detailsPane.visible ? qsTr("Hide details") : qsTr("Details")
                focusPolicy: Qt.NoFocus
                onClicked: detailsPane.visible = !detailsPane.visible
            }
        }
    }

    SplitView {
        id: split
        anchors.fill: parent
        orientation: Qt.Horizontal

        // A visible grip with a wider hit area and a resize cursor, independent of the style
        // (the KDE style draws a one-pixel line that is hard to find and grab).
        handle: Rectangle {
            id: grip
            implicitWidth: 6
            color: SplitHandle.pressed ? palette.highlight
                 : SplitHandle.hovered ? Qt.alpha(palette.highlight, 0.5) : palette.midlight
            containmentMask: Item {
                x: -4
                width: grip.width + 8
                height: grip.height
            }
            HoverHandler { cursorShape: Qt.SplitHCursor }
        }

        DocumentsPane {
            id: documentsPane
            backend: backend
            SplitView.preferredWidth: 230
            SplitView.minimumWidth: 160
        }

        EditorPane {
            id: editorPane
            backend: backend
            SplitView.fillWidth: true
            SplitView.minimumWidth: 320
            onMessage: (level, text) => noticeBar.show(level, text)
            onShowSources: (ids) => {
                if (ids.length === 0) {
                    noticeBar.show("info", qsTr("The selection wasn't dictated."))
                    return
                }
                detailsPane.visible = true
                detailsPane.openRecording(ids[0])
            }
        }

        DetailsPane {
            id: detailsPane
            backend: backend
            SplitView.preferredWidth: 420
            SplitView.minimumWidth: 260
        }
    }

    footer: NoticeBar { id: noticeBar }

    Connections {
        target: backend
        function onNotice(level, message) { noticeBar.show(level, message) }
    }

    PreferencesDialog {
        id: preferences
        backend: backend
        anchors.centerIn: Overlay.overlay
    }

    // Development aid (debug builds only): `--qml-script=/path/file.qml` loads a QML file that can
    // drive this window, e.g. tests/ui/editor.qml.
    Loader {
        source: {
            if (!backend.developerBuild())
                return ""
            const arg = Qt.application.arguments.find(a => a.startsWith("--qml-script="))
            return arg ? "file://" + arg.substring("--qml-script=".length) : ""
        }
    }
}
