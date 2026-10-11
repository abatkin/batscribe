// Records from the real default microphone (mock recognition, no network) with audio retention.
// Needs a working input device; not part of the headless suite.
import QtQuick

Item {
    id: t
    property int step: 0
    property var failures: []
    property real peak: 0
    property int ticks: 0
    readonly property var details: window.detailsPane

    function log(m) { console.warn("TEST: " + m) }
    function check(cond, what) {
        if (cond) log("ok   " + what)
        else { failures.push(what); log("FAIL " + what) }
    }
    Connections {
        target: backend
        function onLevelChanged() { t.peak = Math.max(t.peak, backend.level) }
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
                backend.setPreference("asr.provider", '"mock"')
                backend.setPreference("comp.provider", '"mock"')
                backend.setPreference("audio.device", '""')
                backend.setPreference("audio.retain", 'true')
                window.editorPane.startOrStop()
                t.step = 1
                break
            case 1:
                if (++t.ticks < 25)
                    return
                window.editorPane.startOrStop()
                t.step = 2
                break
            case 2:
                if (backend.recordingState !== "idle")
                    return
                check(t.peak > 0, "level meter moved (peak " + t.peak.toFixed(2) + ")")
                check(backend.modelText().indexOf("store") >= 0, "mock recognition ran on microphone audio")
                details.visible = true
                details.loadDocument()
                t.step = 3
                break
            case 3:
                if (details.docRecordings.length === 0)
                    return
                details.openRecording(details.docRecordings[0].id)
                t.step = 4
                break
            case 4:
                if (!details.recording)
                    return
                const r = details.recording
                log("device: " + r.device + "; audio: " + r.audio_status + " " + r.samples + " samples")
                check(r.status === "completed", "recording completed")
                check(/Hz/.test(r.device || ""), "device recorded")
                check(r.audio_status === "complete" && r.audioExists === true, "audio file complete")
                check(r.samples > 16000 * 3, "audio length plausible")
                log(t.failures.length === 0 ? "PASS" : "FAILED: " + t.failures.join("; "))
                Qt.exit(t.failures.length === 0 ? 0 : 1)
            }
        }
    }
    Timer {
        interval: 60000
        running: true
        onTriggered: { log("FAIL timed out at step " + t.step); Qt.exit(1) }
    }
}
