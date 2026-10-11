// Review finding: closing during "Finishing" lost the last recognition and corrections.
// --phase=1: dictate, close mid-way; the exit dialog waits and quits by itself.
// --phase=2: the corrected text was saved and the recording completed.
// --phase=3: as phase 1, but "Quit Now" at once; --phase=4: the text shown at the time was
// saved and the recording is marked interrupted.
// --phase=5: as phase 1, but saves fail from the moment of closing, so the correction that
// arrives while finishing can't be saved: the dialog must stay open (not quit) until saving
// works again. Then --phase=2 checks the corrected text was saved.
import QtQuick

Item {
    id: t
    property int step: 0
    property var failures: []
    property string shown: ""
    property int waited: 0
    readonly property string phase: Qt.application.arguments.find(a => a.startsWith("--phase=")).substring(8)
    readonly property var details: window.detailsPane
    function log(m) { console.warn("TEST: " + m) }
    function check(cond, what) {
        if (cond) log("ok   " + what)
        else { failures.push(what); log("FAIL " + what) }
    }
    Timer {
        interval: 200
        running: true
        repeat: true
        onTriggered: {
            if (!backend.ready)
                return
            if (t.phase === "5" && t.step >= 2) {
                switch (t.step) {
                case 2:
                    if (backend.recordingState !== "idle" || !backend.saveError)
                        return
                    check(backend.modelText() === "At the store, please buy six oranges.", "the correction arrived while finishing")
                    check(window.visible && window.exitDialog.visible, "the dialog waits for the failed save instead of quitting")
                    check(backend.unsavedCount > 0, "the correction counts as unsaved: " + backend.unsavedCount)
                    t.waited = 0
                    t.step = 3
                    break
                case 3:
                    // Still waiting a few seconds later (saves are retried and keep failing).
                    if (++t.waited < 15)
                        return
                    check(window.visible, "still open while saves fail")
                    log(t.failures.length === 0 ? "PASS (quits once saving works)" : "FAILED: " + t.failures.join("; "))
                    if (t.failures.length > 0)
                        Qt.exit(1)
                    backend.debugFailSaves(false)
                    t.step = 4
                    break
                }
                return
            }
            if (t.phase === "1" || t.phase === "3" || t.phase === "5") {
                switch (t.step) {
                case 0:
                    backend.setPreference("asr.provider", '"mock"')
                    backend.setPreference("comp.provider", '"mock"')
                    backend.setPreference("audio.device", '"synthetic"')
                    window.editorPane.startOrStop()
                    t.step = 1
                    break
                case 1:
                    // Close right after the correction is final, before the edit is applied.
                    if (backend.modelText().indexOf("actually six oranges.") < 0)
                        return
                    window.close()
                    if (t.phase === "5")
                        backend.debugFailSaves(true)
                    check(window.exitDialog.visible, "closing while dictating opens the exit dialog")
                    check(backend.recordingState !== "recording", "closing stopped the recording")
                    if (t.phase === "3") {
                        t.shown = backend.modelText()
                        log("quitting now with " + JSON.stringify(t.shown))
                        window.quitNow()
                        return
                    }
                    if (t.phase === "5") {
                        t.step = 2
                        break
                    }
                    log(t.failures.length === 0 ? "PASS (quits when finishing completes)" : "FAILED")
                    if (t.failures.length > 0)
                        Qt.exit(1)
                    t.step = 2
                    break
                }
            } else if (t.phase === "4") {
                switch (t.step) {
                case 0:
                    check(backend.modelText().indexOf("actually six oranges.") >= 0,
                          "the text shown when quitting was saved: " + JSON.stringify(backend.modelText()))
                    details.visible = true
                    details.loadDocument()
                    t.step = 1
                    break
                case 1:
                    if (details.docRecordings.length === 0)
                        return
                    check(details.docRecordings[0].status === "interrupted", "recording marked interrupted: " + details.docRecordings[0].status)
                    log(t.failures.length === 0 ? "PASS" : "FAILED: " + t.failures.join("; "))
                    Qt.exit(t.failures.length === 0 ? 0 : 1)
                }
            } else {
                switch (t.step) {
                case 0:
                    check(backend.modelText() === "At the store, please buy six oranges.",
                          "the correction finished before closing: " + JSON.stringify(backend.modelText()))
                    details.visible = true
                    details.loadDocument()
                    t.step = 1
                    break
                case 1:
                    if (details.docRecordings.length === 0)
                        return
                    check(details.docRecordings[0].status === "completed", "recording completed: " + details.docRecordings[0].status)
                    log(t.failures.length === 0 ? "PASS" : "FAILED: " + t.failures.join("; "))
                    Qt.exit(t.failures.length === 0 ? 0 : 1)
                }
            }
        }
    }
    Timer {
        interval: 40000
        running: true
        onTriggered: { log("FAIL timed out at step " + t.step); Qt.exit(1) }
    }
}
