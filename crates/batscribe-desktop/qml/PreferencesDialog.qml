import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import net.batkin.batscribe

// Preferences generated from the Rust schema: every setting has a type, unit, help, default,
// validation and reset. Pipeline settings apply to the next recording.
Dialog {
    id: dialog

    required property Backend backend
    readonly property var prefs: {
        try {
            return JSON.parse(backend.prefsJson)
        } catch (e) {
            return {}
        }
    }
    readonly property var keys: {
        try {
            return JSON.parse(backend.keysJson)
        } catch (e) {
            return {}
        }
    }
    property string group: "API keys"
    property bool showAdvanced: false
    property var errors: ({})

    title: qsTr("Preferences")
    modal: true
    width: Math.min(820, parent ? parent.width - 40 : 820)
    height: Math.min(640, parent ? parent.height - 40 : 640)
    standardButtons: Dialog.Close
    onOpened: backend.refreshDevices()

    function setValue(key, value) {
        const err = backend.setPreference(key, JSON.stringify(value))
        const e = Object.assign({}, errors)
        if (err)
            e[key] = err
        else
            delete e[key]
        errors = e
    }

    RowLayout {
        anchors.fill: parent
        spacing: 8

        ListView {
            id: groups
            Layout.preferredWidth: 150
            Layout.fillHeight: true
            model: ["API keys"].concat(dialog.prefs.groups || [])
            delegate: ItemDelegate {
                required property string modelData
                width: ListView.view.width
                text: modelData
                highlighted: dialog.group === modelData
                onClicked: dialog.group = modelData
            }
        }

        ScrollView {
            Layout.fillWidth: true
            Layout.fillHeight: true
            contentWidth: availableWidth

            ColumnLayout {
                width: parent.width
                spacing: 12

                // ----- API keys -----
                Repeater {
                    model: dialog.group === "API keys" ? ["deepgram", "openai", "anthropic"] : []
                    delegate: ColumnLayout {
                        required property string modelData
                        readonly property var status: dialog.keys[modelData] || {}
                        Layout.fillWidth: true
                        Label { text: status.label || modelData; font.bold: true }
                        Label {
                            text: status.has ? (status.persisted ? qsTr("Saved in the system keyring.") : qsTr("Kept for this session only."))
                                             : qsTr("Not set.")
                            opacity: 0.7
                        }
                        RowLayout {
                            Layout.fillWidth: true
                            TextField {
                                id: keyField
                                echoMode: TextInput.Password
                                placeholderText: qsTr("Paste a key")
                                Layout.fillWidth: true
                            }
                            CheckBox { id: remember; text: qsTr("Remember"); checked: true }
                            Button {
                                text: qsTr("Save")
                                enabled: keyField.text.trim().length > 0
                                onClicked: {
                                    backend.setApiKey(modelData, keyField.text, remember.checked)
                                    keyField.text = ""
                                }
                            }
                            Button {
                                text: qsTr("Forget")
                                enabled: status.has === true
                                onClicked: backend.forgetApiKey(modelData)
                            }
                        }
                    }
                }
                Label {
                    visible: dialog.group === "API keys"
                    text: qsTr("Keys are sent only to their provider and are never written to files or history.")
                    wrapMode: Text.Wrap
                    opacity: 0.7
                    Layout.fillWidth: true
                }

                CheckBox {
                    visible: dialog.group !== "API keys"
                    text: qsTr("Show advanced settings")
                    checked: dialog.showAdvanced
                    onToggled: dialog.showAdvanced = checked
                }

                // ----- Generated settings -----
                Repeater {
                    model: (dialog.prefs.schema || []).filter(d => d.group === dialog.group
                                                             && (dialog.showAdvanced || !d.advanced))
                    delegate: ColumnLayout {
                        id: row
                        required property var modelData
                        readonly property var value: dialog.prefs.values ? dialog.prefs.values[modelData.key] : undefined
                        readonly property bool changed: JSON.stringify(value) !== JSON.stringify(modelData.default)
                        Layout.fillWidth: true
                        spacing: 2

                        RowLayout {
                            Layout.fillWidth: true
                            Label {
                                text: row.modelData.label + (row.modelData.advanced ? qsTr(" (advanced)") : "")
                                font.bold: true
                                Layout.fillWidth: true
                            }
                            Label {
                                visible: row.modelData.applies === "nextRecording"
                                text: qsTr("next recording")
                                font.pointSize: 8
                                opacity: 0.6
                            }
                            Button {
                                text: qsTr("Reset")
                                flat: true
                                visible: row.changed
                                onClicked: backend.resetPreference(row.modelData.key)
                            }
                        }

                        Loader {
                            Layout.fillWidth: true
                            Layout.preferredHeight: item ? item.height : 0
                            sourceComponent: {
                                switch (row.modelData.kind.type) {
                                case "bool": return boolEditor
                                case "int": return intEditor
                                case "choice": return choiceEditor
                                default: return row.modelData.kind.multiline ? multiEditor : textEditor
                                }
                            }
                            Component {
                                id: boolEditor
                                Switch {
                                    checked: row.value === true
                                    onToggled: dialog.setValue(row.modelData.key, checked)
                                }
                            }
                            Component {
                                id: intEditor
                                RowLayout {
                                    SpinBox {
                                        from: row.modelData.kind.min
                                        to: row.modelData.kind.max
                                        value: row.value || 0
                                        editable: true
                                        stepSize: Math.max(1, Math.round((row.modelData.kind.max - row.modelData.kind.min) / 200))
                                        onValueModified: dialog.setValue(row.modelData.key, value)
                                    }
                                    Label { text: row.modelData.kind.unit; opacity: 0.7 }
                                    Item { Layout.fillWidth: true }
                                }
                            }
                            Component {
                                id: choiceEditor
                                ComboBox {
                                    model: row.modelData.kind.options.map(o => o[1])
                                    currentIndex: row.modelData.kind.options.findIndex(o => o[0] === row.value)
                                    displayText: currentIndex < 0 && row.value ? row.value + qsTr(" (unavailable)") : currentText
                                    onActivated: (i) => dialog.setValue(row.modelData.key, row.modelData.kind.options[i][0])
                                }
                            }
                            Component {
                                id: textEditor
                                TextField {
                                    text: row.value || ""
                                    onEditingFinished: if (text !== row.value) dialog.setValue(row.modelData.key, text)
                                }
                            }
                            Component {
                                id: multiEditor
                                TextArea {
                                    text: row.value || ""
                                    wrapMode: TextEdit.Wrap
                                    height: 90
                                    onEditingFinished: if (text !== row.value) dialog.setValue(row.modelData.key, text)
                                }
                            }
                        }
                        Label {
                            visible: !!dialog.errors[row.modelData.key]
                            text: dialog.errors[row.modelData.key] || ""
                            color: "#c0392b"
                            wrapMode: Text.Wrap
                            Layout.fillWidth: true
                        }
                        Label {
                            text: row.modelData.help
                            wrapMode: Text.Wrap
                            opacity: 0.65
                            font.pointSize: 9
                            Layout.fillWidth: true
                        }
                    }
                }
            }
        }
    }
}
