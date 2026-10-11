// Drags both SplitView handles with the mouse and checks the side panes resize.
import QtQuick
import QtTest

Item {
    id: t
    property int step: 0
    property var failures: []
    property real before: 0
    TestEvent { id: ev }
    function log(m) { console.warn("TEST: " + m) }
    function check(cond, what) {
        if (cond) log("ok   " + what)
        else { failures.push(what); log("FAIL " + what) }
    }
    function drag(x, y, dx) {
        ev.mouseMove(window, x, y, -1, Qt.NoButton, Qt.NoModifier)
        ev.mousePress(window, x, y, Qt.LeftButton, Qt.NoModifier, -1)
        for (let i = 1; i <= 10; i++)
            ev.mouseMove(window, x + dx * i / 10, y, -1, Qt.LeftButton, Qt.NoModifier)
        ev.mouseRelease(window, x + dx, y, Qt.LeftButton, Qt.NoModifier, -1)
    }
    Timer {
        interval: 300
        running: true
        repeat: true
        onTriggered: {
            if (!backend.ready)
                return
            const docs = window.documentsPane, details = window.detailsPane
            const y = window.splitView.mapToItem(window.contentItem, 0, 0).y + 200
            const header = window.contentItem.mapFromItem(window.splitView, 0, 0)
            switch (t.step) {
            case 0:
                details.visible = true
                t.step = 1
                break
            case 1: {
                t.before = docs.width
                const p = docs.mapToItem(null, docs.width, 0)
                log("style " + (window.splitView.handle ? "custom/handle" : "") + "; documents right edge x=" + p.x)
                // Grab a few pixels beside the handle, as a user would.
                drag(p.x + 2, y + header.y, 120)
                t.step = 2
                break
            }
            case 2: {
                check(docs.width > t.before + 50, "documents pane resized: " + t.before + " -> " + docs.width)
                t.before = details.width
                const p = details.mapToItem(null, 0, 0)
                drag(p.x - 2, y + header.y, -120)
                t.step = 3
                break
            }
            case 3:
                check(details.width > t.before + 50, "details pane resized: " + t.before + " -> " + details.width)
                log(t.failures.length === 0 ? "PASS" : "FAILED: " + t.failures.join("; "))
                Qt.exit(t.failures.length === 0 ? 0 : 1)
            }
        }
    }
}
