import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import net.batkin.batscribe

Pane {
    id: pane

    required property Backend backend
    readonly property var documents: {
        try {
            return JSON.parse(backend.documentsJson)
        } catch (e) {
            return []
        }
    }
    property string renaming: ""
    property string confirmingDelete: ""

    padding: 0

    ColumnLayout {
        anchors.fill: parent
        spacing: 0

        RowLayout {
            Layout.fillWidth: true
            Layout.margins: 6
            Label {
                text: qsTr("Documents")
                font.bold: true
                Layout.fillWidth: true
            }
            Button {
                text: qsTr("New")
                focusPolicy: Qt.NoFocus
                onClicked: backend.newDocument()
            }
        }

        ListView {
            id: list
            objectName: "documentList"
            Layout.fillWidth: true
            Layout.fillHeight: true
            clip: true
            model: pane.documents
            ScrollBar.vertical: ScrollBar {}

            delegate: ItemDelegate {
                id: item
                required property var modelData
                width: ListView.view.width
                highlighted: modelData.id === backend.currentDocumentId
                onClicked: backend.openDocument(modelData.id)

                contentItem: ColumnLayout {
                    spacing: 2
                    RowLayout {
                        visible: pane.renaming !== item.modelData.id
                        Label {
                            text: item.modelData.title
                            font.bold: true
                            elide: Text.ElideRight
                            Layout.fillWidth: true
                        }
                        Label {
                            visible: item.modelData.id === backend.recordingDocumentId
                            text: "●"
                            color: "#c0392b"
                            ToolTip.visible: hovered
                            ToolTip.text: qsTr("Recording")
                        }
                    }
                    TextField {
                        id: titleField
                        visible: pane.renaming === item.modelData.id
                        text: item.modelData.title
                        Layout.fillWidth: true
                        onVisibleChanged: if (visible) { forceActiveFocus(); selectAll() }
                        onAccepted: {
                            backend.renameDocument(item.modelData.id, text)
                            pane.renaming = ""
                        }
                        Keys.onEscapePressed: pane.renaming = ""
                    }
                    Label {
                        text: item.modelData.preview || qsTr("Empty")
                        opacity: 0.7
                        elide: Text.ElideRight
                        Layout.fillWidth: true
                    }
                    Label {
                        text: new Date(item.modelData.updatedAt).toLocaleString(Qt.locale(), Locale.ShortFormat)
                        opacity: 0.55
                        font.pointSize: 8
                    }
                    RowLayout {
                        visible: item.highlighted
                        spacing: 2
                        Button {
                            text: qsTr("Rename")
                            flat: true
                            focusPolicy: Qt.NoFocus
                            onClicked: pane.renaming = item.modelData.id
                        }
                        Button {
                            text: pane.confirmingDelete === item.modelData.id ? qsTr("Confirm delete") : qsTr("Delete")
                            flat: true
                            focusPolicy: Qt.NoFocus
                            enabled: item.modelData.id !== backend.recordingDocumentId
                            onClicked: {
                                if (pane.confirmingDelete === item.modelData.id) {
                                    pane.confirmingDelete = ""
                                    backend.deleteDocument(item.modelData.id)
                                } else {
                                    pane.confirmingDelete = item.modelData.id
                                    confirmReset.restart()
                                }
                            }
                        }
                    }
                    Label {
                        visible: pane.confirmingDelete === item.modelData.id
                        text: qsTr("Deletes the document and its history. Saved audio files are kept.")
                        wrapMode: Text.Wrap
                        font.pointSize: 8
                        Layout.fillWidth: true
                    }
                }
            }
        }
    }

    Timer {
        id: confirmReset
        interval: 5000
        onTriggered: pane.confirmingDelete = ""
    }
}
