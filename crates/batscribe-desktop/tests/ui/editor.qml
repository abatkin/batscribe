// Editor foundation test, loaded into the main window by debug builds via --qml-script.
// Run through scripts/ui-test.sh. Drives real key events through the TextArea and checks that
// the Rust model and the Qt document always agree.
import QtQuick
import QtTest

Item {
    id: t

    property int step: 0
    property var failures: []
    readonly property var editor: window.editorPane.editor
    readonly property var helper: window.editorPane.helper

    readonly property string outDir: {
        const a = Qt.application.arguments.find(x => x.startsWith("--shot-dir="))
        return a ? a.substring("--shot-dir=".length) : ""
    }

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
    // Characters outside the BMP can't be sent as key events; they go in like an input-method
    // commit would, through the document.
    function type(text) {
        for (const c of text) {
            if (c.length === 1 && c.charCodeAt(0) < 128)
                keys.keyClickChar(c, Qt.NoModifier, -1)
            else
                editor.insert(editor.cursorPosition, c)
        }
    }
    function inSync(what) {
        check(helper.plainText() === backend.modelText(),
              what + ": model == editor (" + JSON.stringify(helper.plainText()) + ")")
    }

    Timer {
        interval: 150
        running: true
        repeat: true
        onTriggered: {
            switch (t.step) {
            case 0:
                if (!backend.ready)
                    return
                // Development providers: scripted recognition, rule-based editing, no audio device.
                check(backend.setPreference("asr.provider", '"mock"') === "", "mock recognition selected")
                check(backend.setPreference("comp.provider", '"mock"') === "", "mock editing selected")
                check(backend.setPreference("audio.device", '"synthetic"') === "", "synthetic audio selected")
                check(backend.setPreference("asr.endpointing_ms", '5') !== "", "out-of-range value rejected")
                window.requestActivate()
                editor.forceActiveFocus()
                t.type("Hello world")
                inSync("typing")
                // Caret mid-line: Start must place the region at the end of the logical line.
                editor.cursorPosition = 3
                window.editorPane.startOrStop()
                check(backend.recordingState === "recording", "recording started")
                check(editor.cursorPosition === 11, "caret follows to the region at line end")
                t.step = 1
                break
            case 1:
                if (backend.modelText().indexOf("store") < 0)
                    return
                // Type before the region: allowed, region shifts.
                editor.cursorPosition = 0
                check(!backend.following, "explicit navigation detaches following")
                t.type("😀 ")
                inSync("typing before the region")
                // Type inside the region: blocked by the key filter.
                const inside = backend.voiceCaret - 3
                editor.cursorPosition = inside
                const before = helper.plainText()
                t.type("Q")
                keys.keyClick(Qt.Key_Backspace, Qt.NoModifier, -1)
                check(helper.plainText() === before, "keys inside the region are blocked")
                // A change that bypasses the key filter is reverted.
                editor.insert(inside, "ZZ")
                t.step = 2
                break
            case 2:
                if (t.outDir)
                    window.editorPane.grabToImage(r => t.log("shot saved: " + r.saveToFile(t.outDir + "/editor-dictating.png")))
                check(helper.plainText().indexOf("ZZ") < 0, "programmatic insert into region reverted")
                inSync("after revert")
                t.step = 3
                break
            case 3:
                if (backend.recordingState === "recording" && backend.modelText().indexOf("home.") >= 0) {
                    window.editorPane.startOrStop()
                    check(backend.voiceCaret === -1, "voice caret hidden at Stop")
                    return
                }
                if (backend.recordingState !== "idle")
                    return
                const expected = "😀 Hello world At the store, please buy six oranges. Then come home."
                check(helper.plainText() === expected, "final text " + JSON.stringify(helper.plainText()))
                inSync("after dictation")
                check(backend.voiceCaret === -1, "voice caret hidden after finishing")
                // Undo affects only typed text: first the emoji, then "Hello world".
                editor.forceActiveFocus()
                keys.keyClick(Qt.Key_Z, Qt.ControlModifier, -1)
                check(helper.plainText().startsWith("Hello world At the store"), "undo removes typed emoji")
                keys.keyClick(Qt.Key_Z, Qt.ControlModifier, -1)
                check(helper.plainText() === " At the store, please buy six oranges. Then come home.",
                      "second undo removes typed words, keeps dictation")
                keys.keyClick(Qt.Key_Z, Qt.ControlModifier | Qt.ShiftModifier, -1)
                check(helper.plainText().startsWith("Hello world At"), "redo")
                inSync("after undo/redo")
                // Unicode and newline editing after recording.
                editor.cursorPosition = editor.length
                keys.keyClick(Qt.Key_Return, Qt.NoModifier, -1)
                t.type("naïve café")
                keys.keyClick(Qt.Key_Backspace, Qt.NoModifier, -1)
                inSync("newline and accents")
                t.step = 4
                break
            case 4:
                log(failures.length === 0 ? "PASS" : "FAILED: " + failures.join("; "))
                Qt.exit(failures.length === 0 ? 0 : 1)
            }
        }
    }

    Timer {
        interval: 60000
        running: true
        onTriggered: {
            log("FAIL timed out at step " + t.step)
            Qt.exit(1)
        }
    }
}
