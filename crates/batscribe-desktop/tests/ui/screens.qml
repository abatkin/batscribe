// Captures screenshots of the main views (--shot-dir=DIR). Not a pass/fail test.
import QtQuick
import QtTest

Item {
    id: t
    property int step: 0
    readonly property string outDir: {
        const a = Qt.application.arguments.find(x => x.startsWith("--shot-dir="))
        return a ? a.substring("--shot-dir=".length) : "/tmp"
    }
    TestEvent { id: keys }
    function shot(item, name) {
        item.grabToImage(r => console.warn("TEST: shot " + name + " " + r.saveToFile(outDir + "/" + name + ".png")))
    }
    Timer {
        interval: 250
        running: true
        repeat: true
        onTriggered: {
            if (!backend.ready)
                return
            const ed = window.editorPane.editor
            switch (t.step) {
            case 0:
                backend.setPreference("asr.provider", '"mock"')
                backend.setPreference("comp.provider", '"mock"')
                backend.setPreference("audio.device", '"synthetic"')
                ed.forceActiveFocus()
                for (const c of "Notes from Tuesday.")
                    keys.keyClickChar(c, Qt.NoModifier, -1)
                window.detailsPane.visible = true
                window.editorPane.startOrStop()
                t.step = 1
                break
            case 1:
                if (backend.modelText().indexOf("no actually") < 0)
                    return
                shot(window.splitView, "main-dictating")
                t.step = 2
                break
            case 2:
                if (backend.modelText().indexOf("home.") < 0)
                    return
                window.editorPane.startOrStop()
                t.step = 3
                break
            case 3:
                if (backend.recordingState !== "idle")
                    return
                window.detailsPane.loadDocument()
                t.step = 4
                break
            case 4:
                if (window.detailsPane.docRecordings.length === 0)
                    return
                window.detailsPane.openRecording(window.detailsPane.docRecordings[0].id)
                t.step = 5
                break
            case 5:
                if (!window.detailsPane.events.rows || window.detailsPane.operations.length === 0)
                    return
                shot(window.splitView, "main-inspector")
                const comp = window.detailsPane.operations.find(o => o.status === "applied")
                window.detailsPane.openOperation(comp.id)
                t.step = 6
                break
            case 6:
                if (!window.detailsPane.operation)
                    return
                shot(window.detailsPane, "operation")
                window.preferences.group = "Composition"; window.preferences.showAdvanced = true
                window.preferences.open()
                t.step = 7
                break
            case 7:
                shot(window.preferences.contentItem, "preferences")
                t.step = 8
                break
            case 8:
                Qt.exit(0)
            }
        }
    }
}
