import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import net.batkin.batscribe

// History inspector: document → recording → operation → event, plus document edits.
// Everything loads asynchronously and in pages; inspecting never blocks the editor.
//
// Within a recording, every event and operation carries a colored label ("Recognition 1",
// "Edit 3", "Session") and a time relative to the recording's start, so events that belong
// together are easy to pick out of the interleaved timeline.
Pane {
    id: pane

    required property Backend backend
    // "document", "recording", "operation" or "event"
    property string level: "document"
    property string recordingId: ""
    property string operationId: ""
    property string eventSeq: ""
    property var docRecordings: []
    property var docEdits: ({ rows: [], total: 0, offset: 0 })
    property var gaps: []
    property var recording: null
    property var operations: []
    property var events: ({ rows: [], total: 0, offset: 0 })
    property var transcripts: []
    property var recEdits: ({ rows: [], total: 0, offset: 0 })
    property var operation: null
    property var opEvents: ({ rows: [], total: 0, offset: 0 })
    property var eventDetail: null
    // Timeline filter: "all", "final", "edits" or "recognition".
    property string timelineFilter: "all"
    readonly property int page: backend.pageSize
    readonly property real startMs: recording ? recording.started_at : 0

    // Operation ID -> {label, kind}, numbered per kind in start order.
    readonly property var opLabels: {
        const map = {}
        const counts = {}
        const sorted = operations.slice().sort((a, b) => a.started_at - b.started_at)
        for (const o of sorted) {
            const kind = o.kind === "asr_connection" ? "recognition" : o.kind === "composition" ? "edit" : o.kind
            counts[kind] = (counts[kind] || 0) + 1
            const name = kind === "recognition" ? qsTr("Recognition") : kind === "edit" ? qsTr("Edit") : kind
            map[o.id] = { label: name + " " + counts[kind], kind: kind }
        }
        return map
    }

    padding: 0

    function openRecording(id) {
        recordingId = id
        level = "recording"
        recTabs.currentIndex = 0
        loadRecording()
    }
    function openOperation(id) {
        operationId = id
        level = "operation"
        opTabs.currentIndex = 0
        backend.requestHistory("operation", id, 0)
        backend.requestHistory("operationEvents", id, 0)
    }
    function openEvent(seq) {
        // Inspecting a specific event pins the view.
        backend.setInspectorLiveFollow(false)
        eventSeq = String(seq)
        level = "event"
        backend.requestHistory("event", eventSeq, 0)
    }
    function loadDocument() {
        if (!backend.currentDocumentId)
            return
        backend.requestHistory("recordings", backend.currentDocumentId, 0)
        backend.requestHistory("edits", backend.currentDocumentId, 0)
        backend.requestHistory("gaps", "", 0)
    }
    function loadRecording() {
        backend.requestHistory("recording", recordingId, 0)
        backend.requestHistory("operations", recordingId, 0)
        backend.requestHistory("events", recordingId, backend.inspectorLive ? -1 : events.offset)
        backend.requestHistory("transcripts", recordingId, 0)
        backend.requestHistory("recordingEdits", backend.currentDocumentId + "|" + recordingId, 0)
    }

    // ----- Formatting -----

    function clock(ms) {
        return ms ? new Date(ms).toLocaleTimeString(Qt.locale(), "hh:mm:ss.zzz") : ""
    }
    function shortDate(ms) {
        return ms ? new Date(ms).toLocaleString(Qt.locale(), Locale.ShortFormat) : ""
    }
    // Time since the recording started.
    function rel(ms) {
        if (!ms || !startMs)
            return clock(ms)
        const s = (ms - startMs) / 1000
        return (s < 0 ? "−" : "+") + Math.abs(s).toFixed(2) + " s"
    }
    function seconds(samples) {
        return samples === null || samples === undefined ? "" : (samples / 16000).toFixed(2) + " s"
    }
    function pretty(text) {
        if (!text)
            return ""
        try {
            return JSON.stringify(JSON.parse(text), null, 2)
        } catch (e) {
            return text
        }
    }
    function duration(r) {
        return r && r.ended_at ? ((r.ended_at - r.started_at) / 1000).toFixed(1) + " s" : ""
    }
    function ms(v) {
        return v === null || v === undefined ? "" : Math.round(v) + " ms"
    }
    // "921 in · 45 out (314 thinking) · 200 cached" from either provider's usage object.
    function usageText(text) {
        if (!text)
            return ""
        let u
        try {
            u = JSON.parse(text)
        } catch (e) {
            return text
        }
        const details = u.output_tokens_details || {}
        const thinking = details.thinking_tokens ?? details.reasoning_tokens ?? 0
        const cached = u.cache_read_input_tokens ?? (u.input_tokens_details || {}).cached_tokens ?? 0
        let s = qsTr("%1 in · %2 out").arg(u.input_tokens ?? "?").arg(u.output_tokens ?? "?")
        if (thinking)
            s += qsTr(" (%1 thinking)").arg(thinking)
        if (cached)
            s += qsTr(" · %1 cached").arg(cached)
        return s
    }
    // One measurement per line.
    function metricsText(r) {
        if (!r || !r.metrics)
            return ""
        let m
        try {
            m = JSON.parse(r.metrics)
        } catch (e) {
            return ""
        }
        const stat = x => x ? qsTr("%1 ms median, %2 ms p95").arg(x.median).arg(x.p95) : "–"
        const lines = []
        if (m.firstTextLagMs !== null && m.firstTextLagMs !== undefined)
            lines.push(qsTr("First text: %1 ms").arg(m.firstTextLagMs))
        lines.push(qsTr("Interim: %1").arg(stat(m.interimLagMs)))
        lines.push(qsTr("Final: %1").arg(stat(m.finalLagMs)))
        lines.push(qsTr("Edit: %1").arg(stat(m.editLatencyMs)))
        if (m.correctionDelayMs)
            lines.push(qsTr("Correction: %1").arg(stat(m.correctionDelayMs)))
        const edits = Object.entries(m.edits || {}).map(([k, n]) => n + " " + k)
        if (edits.length)
            lines.push(qsTr("Edits: %1").arg(edits.join(", ")))
        return lines.join("\n")
    }
    function opLabel(id) {
        return id && opLabels[id] ? opLabels[id].label : qsTr("Session")
    }
    function opKind(id) {
        return id && opLabels[id] ? opLabels[id].kind : "session"
    }
    function kindColor(kind) {
        return kind === "recognition" ? "#2874a6" : kind === "edit" ? "#7d3c98" : "#707b7c"
    }
    readonly property var eventNames: ({
        "Results": qsTr("result"), "open": qsTr("connected"), "close": qsTr("closed"),
        "audio": qsTr("audio sent"), "edit_request": qsTr("request"), "edit_result": qsTr("response"),
        "edit_superseded": qsTr("superseded"), "edit_late": qsTr("late response"),
        "edit_skipped": qsTr("skipped"), "recording_started": qsTr("started"),
        "recording_finished": qsTr("finished"), "audio_ended": qsTr("capture stopped"),
        "stop": qsTr("stop"), "metrics": qsTr("measurements"), "reconnect": qsTr("reconnect"),
        "gap": qsTr("gap"), "UtteranceEnd": qsTr("utterance end"), "SpeechStarted": qsTr("speech started")
    })
    function eventName(kind) {
        return eventNames[kind] || kind
    }
    function arrow(direction) {
        return direction === "in" ? "←" : direction === "out" ? "→" : "•"
    }
    function keepEvent(e) {
        switch (timelineFilter) {
        case "final": return !(e.kind === "Results" && (e.summary || "").startsWith("interim")) && e.kind !== "audio"
        case "edits": return opKind(e.operation_id) === "edit"
        case "recognition": return opKind(e.operation_id) === "recognition"
        default: return true
        }
    }

    onVisibleChanged: if (visible) loadDocument()
    Connections {
        target: backend
        function onCurrentDocumentIdChanged() {
            pane.level = "document"
            pane.loadDocument()
        }
        function onRecordingStateChanged() {
            if (backend.recordingState === "recording" && backend.inspectorLive
                    && backend.recordingDocumentId === backend.currentDocumentId) {
                pane.loadDocument()
            }
        }
        function onHistoryRevisionChanged() {
            if (pane.visible && backend.inspectorLive)
                liveTimer.restart()
        }
        function onHistoryLoaded(kind, key, offset, json) {
            let v
            try {
                v = JSON.parse(json)
            } catch (e) {
                return
            }
            if (v && v.error) {
                console.warn("history: " + v.error)
                return
            }
            switch (kind) {
            case "recordings": if (key === backend.currentDocumentId) pane.docRecordings = v || []; break
            case "edits": if (key === backend.currentDocumentId) pane.docEdits = v; break
            case "gaps": pane.gaps = v || []; break
            case "recording": if (key === pane.recordingId) pane.recording = v; break
            case "operations": if (key === pane.recordingId) pane.operations = v || []; break
            case "events": if (key === pane.recordingId) pane.events = v; break
            case "transcripts": if (key === pane.recordingId) pane.transcripts = v.rows || []; break
            case "recordingEdits": pane.recEdits = v; break
            case "operation": if (key === pane.operationId) pane.operation = v; break
            case "operationEvents": if (key === pane.operationId) pane.opEvents = v; break
            case "event": if (key === pane.eventSeq) pane.eventDetail = v; break
            }
        }
    }
    // Live follow: refresh the visible view shortly after history is written.
    Timer {
        id: liveTimer
        interval: 400
        onTriggered: {
            if (pane.level === "document")
                pane.loadDocument()
            else if (pane.level === "recording")
                pane.loadRecording()
        }
    }

    // ----- Components -----

    // A name/value row. Values wrap at word boundaries (anywhere only for long tokens such as
    // paths) and never make the pane wider than it is.
    component Field: RowLayout {
        property string name
        property string value
        visible: value.length > 0
        Layout.fillWidth: true
        spacing: 8
        Label {
            text: parent.name
            opacity: 0.65
            elide: Text.ElideRight
            Layout.preferredWidth: 88
            Layout.alignment: Qt.AlignTop
        }
        Label {
            text: parent.value
            wrapMode: Text.WrapAtWordBoundaryOrAnywhere
            Layout.fillWidth: true
            Layout.preferredWidth: 1
        }
    }

    // A colored label naming an operation ("Edit 3").
    component Chip: Rectangle {
        property string text
        property string kind
        implicitWidth: chipLabel.implicitWidth + 10
        implicitHeight: chipLabel.implicitHeight + 2
        radius: 3
        color: pane.kindColor(kind)
        Label {
            id: chipLabel
            anchors.centerIn: parent
            text: parent.text
            color: "white"
            font.pointSize: 8
        }
    }

    // A tab strip that wraps onto more lines when the pane is narrow, so no tab is ever hidden.
    component Tabs: Flow {
        id: tabs
        property int currentIndex: 0
        property var labels: []
        Layout.fillWidth: true
        Layout.preferredWidth: 1
        Layout.leftMargin: 4
        Layout.rightMargin: 4
        spacing: 2
        Repeater {
            model: tabs.labels
            delegate: ToolButton {
                required property int index
                required property string modelData
                text: modelData
                checkable: true
                checked: index === tabs.currentIndex
                font.bold: checked
                onClicked: tabs.currentIndex = index
            }
        }
    }

    component Pager: RowLayout {
        property var info
        property string kind
        property string key
        visible: info && info.total > pane.page
        Button {
            text: qsTr("Previous")
            flat: true
            enabled: parent.info && parent.info.offset > 0
            onClicked: {
                backend.setInspectorLiveFollow(false)
                backend.requestHistory(parent.kind, parent.key, Math.max(0, parent.info.offset - pane.page))
            }
        }
        Label {
            text: parent.info ? qsTr("%1–%2 of %3").arg(parent.info.offset + 1)
                .arg(Math.min(parent.info.offset + pane.page, parent.info.total)).arg(parent.info.total) : ""
        }
        Button {
            text: qsTr("Next")
            flat: true
            enabled: parent.info && parent.info.offset + pane.page < parent.info.total
            onClicked: backend.requestHistory(parent.kind, parent.key, parent.info.offset + pane.page)
        }
    }

    component EditRow: ItemDelegate {
        required property var modelData
        width: ListView.view ? ListView.view.width : 200
        contentItem: ColumnLayout {
            spacing: 1
            Label {
                text: (pane.level === "recording" ? pane.rel(modelData.at_ms) : pane.shortDate(modelData.at_ms))
                      + "  " + modelData.actor + qsTr("  rev %1→%2").arg(modelData.before_revision).arg(modelData.after_revision)
                font.pointSize: 8
                opacity: 0.7
            }
            Label {
                visible: modelData.removed.length > 0
                text: "− " + JSON.stringify(modelData.removed)
                color: "#b03a2e"
                elide: Text.ElideRight
                Layout.fillWidth: true
                Layout.preferredWidth: 1
            }
            Label {
                visible: modelData.inserted.length > 0
                text: "+ " + JSON.stringify(modelData.inserted)
                color: "#1e8449"
                elide: Text.ElideRight
                Layout.fillWidth: true
                Layout.preferredWidth: 1
            }
        }
    }

    component EventRow: ItemDelegate {
        required property var modelData
        width: ListView.view ? ListView.view.width : 200
        height: visible ? implicitHeight : 0
        visible: pane.keepEvent(modelData)
        onClicked: pane.openEvent(modelData.seq)
        contentItem: ColumnLayout {
            spacing: 2
            RowLayout {
                spacing: 6
                Layout.fillWidth: true
                Label {
                    text: pane.rel(modelData.wall_ms)
                    font.family: "monospace"
                    font.pointSize: 8
                    opacity: 0.7
                    Layout.preferredWidth: 62
                }
                Chip {
                    text: pane.opLabel(modelData.operation_id)
                    kind: pane.opKind(modelData.operation_id)
                }
                Label {
                    text: pane.arrow(modelData.direction) + " " + pane.eventName(modelData.kind)
                    font.pointSize: 8
                    opacity: 0.8
                    elide: Text.ElideRight
                    Layout.fillWidth: true
                    Layout.preferredWidth: 1
                }
            }
            Label {
                text: modelData.summary || ""
                elide: Text.ElideRight
                Layout.fillWidth: true
                Layout.preferredWidth: 1
                leftPadding: 68
            }
        }
    }

    component Code: ScrollView {
        id: codeView
        property string text
        TextArea {
            readOnly: true
            selectByMouse: true
            font.family: "monospace"
            wrapMode: TextEdit.WrapAtWordBoundaryOrAnywhere
            text: codeView.text
        }
    }

    // ----- Layout -----

    ColumnLayout {
        anchors.fill: parent
        spacing: 0

        // Breadcrumbs and live follow.
        RowLayout {
            Layout.fillWidth: true
            Layout.margins: 4
            spacing: 4
            Flow {
                Layout.fillWidth: true
                Layout.preferredWidth: 1
                spacing: 2
                Button {
                    id: crumbDoc
                    text: qsTr("Document")
                    flat: true
                    font.bold: pane.level === "document"
                    onClicked: { pane.level = "document"; pane.loadDocument() }
                }
                Label { text: "›"; visible: pane.level !== "document"; height: crumbDoc.height; verticalAlignment: Text.AlignVCenter }
                Button {
                    visible: pane.level !== "document"
                    text: pane.recording ? qsTr("Recording %1").arg(new Date(pane.recording.started_at).toLocaleTimeString(Qt.locale(), Locale.ShortFormat)) : qsTr("Recording")
                    flat: true
                    font.bold: pane.level === "recording"
                    onClicked: pane.openRecording(pane.recordingId)
                }
                Label { text: "›"; visible: pane.level === "operation"; height: crumbDoc.height; verticalAlignment: Text.AlignVCenter }
                Button {
                    visible: pane.level === "operation"
                    text: pane.opLabel(pane.operationId)
                    flat: true
                    font.bold: true
                }
                Label { text: "›"; visible: pane.level === "event"; height: crumbDoc.height; verticalAlignment: Text.AlignVCenter }
                Button {
                    visible: pane.level === "event"
                    text: pane.eventDetail ? qsTr("Event %1").arg(pane.eventDetail.app_seq) : qsTr("Event")
                    flat: true
                    font.bold: true
                }
            }
            Switch {
                objectName: "liveFollow"
                text: qsTr("Live")
                checked: backend.inspectorLive
                Layout.alignment: Qt.AlignTop
                onToggled: {
                    backend.setInspectorLiveFollow(checked)
                    if (checked)
                        liveTimer.restart()
                }
                ToolTip.visible: hovered
                ToolTip.text: qsTr("Show new history as it arrives")
            }
        }

        // ----- Document -----
        ColumnLayout {
            visible: pane.level === "document"
            Layout.fillWidth: true
            Layout.fillHeight: true
            Tabs {
                id: docTabs
                labels: [qsTr("Recordings (%1)").arg(pane.docRecordings.length), qsTr("Edits (%1)").arg(pane.docEdits.total || 0), qsTr("Gaps (%1)").arg(pane.gaps.length)]
            }
            StackLayout {
                currentIndex: docTabs.currentIndex
                Layout.fillWidth: true
                Layout.fillHeight: true
                ListView {
                    objectName: "recordingList"
                    clip: true
                    model: pane.docRecordings
                    ScrollBar.vertical: ScrollBar {}
                    delegate: ItemDelegate {
                        required property var modelData
                        width: ListView.view.width
                        onClicked: pane.openRecording(modelData.id)
                        contentItem: ColumnLayout {
                            spacing: 1
                            Label {
                                text: pane.shortDate(modelData.started_at) + "  ·  " + modelData.status
                                font.bold: true
                                wrapMode: Text.WordWrap
                                Layout.fillWidth: true
                                Layout.preferredWidth: 1
                            }
                            Label {
                                text: [pane.duration(modelData), qsTr("%1 operations").arg(modelData.operations),
                                       modelData.audio_status !== "none" ? qsTr("audio %1").arg(modelData.audio_status) : ""]
                                      .filter(x => x).join("  ·  ")
                                opacity: 0.7
                                wrapMode: Text.WordWrap
                                Layout.fillWidth: true
                                Layout.preferredWidth: 1
                            }
                            Label {
                                visible: !!modelData.note
                                text: modelData.note || ""
                                wrapMode: Text.WordWrap
                                font.pointSize: 8
                                Layout.fillWidth: true
                                Layout.preferredWidth: 1
                            }
                        }
                    }
                    Label {
                        anchors.centerIn: parent
                        visible: parent.count === 0
                        text: qsTr("No recordings yet")
                        opacity: 0.6
                    }
                }
                ColumnLayout {
                    Pager { info: pane.docEdits; kind: "edits"; key: backend.currentDocumentId }
                    ListView {
                        clip: true
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        model: pane.docEdits.rows || []
                        delegate: EditRow {}
                        ScrollBar.vertical: ScrollBar {}
                    }
                }
                ListView {
                    clip: true
                    model: pane.gaps
                    delegate: Label {
                        required property var modelData
                        width: ListView.view.width
                        padding: 6
                        wrapMode: Text.WordWrap
                        text: qsTr("%1: %2 history events not saved (%3)").arg(pane.shortDate(modelData.at_ms))
                              .arg(modelData.dropped).arg(modelData.note || "")
                    }
                    Label {
                        anchors.centerIn: parent
                        visible: parent.count === 0
                        text: qsTr("No history was lost")
                        opacity: 0.6
                    }
                }
            }
        }

        // ----- Recording -----
        ColumnLayout {
            visible: pane.level === "recording"
            Layout.fillWidth: true
            Layout.fillHeight: true
            ColumnLayout {
                Layout.margins: 6
                Layout.fillWidth: true
                spacing: 2
                Field { name: qsTr("Status"); value: pane.recording ? pane.recording.status : "" }
                Field { name: qsTr("Started"); value: pane.recording ? pane.shortDate(pane.recording.started_at) : "" }
                Field { name: qsTr("Duration"); value: pane.duration(pane.recording) }
                Field { name: qsTr("Microphone"); value: pane.recording && pane.recording.device ? pane.recording.device : "" }
                Field {
                    name: qsTr("Audio")
                    value: !pane.recording ? ""
                        : pane.recording.audio_status === "none" ? qsTr("not kept")
                        : pane.recording.audio_status + (pane.recording.audio_path ? ": " + pane.recording.audio_path : "")
                          + (pane.recording.audioExists === false ? qsTr(" (file missing)") : "")
                          + (pane.recording.samples ? qsTr(", %1 s").arg((pane.recording.samples / 16000).toFixed(1)) : "")
                }
                Field { name: qsTr("Note"); value: pane.recording && pane.recording.note ? pane.recording.note : "" }
                Field { name: qsTr("Measured"); value: pane.metricsText(pane.recording) }
            }
            Tabs {
                id: recTabs
                labels: [qsTr("Timeline"), qsTr("Operations (%1)").arg(pane.operations.length), qsTr("Transcript"), qsTr("Edits"), qsTr("Settings")]
            }
            StackLayout {
                currentIndex: recTabs.currentIndex
                Layout.fillWidth: true
                Layout.fillHeight: true
                ColumnLayout {
                    RowLayout {
                        Layout.fillWidth: true
                        Layout.leftMargin: 6
                        Layout.rightMargin: 6
                        ComboBox {
                            objectName: "timelineFilter"
                            model: [qsTr("Everything"), qsTr("Without interim results"), qsTr("Edits only"), qsTr("Recognition only")]
                            readonly property var values: ["all", "final", "edits", "recognition"]
                            currentIndex: values.indexOf(pane.timelineFilter)
                            onActivated: (i) => pane.timelineFilter = values[i]
                            Layout.fillWidth: true
                            Layout.preferredWidth: 1
                        }
                        Label {
                            text: qsTr("→ sent  ← received  • app")
                            font.pointSize: 8
                            opacity: 0.6
                        }
                    }
                    Pager { info: pane.events; kind: "events"; key: pane.recordingId }
                    ListView {
                        id: timeline
                        objectName: "timeline"
                        clip: true
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        model: pane.events.rows || []
                        delegate: EventRow {}
                        ScrollBar.vertical: ScrollBar {}
                        onCountChanged: if (backend.inspectorLive) positionViewAtEnd()
                    }
                }
                ListView {
                    clip: true
                    model: pane.operations
                    ScrollBar.vertical: ScrollBar {}
                    delegate: ItemDelegate {
                        required property var modelData
                        width: ListView.view.width
                        onClicked: pane.openOperation(modelData.id)
                        contentItem: ColumnLayout {
                            spacing: 2
                            RowLayout {
                                spacing: 6
                                Chip {
                                    text: pane.opLabel(modelData.id)
                                    kind: pane.opKind(modelData.id)
                                }
                                Label {
                                    text: [modelData.status, pane.rel(modelData.started_at),
                                           modelData.latency_ms ? pane.ms(modelData.latency_ms) : ""]
                                          .filter(x => x).join("  ·  ")
                                    elide: Text.ElideRight
                                    Layout.fillWidth: true
                                    Layout.preferredWidth: 1
                                }
                            }
                            Label {
                                text: modelData.outcome || modelData.error || ""
                                visible: text.length > 0
                                wrapMode: Text.WordWrap
                                maximumLineCount: 2
                                elide: Text.ElideRight
                                Layout.fillWidth: true
                                Layout.preferredWidth: 1
                                opacity: 0.8
                            }
                        }
                    }
                }
                ListView {
                    clip: true
                    model: pane.transcripts
                    ScrollBar.vertical: ScrollBar {}
                    delegate: Label {
                        required property var modelData
                        width: ListView.view.width
                        padding: 4
                        wrapMode: Text.WordWrap
                        opacity: modelData.finality === "interim" ? 0.6 : 1
                        text: "[" + pane.seconds(modelData.audio_start) + "–" + pane.seconds(modelData.audio_end) + "] "
                              + modelData.finality + ": " + modelData.text
                    }
                }
                ColumnLayout {
                    Pager { info: pane.recEdits; kind: "recordingEdits"; key: backend.currentDocumentId + "|" + pane.recordingId }
                    ListView {
                        clip: true
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        model: pane.recEdits.rows || []
                        delegate: EditRow {}
                        ScrollBar.vertical: ScrollBar {}
                    }
                }
                Code {
                    text: pane.recording && pane.recording.settings
                          ? qsTr("Application %1, prompt %2\n\n").arg(pane.recording.app_version).arg(pane.recording.prompt_version)
                            + pane.recording.settings : ""
                }
            }
        }

        // ----- Operation -----
        ColumnLayout {
            visible: pane.level === "operation"
            Layout.fillWidth: true
            Layout.fillHeight: true
            ColumnLayout {
                Layout.margins: 6
                Layout.fillWidth: true
                spacing: 2
                Field { name: qsTr("Provider"); value: pane.operation ? [pane.operation.provider, pane.operation.model].filter(x => x).join(", ") : "" }
                Field { name: qsTr("Status"); value: pane.operation ? pane.operation.status : "" }
                Field { name: qsTr("Outcome"); value: pane.operation && pane.operation.outcome ? pane.operation.outcome : "" }
                Field { name: qsTr("Error"); value: pane.operation && pane.operation.error ? pane.operation.error : "" }
                Field { name: qsTr("Started"); value: pane.operation ? pane.rel(pane.operation.started_at) : "" }
                Field { name: qsTr("Latency"); value: pane.operation && pane.operation.latency_ms ? pane.ms(pane.operation.latency_ms) : "" }
                Field { name: qsTr("Tokens"); value: pane.operation ? pane.usageText(pane.operation.usage) : "" }
                Field { name: qsTr("Request ID"); value: pane.operation && pane.operation.request_id ? pane.operation.request_id : "" }
            }
            Tabs {
                id: opTabs
                labels: [qsTr("Request"), qsTr("Response"), qsTr("Events")]
            }
            StackLayout {
                currentIndex: opTabs.currentIndex
                Layout.fillWidth: true
                Layout.fillHeight: true
                Code { text: pane.operation ? pane.pretty(pane.operation.request) : "" }
                Code { text: pane.operation ? pane.pretty(pane.operation.response) : "" }
                ColumnLayout {
                    Pager { info: pane.opEvents; kind: "operationEvents"; key: pane.operationId }
                    ListView {
                        clip: true
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        model: pane.opEvents.rows || []
                        delegate: EventRow {}
                        ScrollBar.vertical: ScrollBar {}
                    }
                }
            }
        }

        // ----- Event -----
        ColumnLayout {
            visible: pane.level === "event"
            Layout.fillWidth: true
            Layout.fillHeight: true
            ColumnLayout {
                Layout.margins: 6
                Layout.fillWidth: true
                spacing: 2
                RowLayout {
                    visible: !!pane.eventDetail
                    spacing: 6
                    Chip {
                        text: pane.eventDetail ? pane.opLabel(pane.eventDetail.operation_id) : ""
                        kind: pane.eventDetail ? pane.opKind(pane.eventDetail.operation_id) : ""
                    }
                    Label {
                        text: pane.eventDetail ? pane.arrow(pane.eventDetail.direction) + " " + pane.eventName(pane.eventDetail.kind) : ""
                    }
                }
                Field { name: qsTr("Time"); value: pane.eventDetail ? pane.rel(pane.eventDetail.wall_ms) + qsTr(" (%1)").arg(pane.clock(pane.eventDetail.wall_ms)) : "" }
                Field {
                    name: qsTr("Audio")
                    value: pane.eventDetail && pane.eventDetail.audio_start !== null
                           ? pane.seconds(pane.eventDetail.audio_start) + " – " + pane.seconds(pane.eventDetail.audio_end) : ""
                }
                Field { name: qsTr("Summary"); value: pane.eventDetail && pane.eventDetail.summary ? pane.eventDetail.summary : "" }
                Button {
                    visible: pane.eventDetail && !!pane.eventDetail.operation_id
                    text: qsTr("Open %1").arg(pane.eventDetail ? pane.opLabel(pane.eventDetail.operation_id) : "")
                    flat: true
                    onClicked: pane.openOperation(pane.eventDetail.operation_id)
                }
            }
            Tabs {
                id: evTabs
                labels: [qsTr("Raw"), qsTr("Parsed")]
            }
            StackLayout {
                currentIndex: evTabs.currentIndex
                Layout.fillWidth: true
                Layout.fillHeight: true
                Code { text: pane.eventDetail ? pane.pretty(pane.eventDetail.payload) : "" }
                Code { text: pane.eventDetail ? pane.pretty(pane.eventDetail.normalized) : "" }
            }
        }
    }
}
