import QtCore
import QtQuick
import QtQuick.Controls
import QtQuick.Dialogs
import QtQuick.Layouts
import net.batkin.batscribe

// The dictation editor: a plain-text TextArea bridged to the Rust editor model by TextHelper.
Pane {
    id: pane

    required property Backend backend
    property alias editor: editor
    property alias helper: helper
    // True while QML moves the caret itself, so the move doesn't detach following.
    property bool programmaticCaret: false
    readonly property bool recordingHere: backend.recordingState !== "idle"
        && backend.recordingDocumentId === backend.currentDocumentId
    readonly property bool recordingElsewhere: backend.recordingState !== "idle"
        && backend.recordingDocumentId !== backend.currentDocumentId

    signal showSources(var ids)
    /// A message for the notice bar ("info", "warning" or "error").
    signal message(string level, string text)

    padding: 0

    function copyAll() {
        helper.copyToClipboard(helper.plainText())
        pane.message("info", qsTr("Copied the document to the clipboard."))
    }
    function save() {
        if (backend.currentFilePath)
            report(backend.saveToFile(backend.currentFilePath))
        else
            saveAs()
    }
    function saveAs() {
        const path = backend.currentFilePath
        if (path) {
            saveDialog.currentFolder = "file://" + path.substring(0, path.lastIndexOf("/"))
            saveDialog.selectedFile = "file://" + path
        } else {
            const folder = StandardPaths.writableLocation(StandardPaths.DocumentsLocation)
            saveDialog.currentFolder = folder
            saveDialog.selectedFile = folder + "/" + (backend.currentTitle || "Untitled") + ".txt"
        }
        saveDialog.open()
    }
    function report(error) {
        if (error)
            pane.message("error", error)
    }

    function startOrStop() {
        if (backend.recordingState === "idle")
            backend.startRecording(editor.cursorPosition)
        else if (backend.recordingState === "recording")
            backend.stopRecording()
    }

    ColumnLayout {
        anchors.fill: parent
        spacing: 0

        ToolBar {
            Layout.fillWidth: true
            RowLayout {
                anchors.fill: parent
                spacing: 6

                Button {
                    id: startStop
                    objectName: "startStop"
                    text: backend.recordingState === "idle" ? qsTr("Start")
                        : backend.recordingState === "recording" ? qsTr("Stop") : qsTr("Finishing…")
                    enabled: backend.ready && backend.recordingState !== "finishing"
                    highlighted: backend.recordingState === "recording"
                    // Keeps the editor's caret and selection.
                    focusPolicy: Qt.NoFocus
                    onClicked: pane.startOrStop()
                    ToolTip.visible: hovered
                    ToolTip.text: qsTr("Ctrl+Shift+Space")
                }
                ProgressBar {
                    objectName: "level"
                    visible: backend.recordingState === "recording"
                    from: 0
                    to: 1
                    value: backend.level
                    Layout.preferredWidth: 90
                }
                Label {
                    text: backend.statusText ? backend.statusText
                        : backend.recordingState === "recording" ? qsTr("Recording")
                        : backend.recordingState === "finishing" ? qsTr("Finishing") : ""
                    color: backend.recordingState === "recording" ? "#c0392b" : palette.windowText
                    elide: Text.ElideRight
                    Layout.maximumWidth: 260
                }
                Button {
                    text: qsTr("Follow dictation")
                    visible: backend.recordingState === "recording" && !backend.following
                    focusPolicy: Qt.NoFocus
                    onClicked: backend.followDictation()
                }
                Item { Layout.fillWidth: true }
                ToolButton {
                    text: qsTr("Sources")
                    focusPolicy: Qt.NoFocus
                    onClicked: {
                        const a = editor.selectionStart, b = editor.selectionEnd
                        pane.showSources(JSON.parse(backend.recordingsForSelection(
                            a === b ? editor.cursorPosition : a, a === b ? editor.cursorPosition : b)))
                    }
                    ToolTip.visible: hovered
                    ToolTip.text: qsTr("Show the recording that produced the selected text")
                }
                ToolButton {
                    text: qsTr("Copy")
                    focusPolicy: Qt.NoFocus
                    onClicked: pane.copyAll()
                    ToolTip.visible: hovered
                    ToolTip.text: qsTr("Copy the whole document to the clipboard")
                }
                ToolButton {
                    text: qsTr("Save")
                    focusPolicy: Qt.NoFocus
                    onClicked: pane.save()
                    ToolTip.visible: hovered
                    ToolTip.text: backend.currentFilePath
                        ? qsTr("Save to %1 (Ctrl+S)").arg(backend.currentFilePath)
                        : qsTr("Save to a text file (Ctrl+S)")
                }
                ToolButton {
                    text: qsTr("Save As…")
                    focusPolicy: Qt.NoFocus
                    onClicked: pane.saveAs()
                    ToolTip.visible: hovered
                    ToolTip.text: qsTr("Save to a different file (Ctrl+Shift+S)")
                }
                ToolButton {
                    text: qsTr("Undo")
                    enabled: backend.canUndo
                    focusPolicy: Qt.NoFocus
                    onClicked: backend.undo()
                }
                ToolButton {
                    text: qsTr("Redo")
                    enabled: backend.canRedo
                    focusPolicy: Qt.NoFocus
                    onClicked: backend.redo()
                }
            }
        }

        // The recording stays bound to its document when another one is displayed.
        Pane {
            visible: pane.recordingElsewhere
            Layout.fillWidth: true
            padding: 6
            background: Rectangle { color: "#fdecea" }
            RowLayout {
                anchors.fill: parent
                Label {
                    text: qsTr("Dictating into “%1”").arg(backend.recordingTitle)
                    color: "#222"
                    Layout.fillWidth: true
                    elide: Text.ElideRight
                }
                Button {
                    text: qsTr("Jump back")
                    focusPolicy: Qt.NoFocus
                    onClicked: backend.followDictation()
                }
                Button {
                    text: qsTr("Stop")
                    enabled: backend.recordingState === "recording"
                    focusPolicy: Qt.NoFocus
                    onClicked: backend.stopRecording()
                }
            }
        }

        ScrollView {
            id: scroll
            Layout.fillWidth: true
            Layout.fillHeight: true

            TextArea {
                id: editor
                objectName: "editor"
                textFormat: TextEdit.PlainText
                wrapMode: TextEdit.Wrap
                selectByMouse: true
                persistentSelection: true
                font.pointSize: backend.fontSize
                focus: true
                placeholderText: qsTr("Type, or press Start to dictate.")
                leftPadding: 12
                rightPadding: 12
                topPadding: 10

                Keys.onPressed: (event) => {
                    if (event.matches(StandardKey.Undo)) {
                        backend.undo()
                        event.accepted = true
                        return
                    }
                    if (event.matches(StandardKey.Redo)) {
                        backend.redo()
                        event.accepted = true
                        return
                    }
                    let kind = ""
                    if (event.key === Qt.Key_Backspace)
                        kind = "backspace"
                    else if (event.key === Qt.Key_Delete)
                        kind = "delete"
                    else if (event.matches(StandardKey.Paste) || event.matches(StandardKey.Cut)
                             || event.key === Qt.Key_Return || event.key === Qt.Key_Enter
                             || (event.text.length > 0 && !(event.modifiers & Qt.ControlModifier)))
                        kind = "insert"
                    if (kind && backend.editBlocked(kind, editor.cursorPosition,
                                                    editor.selectionStart, editor.selectionEnd))
                        event.accepted = true
                }

                onCursorPositionChanged: {
                    if (!helper.applying && !pane.programmaticCaret)
                        backend.userMovedCaret(cursorPosition)
                }

                // The voice caret: where new speech arrives.
                Rectangle {
                    id: voiceCaret
                    objectName: "voiceCaret"
                    readonly property rect r: {
                        // Re-evaluate when the text or layout changes, not only the position.
                        editor.length; editor.contentHeight; editor.width
                        return backend.voiceCaret >= 0
                            ? editor.positionToRectangle(Math.min(backend.voiceCaret, editor.length))
                            : Qt.rect(0, 0, 0, 0)
                    }
                    visible: backend.voiceCaret >= 0
                    x: r.x
                    y: r.y
                    width: 3
                    height: Math.max(r.height, 12)
                    radius: 1
                    color: "#d35400"
                    SequentialAnimation on opacity {
                        running: voiceCaret.visible
                        loops: Animation.Infinite
                        NumberAnimation { to: 0.35; duration: 600 }
                        NumberAnimation { to: 1; duration: 600 }
                    }
                }
            }
        }
    }

    Shortcut {
        sequence: "Ctrl+Shift+Space"
        onActivated: pane.startOrStop()
    }
    Shortcut {
        sequences: [StandardKey.Save]
        onActivated: pane.save()
    }
    Shortcut {
        sequences: [StandardKey.SaveAs]
        onActivated: pane.saveAs()
    }

    FileDialog {
        id: saveDialog
        title: qsTr("Save document")
        fileMode: FileDialog.SaveFile
        nameFilters: [qsTr("Text files (*.txt)"), qsTr("Markdown (*.md)"), qsTr("All files (*)")]
        defaultSuffix: "txt"
        onAccepted: pane.report(backend.saveToFile(selectedFile.toString()))
    }

    // Scrolling away while following stops following; "Follow dictation" resumes.
    Connections {
        target: scroll.contentItem
        ignoreUnknownSignals: true
        function onMovementStarted() {
            if (backend.following)
                backend.userMovedCaret(-2)
        }
    }

    // Saves after an idle period, and retries failed saves periodically.
    Timer {
        id: saveTimer
        interval: backend.saveDebounceMs
        onTriggered: backend.flushEdits()
    }
    Timer {
        // While dictating, save every few seconds so an interruption loses little.
        interval: backend.recordingState === "idle" ? 15000 : 3000
        running: true
        repeat: true
        onTriggered: backend.flushEdits()
    }

    TextHelper {
        id: helper
        document: editor.textDocument
        revisableColor: Qt.alpha(pane.palette.highlight, 0.14)
        provisionalColor: Qt.alpha(pane.palette.highlight, 0.32)
        provisionalTextColor: Qt.tint(pane.palette.text, Qt.alpha(pane.palette.window, 0.45))
        onUserEdited: (position, removed, inserted, newLength) => {
            if (!pane.backend.userEdited(position, removed, inserted, newLength))
                pane.backend.resync(helper.plainText())
            saveTimer.restart()
        }
    }

    Connections {
        target: pane.backend
        function onApplyEdits(json) { helper.applyEdits(json) }
        function onLoadText(text) {
            helper.setText(text)
            pane.programmaticCaret = true
            editor.cursorPosition = 0
            pane.programmaticCaret = false
        }
        function onHighlightsChanged(json) { helper.setHighlights(json) }
        function onPlaceCaret(position) {
            if (position > editor.length)
                return
            pane.programmaticCaret = true
            editor.cursorPosition = position
            pane.programmaticCaret = false
        }
    }
}
