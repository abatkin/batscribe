// Review finding: switching documents after a failed save lost the unsaved text. Also: closing
// while saves fail waits (exit dialog) and quits by itself once a save succeeds.
import QtQuick
import QtTest

Item {
    id: t
    property int step: 0
    property var failures: []
    property string firstId: ""
    readonly property var pane: window.editorPane
    TestEvent { id: keys }
    function log(m) { console.warn("TEST: " + m) }
    function check(cond, what) {
        if (cond) log("ok   " + what)
        else { failures.push(what); log("FAIL " + what) }
    }
    function type(s) { for (const c of s) keys.keyClickChar(c, Qt.NoModifier, -1) }
    Timer {
        interval: 200
        running: true
        repeat: true
        onTriggered: {
            if (!backend.ready)
                return
            switch (t.step) {
            case 0:
                t.firstId = backend.currentDocumentId
                pane.editor.forceActiveFocus()
                backend.debugFailSaves(true)
                t.type("Unsaved words")
                backend.flushEdits()
                t.step = 1
                break
            case 1:
                if (!backend.saveError)
                    return
                check(backend.unsavedCount >= 1, "the failed document counts as unsaved")
                backend.newDocument()
                backend.openDocument(t.firstId)
                check(pane.helper.plainText() === "Unsaved words", "unsaved text survives switching away and back: "
                      + JSON.stringify(pane.helper.plainText()))
                backend.debugFailSaves(false)
                backend.flushEdits()
                t.step = 2
                break
            case 2:
                if (backend.unsavedCount !== 0)
                    return
                check(backend.saveError === "", "the error clears once saved")
                backend.newDocument()
                backend.openDocument(t.firstId)
                check(pane.helper.plainText() === "Unsaved words", "saved text reopens from the database")
                // Closing while saves fail must wait.
                backend.debugFailSaves(true)
                pane.editor.forceActiveFocus()
                pane.editor.cursorPosition = pane.editor.length
                t.type("!")
                window.close()
                t.step = 3
                break
            case 3:
                if (!backend.saveError)
                    return
                check(window.exitDialog.visible, "the exit dialog waits for the save")
                check(window.visible, "the window stays open")
                log(t.failures.length === 0 ? "PASS (quits once the save succeeds)" : "FAILED: " + t.failures.join("; "))
                if (t.failures.length > 0)
                    Qt.exit(1)
                backend.debugFailSaves(false)
                t.step = 4
                break
            }
        }
    }
    Timer {
        interval: 30000
        running: true
        onTriggered: { log("FAIL timed out at step " + t.step); Qt.exit(1) }
    }
}
