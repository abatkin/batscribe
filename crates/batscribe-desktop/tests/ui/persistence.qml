// Persistence and inspection across launches. Run three times with the same UI_TEST_DATA:
//   --phase=1  dictate, rename, add a second document, close normally
//   --phase=2  everything reopens without network; then exit abruptly while recording
//   --phase=3  the interrupted recording is marked and its saved text survived
import QtQuick
import QtTest

Item {
    id: t

    property int step: 0
    property var failures: []
    property int waited: 0
    property string firstId: ""
    readonly property var editor: window.editorPane.editor
    readonly property var helper: window.editorPane.helper
    readonly property var details: window.detailsPane
    readonly property string phase: {
        const a = Qt.application.arguments.find(x => x.startsWith("--phase="))
        return a ? a.substring("--phase=".length) : "1"
    }
    readonly property string expected: "Persisted note. At the store, please buy six oranges. Then come home."

    TestEvent { id: keys }

    function log(m) { console.warn("TEST: " + m) }
    function check(cond, what) {
        if (cond) {
            log("ok   " + what)
        } else {
            failures.push(what)
            log("FAIL " + what)
        }
    }
    function type(text) {
        for (const c of text)
            keys.keyClickChar(c, Qt.NoModifier, -1)
    }
    function docs() { return JSON.parse(backend.documentsJson) }
    function finish() {
        log(failures.length === 0 ? "PASS" : "FAILED: " + failures.join("; "))
        if (failures.length === 0)
            window.close() // normal shutdown: flushes and closes storage
        else
            Qt.exit(1)
    }
    function useDevProviders() {
        backend.setPreference("asr.provider", '"mock"')
        backend.setPreference("comp.provider", '"mock"')
        backend.setPreference("audio.device", '"synthetic"')
    }

    Timer {
        interval: 200
        running: true
        repeat: true
        onTriggered: {
            if (!backend.ready)
                return
            if (t.phase === "1") t.phase1()
            else if (t.phase === "2") t.phase2()
            else t.phase3()
        }
    }

    function phase1() {
        switch (step) {
        case 0:
            useDevProviders()
            window.requestActivate()
            editor.forceActiveFocus()
            type("Persisted note.")
            window.editorPane.startOrStop()
            step = 1
            break
        case 1:
            if (backend.modelText().indexOf("home.") < 0)
                return
            window.editorPane.startOrStop()
            step = 2
            break
        case 2:
            if (backend.recordingState !== "idle")
                return
            check(helper.plainText() === expected, "dictated text " + JSON.stringify(helper.plainText()))
            firstId = backend.currentDocumentId
            backend.renameDocument(firstId, "Groceries")
            backend.newDocument()
            editor.forceActiveFocus()
            type("Second document")
            backend.flushEdits()
            step = 3
            break
        case 3:
            if (docs().length < 2)
                return
            backend.openDocument(firstId)
            check(helper.plainText() === expected, "switching back shows the first document")
            check(backend.currentTitle === "Groceries", "renamed")
            details.visible = true
            details.loadDocument()
            step = 4
            break
        case 4:
            if (details.docRecordings.length === 0)
                return
            check(details.docRecordings[0].status === "completed", "recording completed")
            details.openRecording(details.docRecordings[0].id)
            step = 5
            break
        case 5:
            if (!details.events.rows || details.events.rows.length === 0 || details.operations.length === 0)
                return
            check(details.events.total > 10, "timeline has events (" + details.events.total + ")")
            const comp = details.operations.find(o => o.kind === "composition" && o.status === "applied")
            check(!!comp, "an applied composition operation")
            if (comp)
                details.openOperation(comp.id)
            step = 6
            break
        case 6:
            if (!details.operation)
                return
            check(details.operation.request.indexOf("current_text") >= 0, "exact model input is stored")
            check(details.operation.response.indexOf("six oranges") >= 0, "exact model output is stored")
            details.openEvent(details.events.rows[0].seq)
            step = 7
            break
        case 7:
            if (!details.eventDetail)
                return
            check(!backend.inspectorLive, "inspecting an event pins the view")
            finish()
        }
    }

    function phase2() {
        switch (step) {
        case 0:
            const g = docs().find(d => d.title === "Groceries")
            check(!!g, "documents reopen")
            check(docs().some(d => d.preview === "Second document"), "second document saved")
            if (!g) { finish(); return }
            firstId = g.id
            backend.openDocument(g.id)
            check(helper.plainText() === expected, "text reopens: " + JSON.stringify(helper.plainText()))
            const start = expected.indexOf("six"), end = start + 3
            check(JSON.parse(backend.recordingsForSelection(start, end)).length === 1, "dictated text links to its recording")
            check(JSON.parse(backend.recordingsForSelection(0, 9)).length === 0, "typed text has no recording")
            details.visible = true
            details.loadDocument()
            step = 1
            break
        case 1:
            if (details.docRecordings.length === 0)
                return
            check(details.docRecordings.length === 1, "history reopens")
            // Start a recording and stop the process abruptly once some text was saved.
            useDevProviders()
            editor.forceActiveFocus()
            editor.cursorPosition = editor.length
            keys.keyClick(Qt.Key_Return, Qt.NoModifier, -1)
            window.editorPane.startOrStop()
            step = 2
            break
        case 2:
            if (backend.modelText().indexOf("oranges,") >= 0)
                step = 3
            break
        case 3:
            if (++waited < 25) // > 3 s, so the periodic save has run
                return
            log("exiting abruptly while recording")
            Qt.exit(0)
        }
    }

    function phase3() {
        switch (step) {
        case 0:
            const g = docs().find(d => d.title === "Groceries")
            backend.openDocument(g.id)
            const text = helper.plainText()
            check(text.startsWith(expected + "\nAt the store"), "text saved during the interrupted recording survived: " + JSON.stringify(text))
            check(backend.recordingState === "idle", "nothing is recording after restart")
            details.visible = true
            details.loadDocument()
            step = 1
            break
        case 1:
            if (details.docRecordings.length < 2)
                return
            check(details.docRecordings[0].status === "interrupted", "unfinished recording marked interrupted")
            // Typing works normally in the formerly dictated text.
            editor.forceActiveFocus()
            editor.cursorPosition = editor.length
            type(" ok")
            check(helper.plainText() === backend.modelText(), "model == editor")
            finish()
        }
    }

    Timer {
        interval: 60000
        running: true
        onTriggered: {
            log("FAIL timed out in phase " + t.phase + " at step " + t.step)
            Qt.exit(1)
        }
    }
}
