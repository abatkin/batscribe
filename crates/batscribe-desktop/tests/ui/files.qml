// Copy to clipboard, Save As and Save. Run through scripts/ui-test.sh.
import QtQuick
import QtTest

Item {
    id: t
    property int step: 0
    property var failures: []
    readonly property var pane: window.editorPane
    readonly property string dir: Qt.application.arguments.find(a => a.startsWith("--out=")).substring(6)

    TestEvent { id: keys }
    // Reads the clipboard back.
    TextEdit { id: pasteTarget; visible: false }

    function log(m) { console.warn("TEST: " + m) }
    function check(cond, what) {
        if (cond) log("ok   " + what)
        else { failures.push(what); log("FAIL " + what) }
    }
    function read(path) {
        const x = new XMLHttpRequest()
        x.open("GET", "file://" + path, false)
        x.send()
        return x.responseText
    }
    Timer {
        interval: 200
        running: true
        repeat: true
        onTriggered: {
            if (!backend.ready)
                return
            switch (t.step) {
            case 0:
                pane.editor.forceActiveFocus()
                for (const c of "Line one\nLine two") {
                    if (c === "\n")
                        keys.keyClick(Qt.Key_Return, Qt.NoModifier, -1)
                    else
                        keys.keyClickChar(c, Qt.NoModifier, -1)
                }
                pane.copyAll()
                pasteTarget.text = ""
                pasteTarget.paste()
                check(pasteTarget.text === "Line one\nLine two", "copy puts the document on the clipboard")
                check(backend.currentFilePath === "", "no file yet")
                const err = backend.saveToFile("file://" + t.dir + "/My%20Notes.txt")
                check(err === "", "save as succeeds " + err)
                check(backend.currentFilePath === t.dir + "/My Notes.txt", "file remembered: " + backend.currentFilePath)
                check(backend.currentTitle === "My Notes", "untitled document takes the file name")
                check(t.read(t.dir + "/My Notes.txt") === "Line one\nLine two\n", "file content")
                // Edit, then Save writes to the same file without asking.
                pane.editor.cursorPosition = pane.editor.length
                keys.keyClickChar("!", Qt.NoModifier, -1)
                pane.save()
                check(t.read(t.dir + "/My Notes.txt") === "Line one\nLine two!\n", "save rewrites the remembered file")
                check(backend.saveToFile("file:///nonexistent-dir/x.txt") !== "", "an unwritable location reports an error")
                backend.newDocument()
                t.step = 1
                break
            case 1:
                check(backend.currentFilePath === "", "a new document has no file")
                const first = JSON.parse(backend.documentsJson).find(d => d.title === "My Notes")
                backend.openDocument(first.id)
                check(backend.currentFilePath === t.dir + "/My Notes.txt", "the file is remembered when reopening")
                log(t.failures.length === 0 ? "PASS" : "FAILED: " + t.failures.join("; "))
                Qt.exit(t.failures.length === 0 ? 0 : 1)
            }
        }
    }
}
