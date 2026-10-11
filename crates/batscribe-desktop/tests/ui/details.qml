// Screenshots of the Details pane at a narrow width (--shot-dir=DIR). Not pass/fail.
import QtQuick
import QtTest

Item {
    id: t
    property int step: 0
    readonly property string outDir: Qt.application.arguments.find(x => x.startsWith("--shot-dir=")).substring(11)
    readonly property var d: window.detailsPane
    TestEvent { id: keys }
    function shot(name) {
        d.grabToImage(r => console.warn("TEST: shot " + name + " " + r.saveToFile(outDir + "/" + name + ".png")))
    }
    Timer {
        interval: 250
        running: true
        repeat: true
        onTriggered: {
            if (!backend.ready)
                return
            switch (t.step) {
            case 0:
                backend.setPreference("asr.provider", '"mock"')
                backend.setPreference("comp.provider", '"mock"')
                backend.setPreference("audio.device", '"synthetic"')
                d.visible = true
                t.step = 10
                break
            case 10: {
                // Narrow the pane by dragging its handle 90 px to the right.
                const p = d.mapToItem(null, 0, 300)
                keys.mousePress(window, p.x - 2, p.y, Qt.LeftButton, Qt.NoModifier, -1)
                for (let i = 1; i <= 10; i++)
                    keys.mouseMove(window, p.x - 2 + 9 * i, p.y, -1, Qt.LeftButton, Qt.NoModifier)
                keys.mouseRelease(window, p.x + 88, p.y, Qt.LeftButton, Qt.NoModifier, -1)
                console.warn("TEST: details width " + d.width)
                window.editorPane.startOrStop()
                t.step = 1
                break
            }
            case 1:
                if (backend.modelText().indexOf("home.") < 0)
                    return
                window.editorPane.startOrStop()
                t.step = 2
                break
            case 2:
                if (backend.recordingState !== "idle")
                    return
                d.loadDocument()
                t.step = 3
                break
            case 3:
                if (d.docRecordings.length === 0)
                    return
                d.openRecording(d.docRecordings[0].id)
                t.step = 4
                break
            case 4:
                if (!d.recording || !d.events.rows || d.operations.length === 0)
                    return
                t.step = 5
                break
            case 5:
                shot("recording-timeline")
                t.step = 6
                break
            case 6:
                d.openOperation(d.operations.find(o => o.status === "applied").id)
                t.step = 7
                break
            case 7:
                if (!d.operation)
                    return
                shot("operation")
                t.step = 8
                break
            case 8:
                Qt.exit(0)
            }
        }
    }
}
