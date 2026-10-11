//! SQLite storage: documents (product data, saved reliably) and diagnostic history
//! (best-effort).
//!
//! Writes go through one background writer thread so nothing waits for the disk. Document
//! saves are never dropped; a failure is reported and the caller retries. History items are
//! dropped when the writer's queue is over its limit or the database fails; dropped items are
//! counted and recorded as a gap. Reads use a separate connection (WAL mode allows both).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::Value;

use crate::editor::{EditRecord, ProvenanceSpan};

pub const SCHEMA_VERSION: i64 = 1;
/// Version of the event payload format recorded with each event.
pub const EVENT_SCHEMA_VERSION: i64 = 1;

const MIGRATION_1: &str = "
CREATE TABLE documents (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    text TEXT NOT NULL,
    revision INTEGER NOT NULL,
    provenance TEXT NOT NULL DEFAULT '[]',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE settings_snapshots (
    id TEXT PRIMARY KEY,
    created_at INTEGER NOT NULL,
    app_version TEXT NOT NULL,
    prompt_version TEXT NOT NULL,
    json TEXT NOT NULL
);
CREATE TABLE recordings (
    id TEXT PRIMARY KEY,
    document_id TEXT NOT NULL,
    status TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    ended_at INTEGER,
    settings_snapshot_id TEXT,
    initial_position INTEGER,
    device TEXT,
    audio_path TEXT,
    audio_status TEXT NOT NULL DEFAULT 'none',
    sample_rate INTEGER,
    channels INTEGER,
    samples INTEGER NOT NULL DEFAULT 0,
    note TEXT,
    metrics TEXT
);
CREATE INDEX recordings_document ON recordings(document_id, started_at);
CREATE TABLE operations (
    id TEXT PRIMARY KEY,
    document_id TEXT NOT NULL,
    recording_id TEXT,
    parent_id TEXT,
    attempt INTEGER NOT NULL DEFAULT 1,
    kind TEXT NOT NULL,
    provider TEXT,
    model TEXT,
    request_id TEXT,
    status TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    ended_at INTEGER,
    region_revision INTEGER,
    source_watermark INTEGER,
    settings_snapshot_id TEXT,
    request TEXT,
    response TEXT,
    usage TEXT,
    outcome TEXT,
    error TEXT,
    latency_ms INTEGER
);
CREATE INDEX operations_recording ON operations(recording_id, started_at);
CREATE TABLE events (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    app_seq INTEGER NOT NULL,
    document_id TEXT,
    recording_id TEXT,
    operation_id TEXT,
    schema_version INTEGER NOT NULL,
    direction TEXT NOT NULL,
    kind TEXT NOT NULL,
    wall_ms INTEGER NOT NULL,
    mono_ms INTEGER NOT NULL,
    summary TEXT,
    payload TEXT,
    normalized TEXT,
    audio_start INTEGER,
    audio_end INTEGER
);
CREATE INDEX events_recording ON events(recording_id, app_seq);
CREATE INDEX events_operation ON events(operation_id, app_seq);
CREATE TABLE transcript_revisions (
    id INTEGER PRIMARY KEY,
    recording_id TEXT NOT NULL,
    segment_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    event_app_seq INTEGER,
    epoch INTEGER,
    text TEXT NOT NULL,
    finality TEXT NOT NULL,
    audio_start INTEGER,
    audio_end INTEGER,
    confidence REAL,
    words TEXT
);
CREATE INDEX transcripts_recording ON transcript_revisions(recording_id, id);
CREATE TABLE document_edits (
    id INTEGER PRIMARY KEY,
    document_id TEXT NOT NULL,
    recording_id TEXT,
    actor TEXT NOT NULL,
    before_revision INTEGER NOT NULL,
    after_revision INTEGER NOT NULL,
    position INTEGER NOT NULL,
    removed TEXT NOT NULL,
    inserted TEXT NOT NULL,
    undo_group INTEGER,
    source TEXT,
    at_ms INTEGER NOT NULL
);
CREATE INDEX edits_document ON document_edits(document_id, id);
CREATE TABLE gaps (
    id INTEGER PRIMARY KEY,
    at_ms INTEGER NOT NULL,
    dropped INTEGER NOT NULL,
    note TEXT
);
CREATE TABLE preferences (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// Milliseconds on a monotonic clock since the process started.
pub fn mono_ms() -> i64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as i64
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct StorageError(pub String);

impl From<rusqlite::Error> for StorageError {
    fn from(e: rusqlite::Error) -> Self {
        Self(e.to_string())
    }
}

fn open(path: &Path) -> Result<Connection, StorageError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| StorageError(e.to_string()))?;
    }
    let conn = Connection::open(path)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(conn)
}

fn migrate(conn: &Connection) -> Result<(), StorageError> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version < 1 {
        conn.execute_batch(&format!(
            "BEGIN; {MIGRATION_1} PRAGMA user_version = 1; COMMIT;"
        ))?;
    }
    if version < 2 {
        // The file a document was last saved to.
        conn.execute_batch(
            "BEGIN; ALTER TABLE documents ADD COLUMN file_path TEXT; \
             PRAGMA user_version = 2; COMMIT;",
        )?;
    }
    Ok(())
}

// ----- Rows written to history -----

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RecordingRow {
    pub id: String,
    pub document_id: String,
    pub status: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub settings_snapshot_id: Option<String>,
    pub initial_position: Option<i64>,
    pub device: Option<String>,
    pub audio_path: Option<String>,
    pub audio_status: String,
    pub sample_rate: Option<i64>,
    pub channels: Option<i64>,
    pub samples: i64,
    pub note: Option<String>,
    /// JSON latency and outcome measurements, written when the recording ends.
    pub metrics: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct OperationRow {
    pub id: String,
    pub document_id: String,
    pub recording_id: Option<String>,
    pub parent_id: Option<String>,
    pub attempt: i64,
    pub kind: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub request_id: Option<String>,
    pub status: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub region_revision: Option<i64>,
    pub source_watermark: Option<i64>,
    pub settings_snapshot_id: Option<String>,
    pub request: Option<String>,
    pub response: Option<String>,
    pub usage: Option<String>,
    pub outcome: Option<String>,
    pub error: Option<String>,
    pub latency_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct EventRow {
    pub app_seq: i64,
    pub document_id: Option<String>,
    pub recording_id: Option<String>,
    pub operation_id: Option<String>,
    /// `in` (from a provider), `out` (to a provider) or `local`.
    pub direction: String,
    pub kind: String,
    pub wall_ms: i64,
    pub mono_ms: i64,
    /// One line for lists.
    pub summary: Option<String>,
    pub payload: Option<String>,
    pub normalized: Option<String>,
    pub audio_start: Option<i64>,
    pub audio_end: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptRow {
    pub recording_id: String,
    pub segment_id: String,
    pub revision: i64,
    pub event_app_seq: Option<i64>,
    pub epoch: Option<i64>,
    pub text: String,
    pub finality: String,
    pub audio_start: Option<i64>,
    pub audio_end: Option<i64>,
    pub confidence: Option<f64>,
    pub words: Option<String>,
}

#[derive(Debug, Clone)]
pub enum HistoryItem {
    Recording(RecordingRow),
    Operation(OperationRow),
    Event(EventRow),
    Transcript(TranscriptRow),
    Edits {
        document_id: String,
        records: Vec<EditRecord>,
    },
    Snapshot {
        id: String,
        json: String,
    },
}

/// Allocates application sequence numbers (the order events were processed in).
pub struct Sequencer(AtomicU64);

impl Sequencer {
    pub fn next(&self) -> i64 {
        self.0.fetch_add(1, Ordering::Relaxed) as i64
    }
}

#[derive(Debug, Clone)]
pub enum WriterNotice {
    DocumentSaved { id: String, revision: u64 },
    DocumentSaveFailed { id: String, message: String },
    HistoryGap { dropped: usize, reason: String },
    HistoryWritten,
}

enum Op {
    SaveDocument {
        id: String,
        text: String,
        revision: u64,
        provenance: String,
    },
    CreateDocument {
        id: String,
        title: String,
        text: String,
    },
    RenameDocument {
        id: String,
        title: String,
    },
    SetDocumentFile {
        id: String,
        path: String,
    },
    DeleteDocument {
        id: String,
    },
    SetPreference {
        key: String,
        value: Option<String>,
    },
    UpdateAudio {
        recording_id: String,
        status: String,
        samples: Option<i64>,
    },
    History(Box<HistoryItem>),
    Flush(Sender<()>),
}

type NoticeFn = Arc<dyn Fn(WriterNotice) + Send + Sync>;

struct Shared {
    tx: Mutex<Option<Sender<Op>>>,
    pending_history: AtomicUsize,
    dropped: AtomicUsize,
    queue_limit: AtomicUsize,
    /// Test hook: fail every document save.
    fail_saves: std::sync::atomic::AtomicBool,
    batch_ms: AtomicU64,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

/// Handle to the database. Cheap to clone.
#[derive(Clone)]
pub struct Storage {
    shared: Arc<Shared>,
    reader: Arc<Mutex<Connection>>,
    pub sequencer: Arc<Sequencer>,
    path: PathBuf,
}

impl Storage {
    /// Opens (creating and migrating) the database and starts the writer thread. Recordings left
    /// active by a previous run are marked interrupted.
    pub fn open(path: &Path, on_notice: NoticeFn) -> Result<Self, StorageError> {
        let writer_conn = open(path)?;
        migrate(&writer_conn)?;
        let interrupted = writer_conn.execute(
            "UPDATE recordings SET status = 'interrupted',
                 note = COALESCE(note || ' ', '') || 'The application stopped during this recording.'
             WHERE status IN ('recording', 'finishing')",
            [],
        )?;
        writer_conn.execute(
            "UPDATE operations SET status = 'interrupted' WHERE status IN ('pending', 'running')",
            [],
        )?;
        let _ = interrupted;
        let max_seq: i64 =
            writer_conn.query_row("SELECT COALESCE(MAX(app_seq), 0) FROM events", [], |r| {
                r.get(0)
            })?;
        let reader = open(path)?;
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Shared {
            tx: Mutex::new(Some(tx)),
            pending_history: AtomicUsize::new(0),
            dropped: AtomicUsize::new(0),
            queue_limit: AtomicUsize::new(20_000),
            fail_saves: std::sync::atomic::AtomicBool::new(false),
            batch_ms: AtomicU64::new(250),
            thread: Mutex::new(None),
        });
        let thread_shared = shared.clone();
        let handle = std::thread::Builder::new()
            .name("batscribe-db".into())
            .spawn(move || writer_loop(writer_conn, rx, thread_shared, on_notice))
            .map_err(|e| StorageError(e.to_string()))?;
        *shared.thread.lock().unwrap() = Some(handle);
        Ok(Self {
            shared,
            reader: Arc::new(Mutex::new(reader)),
            sequencer: Arc::new(Sequencer(AtomicU64::new(max_seq as u64 + 1))),
            path: path.to_owned(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Test hook: makes every document save fail (as a full disk would) until turned off.
    pub fn inject_save_failures(&self, fail: bool) {
        self.shared.fail_saves.store(fail, Ordering::Relaxed);
    }

    pub fn configure(&self, queue_limit: usize, batch_ms: u64) {
        self.shared
            .queue_limit
            .store(queue_limit.max(1), Ordering::Relaxed);
        self.shared
            .batch_ms
            .store(batch_ms.max(1), Ordering::Relaxed);
    }

    fn send(&self, op: Op) -> bool {
        match self.shared.tx.lock().unwrap().as_ref() {
            Some(tx) => tx.send(op).is_ok(),
            None => false,
        }
    }

    /// Queues a document save. Never dropped; failures arrive as `DocumentSaveFailed`.
    pub fn save_document(
        &self,
        id: &str,
        text: &str,
        revision: u64,
        provenance: &[ProvenanceSpan],
    ) {
        self.send(Op::SaveDocument {
            id: id.to_owned(),
            text: text.to_owned(),
            revision,
            provenance: serde_json::to_string(provenance).unwrap_or_else(|_| "[]".into()),
        });
    }

    pub fn create_document(&self, id: &str, title: &str, text: &str) {
        self.send(Op::CreateDocument {
            id: id.to_owned(),
            title: title.to_owned(),
            text: text.to_owned(),
        });
    }

    pub fn rename_document(&self, id: &str, title: &str) {
        self.send(Op::RenameDocument {
            id: id.to_owned(),
            title: title.to_owned(),
        });
    }

    /// Remembers the file a document was saved to.
    pub fn set_document_file(&self, id: &str, path: &str) {
        self.send(Op::SetDocumentFile {
            id: id.to_owned(),
            path: path.to_owned(),
        });
    }

    /// Deletes a document and its history. Retained audio files are left alone.
    pub fn delete_document(&self, id: &str) {
        self.send(Op::DeleteDocument { id: id.to_owned() });
    }

    pub fn set_preference(&self, key: &str, value: Option<&Value>) {
        self.send(Op::SetPreference {
            key: key.to_owned(),
            value: value.map(Value::to_string),
        });
    }

    /// Records the reconciled state of a recording's audio file.
    pub fn update_audio(&self, recording_id: &str, status: &str, samples: Option<i64>) {
        self.send(Op::UpdateAudio {
            recording_id: recording_id.to_owned(),
            status: status.to_owned(),
            samples,
        });
    }

    /// Recordings whose audio file was still being written when the application stopped.
    pub fn unreconciled_audio(&self) -> Result<Vec<(String, String)>, StorageError> {
        self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT id, audio_path FROM recordings
                 WHERE audio_path IS NOT NULL AND audio_status = 'recording'
                   AND status NOT IN ('recording', 'finishing')",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect()
        })
    }

    /// Queues a history item, or counts it as dropped when the queue is full.
    pub fn history(&self, item: HistoryItem) {
        let limit = self.shared.queue_limit.load(Ordering::Relaxed);
        if self.shared.pending_history.load(Ordering::Relaxed) >= limit {
            self.shared.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.shared.pending_history.fetch_add(1, Ordering::Relaxed);
        if !self.send(Op::History(Box::new(item))) {
            self.shared.pending_history.fetch_sub(1, Ordering::Relaxed);
            self.shared.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn event(&self, mut row: EventRow) -> i64 {
        if row.app_seq == 0 {
            row.app_seq = self.sequencer.next();
        }
        if row.wall_ms == 0 {
            row.wall_ms = now_ms();
            row.mono_ms = mono_ms();
        }
        let seq = row.app_seq;
        self.history(HistoryItem::Event(row));
        seq
    }

    /// Blocks until everything queued so far is written (or `timeout` passes).
    pub fn flush(&self, timeout: Duration) -> bool {
        let (tx, rx) = mpsc::channel();
        self.send(Op::Flush(tx)) && rx.recv_timeout(timeout).is_ok()
    }

    /// Flushes and stops the writer.
    pub fn close(&self, timeout: Duration) {
        self.flush(timeout);
        self.shared.tx.lock().unwrap().take();
        if let Some(handle) = self.shared.thread.lock().unwrap().take() {
            let _ = handle.join();
        }
    }

    fn read<T>(
        &self,
        f: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> Result<T, StorageError> {
        let conn = self.reader.lock().unwrap_or_else(|e| e.into_inner());
        f(&conn).map_err(StorageError::from)
    }
}

fn writer_loop(mut conn: Connection, rx: Receiver<Op>, shared: Arc<Shared>, notice: NoticeFn) {
    let mut history: Vec<HistoryItem> = Vec::new();
    let mut flushes: Vec<Sender<()>> = Vec::new();
    let mut deadline: Option<Instant> = None;
    loop {
        let timeout = deadline.map_or(Duration::from_secs(3600), |d| {
            d.saturating_duration_since(Instant::now())
        });
        match rx.recv_timeout(timeout) {
            Ok(Op::History(item)) => {
                history.push(*item);
                if deadline.is_none() {
                    let ms = shared.batch_ms.load(Ordering::Relaxed);
                    deadline = Some(Instant::now() + Duration::from_millis(ms));
                }
                continue;
            }
            Ok(Op::Flush(done)) => flushes.push(done),
            Ok(op) => {
                // Documents and preferences are written right away, in their own transaction.
                let saved = match &op {
                    Op::SaveDocument { id, revision, .. } => Some((id.clone(), *revision)),
                    _ => None,
                };
                let result = if saved.is_some() && shared.fail_saves.load(Ordering::Relaxed) {
                    Err((
                        saved.clone().unwrap_or_default().0,
                        "injected failure".to_owned(),
                    ))
                } else {
                    write_reliable(&mut conn, op)
                };
                match (result, saved) {
                    (Err((id, message)), _) => {
                        notice(WriterNotice::DocumentSaveFailed { id, message })
                    }
                    (Ok(()), Some((id, revision))) => {
                        notice(WriterNotice::DocumentSaved { id, revision })
                    }
                    (Ok(()), None) => {}
                }
                continue;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                write_history(&mut conn, &mut history, &shared, &notice);
                return;
            }
        }
        write_history(&mut conn, &mut history, &shared, &notice);
        deadline = None;
        for done in flushes.drain(..) {
            let _ = done.send(());
        }
    }
}

fn write_reliable(conn: &mut Connection, op: Op) -> Result<(), (String, String)> {
    let now = now_ms();
    let (id, result) = match op {
        Op::SaveDocument {
            id,
            text,
            revision,
            provenance,
        } => {
            let r = conn.execute(
                "UPDATE documents SET text = ?2, revision = ?3, provenance = ?4, updated_at = ?5
                 WHERE id = ?1",
                params![id, text, revision as i64, provenance, now],
            );
            (id, r.map(|_| ()))
        }
        Op::CreateDocument { id, title, text } => {
            let r = conn.execute(
                "INSERT INTO documents (id, title, text, revision, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 0, ?4, ?4)",
                params![id, title, text, now],
            );
            (id, r.map(|_| ()))
        }
        Op::RenameDocument { id, title } => {
            let r = conn.execute(
                "UPDATE documents SET title = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, title, now],
            );
            (id, r.map(|_| ()))
        }
        Op::SetDocumentFile { id, path } => {
            let r = conn.execute(
                "UPDATE documents SET file_path = ?2 WHERE id = ?1",
                params![id, path],
            );
            (id, r.map(|_| ()))
        }
        Op::DeleteDocument { id } => {
            let r = (|| {
                let tx = conn.transaction()?;
                for sql in [
                    "DELETE FROM events WHERE document_id = ?1",
                    "DELETE FROM transcript_revisions WHERE recording_id IN
                         (SELECT id FROM recordings WHERE document_id = ?1)",
                    "DELETE FROM operations WHERE document_id = ?1",
                    "DELETE FROM document_edits WHERE document_id = ?1",
                    "DELETE FROM recordings WHERE document_id = ?1",
                    "DELETE FROM documents WHERE id = ?1",
                ] {
                    tx.execute(sql, params![id])?;
                }
                tx.commit()
            })();
            (id, r)
        }
        Op::SetPreference { key, value } => {
            let r = match value {
                Some(v) => conn.execute(
                    "INSERT INTO preferences (key, value) VALUES (?1, ?2)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![key, v],
                ),
                None => conn.execute("DELETE FROM preferences WHERE key = ?1", params![key]),
            };
            (key, r.map(|_| ()))
        }
        Op::UpdateAudio {
            recording_id,
            status,
            samples,
        } => {
            let r = conn.execute(
                "UPDATE recordings SET audio_status = ?2, samples = COALESCE(?3, samples)
                 WHERE id = ?1",
                params![recording_id, status, samples],
            );
            (recording_id, r.map(|_| ()))
        }
        Op::History(_) | Op::Flush(_) => return Ok(()),
    };
    result.map_err(|e| (id, e.to_string()))
}

fn write_history(
    conn: &mut Connection,
    items: &mut Vec<HistoryItem>,
    shared: &Shared,
    notice: &NoticeFn,
) {
    let dropped_before = shared.dropped.swap(0, Ordering::Relaxed);
    if items.is_empty() && dropped_before == 0 {
        return;
    }
    let count = items.len();
    let result = (|| -> rusqlite::Result<()> {
        let tx = conn.transaction()?;
        if dropped_before > 0 {
            tx.execute(
                "INSERT INTO gaps (at_ms, dropped, note) VALUES (?1, ?2, ?3)",
                params![now_ms(), dropped_before as i64, "history queue full"],
            )?;
        }
        for item in items.iter() {
            write_item(&tx, item)?;
        }
        tx.commit()
    })();
    items.clear();
    shared.pending_history.fetch_sub(count, Ordering::Relaxed);
    match result {
        Ok(()) => {
            if dropped_before > 0 {
                notice(WriterNotice::HistoryGap {
                    dropped: dropped_before,
                    reason: "the history queue was full".into(),
                });
            }
            notice(WriterNotice::HistoryWritten);
        }
        Err(e) => {
            let dropped = count + dropped_before;
            let _ = conn.execute(
                "INSERT INTO gaps (at_ms, dropped, note) VALUES (?1, ?2, ?3)",
                params![now_ms(), dropped as i64, e.to_string()],
            );
            notice(WriterNotice::HistoryGap {
                dropped,
                reason: e.to_string(),
            });
        }
    }
}

fn write_item(tx: &rusqlite::Transaction<'_>, item: &HistoryItem) -> rusqlite::Result<()> {
    match item {
        HistoryItem::Recording(r) => {
            tx.execute(
                "INSERT INTO recordings (id, document_id, status, started_at, ended_at,
                     settings_snapshot_id, initial_position, device, audio_path, audio_status,
                     sample_rate, channels, samples, note, metrics)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
                 ON CONFLICT(id) DO UPDATE SET status = excluded.status,
                     ended_at = excluded.ended_at, device = excluded.device,
                     audio_path = excluded.audio_path, audio_status = excluded.audio_status,
                     sample_rate = excluded.sample_rate, channels = excluded.channels,
                     samples = excluded.samples, note = excluded.note,
                     metrics = COALESCE(excluded.metrics, metrics)",
                params![
                    r.id,
                    r.document_id,
                    r.status,
                    r.started_at,
                    r.ended_at,
                    r.settings_snapshot_id,
                    r.initial_position,
                    r.device,
                    r.audio_path,
                    r.audio_status,
                    r.sample_rate,
                    r.channels,
                    r.samples,
                    r.note,
                    r.metrics
                ],
            )?;
        }
        HistoryItem::Operation(o) => {
            tx.execute(
                "INSERT INTO operations (id, document_id, recording_id, parent_id, attempt, kind,
                     provider, model, request_id, status, started_at, ended_at, region_revision,
                     source_watermark, settings_snapshot_id, request, response, usage, outcome,
                     error, latency_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                     ?17, ?18, ?19, ?20, ?21)
                 ON CONFLICT(id) DO UPDATE SET request_id = COALESCE(excluded.request_id, request_id),
                     status = excluded.status, ended_at = excluded.ended_at,
                     request = COALESCE(excluded.request, request),
                     response = COALESCE(excluded.response, response),
                     usage = COALESCE(excluded.usage, usage), outcome = excluded.outcome,
                     error = excluded.error, latency_ms = excluded.latency_ms",
                params![
                    o.id, o.document_id, o.recording_id, o.parent_id, o.attempt, o.kind,
                    o.provider, o.model, o.request_id, o.status, o.started_at, o.ended_at,
                    o.region_revision, o.source_watermark, o.settings_snapshot_id, o.request,
                    o.response, o.usage, o.outcome, o.error, o.latency_ms
                ],
            )?;
        }
        HistoryItem::Event(e) => {
            tx.execute(
                "INSERT INTO events (app_seq, document_id, recording_id, operation_id,
                     schema_version, direction, kind, wall_ms, mono_ms, summary, payload,
                     normalized, audio_start, audio_end)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    e.app_seq,
                    e.document_id,
                    e.recording_id,
                    e.operation_id,
                    EVENT_SCHEMA_VERSION,
                    e.direction,
                    e.kind,
                    e.wall_ms,
                    e.mono_ms,
                    e.summary,
                    e.payload,
                    e.normalized,
                    e.audio_start,
                    e.audio_end
                ],
            )?;
        }
        HistoryItem::Transcript(t) => {
            tx.execute(
                "INSERT INTO transcript_revisions (recording_id, segment_id, revision,
                     event_app_seq, epoch, text, finality, audio_start, audio_end, confidence,
                     words)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    t.recording_id,
                    t.segment_id,
                    t.revision,
                    t.event_app_seq,
                    t.epoch,
                    t.text,
                    t.finality,
                    t.audio_start,
                    t.audio_end,
                    t.confidence,
                    t.words
                ],
            )?;
        }
        HistoryItem::Edits {
            document_id,
            records,
        } => {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO document_edits (document_id, recording_id, actor, before_revision,
                     after_revision, position, removed, inserted, undo_group, source, at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )?;
            for r in records {
                stmt.execute(params![
                    document_id,
                    r.recording_id,
                    r.actor.as_str(),
                    r.before_revision as i64,
                    r.after_revision as i64,
                    r.position as i64,
                    r.removed,
                    r.inserted,
                    r.undo_group.map(|g| g as i64),
                    r.source,
                    r.at_ms
                ])?;
            }
        }
        HistoryItem::Snapshot { id, json } => {
            tx.execute(
                "INSERT OR IGNORE INTO settings_snapshots (id, created_at, app_version,
                     prompt_version, json) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    id,
                    now_ms(),
                    env!("CARGO_PKG_VERSION"),
                    crate::settings::PROMPT_VERSION,
                    json
                ],
            )?;
        }
    }
    Ok(())
}

// ----- Reads -----

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentSummary {
    pub id: String,
    pub title: String,
    pub preview: String,
    pub updated_at: i64,
}

#[derive(Debug, Clone)]
pub struct DocumentRecord {
    pub id: String,
    pub title: String,
    pub text: String,
    pub revision: u64,
    pub provenance: Vec<ProvenanceSpan>,
    pub file_path: Option<String>,
}

fn json_rows(
    stmt: &mut rusqlite::Statement<'_>,
    params: impl rusqlite::Params,
) -> rusqlite::Result<Vec<Value>> {
    let names: Vec<String> = stmt
        .column_names()
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let rows = stmt.query_map(params, |row| {
        let mut obj = serde_json::Map::new();
        for (i, name) in names.iter().enumerate() {
            let v = match row.get_ref(i)? {
                rusqlite::types::ValueRef::Null => Value::Null,
                rusqlite::types::ValueRef::Integer(n) => Value::from(n),
                rusqlite::types::ValueRef::Real(f) => Value::from(f),
                rusqlite::types::ValueRef::Text(t) => {
                    Value::from(String::from_utf8_lossy(t).into_owned())
                }
                rusqlite::types::ValueRef::Blob(_) => Value::Null,
            };
            obj.insert(name.clone(), v);
        }
        Ok(Value::Object(obj))
    })?;
    rows.collect()
}

impl Storage {
    pub fn documents(&self) -> Result<Vec<DocumentSummary>, StorageError> {
        self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT id, title, substr(text, 1, 200), updated_at FROM documents
                 ORDER BY updated_at DESC",
            )?;
            let rows = stmt.query_map([], |r| {
                let text: String = r.get(2)?;
                let preview = text
                    .lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty())
                    .unwrap_or("")
                    .chars()
                    .take(80)
                    .collect();
                Ok(DocumentSummary {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    preview,
                    updated_at: r.get(3)?,
                })
            })?;
            rows.collect()
        })
    }

    pub fn document(&self, id: &str) -> Result<Option<DocumentRecord>, StorageError> {
        self.read(|c| {
            c.query_row(
                "SELECT id, title, text, revision, provenance, file_path FROM documents
                 WHERE id = ?1",
                params![id],
                |r| {
                    let provenance: String = r.get(4)?;
                    Ok(DocumentRecord {
                        id: r.get(0)?,
                        title: r.get(1)?,
                        text: r.get(2)?,
                        revision: r.get::<_, i64>(3)? as u64,
                        provenance: serde_json::from_str(&provenance).unwrap_or_default(),
                        file_path: r.get(5)?,
                    })
                },
            )
            .optional()
        })
    }

    pub fn preferences(&self) -> Result<crate::settings::Preferences, StorageError> {
        self.read(|c| {
            let mut stmt = c.prepare("SELECT key, value FROM preferences")?;
            let rows =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            let mut prefs = crate::settings::Preferences::default();
            for row in rows {
                let (k, v) = row?;
                if let Ok(v) = serde_json::from_str(&v) {
                    prefs.stored.insert(k, v);
                }
            }
            Ok(prefs)
        })
    }

    pub fn recordings(&self, document_id: &str) -> Result<Vec<Value>, StorageError> {
        self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT r.*, (SELECT COUNT(*) FROM operations o WHERE o.recording_id = r.id) AS operations
                 FROM recordings r WHERE document_id = ?1 ORDER BY started_at DESC",
            )?;
            json_rows(&mut stmt, params![document_id])
        })
    }

    pub fn recording(&self, id: &str) -> Result<Option<Value>, StorageError> {
        self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT r.*, s.json AS settings, s.app_version, s.prompt_version
                 FROM recordings r LEFT JOIN settings_snapshots s ON s.id = r.settings_snapshot_id
                 WHERE r.id = ?1",
            )?;
            Ok(json_rows(&mut stmt, params![id])?.into_iter().next())
        })
    }

    /// Operations of a recording, without their large payloads.
    pub fn operations(&self, recording_id: &str) -> Result<Vec<Value>, StorageError> {
        self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT id, parent_id, attempt, kind, provider, model, request_id, status,
                     started_at, ended_at, region_revision, source_watermark, usage, outcome,
                     error, latency_ms
                 FROM operations WHERE recording_id = ?1 ORDER BY started_at, rowid",
            )?;
            json_rows(&mut stmt, params![recording_id])
        })
    }

    pub fn operation(&self, id: &str) -> Result<Option<Value>, StorageError> {
        self.read(|c| {
            let mut stmt = c.prepare("SELECT * FROM operations WHERE id = ?1")?;
            Ok(json_rows(&mut stmt, params![id])?.into_iter().next())
        })
    }

    /// A page of events for a recording (or one operation), oldest first, without payloads.
    pub fn events(
        &self,
        recording_id: &str,
        operation_id: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<(Vec<Value>, i64), StorageError> {
        self.read(|c| {
            let (filter, key) = match operation_id {
                Some(op) => ("operation_id = ?1", op),
                None => ("recording_id = ?1", recording_id),
            };
            let total: i64 = c.query_row(
                &format!("SELECT COUNT(*) FROM events WHERE {filter}"),
                params![key],
                |r| r.get(0),
            )?;
            let mut stmt = c.prepare(&format!(
                "SELECT seq, app_seq, operation_id, direction, kind, wall_ms, mono_ms, summary,
                     audio_start, audio_end
                 FROM events WHERE {filter} ORDER BY app_seq LIMIT ?2 OFFSET ?3"
            ))?;
            Ok((json_rows(&mut stmt, params![key, limit, offset])?, total))
        })
    }

    pub fn event_detail(&self, seq: i64) -> Result<Option<Value>, StorageError> {
        self.read(|c| {
            let mut stmt = c.prepare("SELECT * FROM events WHERE seq = ?1")?;
            Ok(json_rows(&mut stmt, params![seq])?.into_iter().next())
        })
    }

    /// A page of document edits, newest first, optionally only those of one recording.
    pub fn edits(
        &self,
        document_id: &str,
        recording_id: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<(Vec<Value>, i64), StorageError> {
        self.read(|c| {
            let filter = "document_id = ?1 AND (?2 IS NULL OR recording_id = ?2)";
            let total: i64 = c.query_row(
                &format!("SELECT COUNT(*) FROM document_edits WHERE {filter}"),
                params![document_id, recording_id],
                |r| r.get(0),
            )?;
            let mut stmt = c.prepare(&format!(
                "SELECT * FROM document_edits WHERE {filter} ORDER BY id DESC LIMIT ?3 OFFSET ?4"
            ))?;
            let rows = json_rows(&mut stmt, params![document_id, recording_id, limit, offset])?;
            Ok((rows, total))
        })
    }

    pub fn transcripts(
        &self,
        recording_id: &str,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<Value>, StorageError> {
        self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT * FROM transcript_revisions WHERE recording_id = ?1 ORDER BY id LIMIT ?2 OFFSET ?3",
            )?;
            json_rows(&mut stmt, params![recording_id, limit, offset])
        })
    }

    pub fn gaps(&self) -> Result<Vec<Value>, StorageError> {
        self.read(|c| {
            let mut stmt = c.prepare("SELECT * FROM gaps ORDER BY id DESC LIMIT 100")?;
            json_rows(&mut stmt, [])
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Actor;

    fn storage(dir: &tempfile::TempDir) -> (Storage, Arc<Mutex<Vec<WriterNotice>>>) {
        let notices = Arc::new(Mutex::new(Vec::new()));
        let n = notices.clone();
        let s = Storage::open(
            &dir.path().join("db.sqlite3"),
            Arc::new(move |x| n.lock().unwrap().push(x)),
        )
        .unwrap();
        (s, notices)
    }

    #[test]
    fn documents_round_trip_and_history() {
        let dir = tempfile::tempdir().unwrap();
        let (s, _) = storage(&dir);
        s.create_document("d1", "First", "");
        s.save_document(
            "d1",
            "Hello 😀",
            3,
            &[ProvenanceSpan {
                start: 0,
                end: 5,
                recording_id: "r".into(),
            }],
        );
        s.history(HistoryItem::Recording(RecordingRow {
            id: "r".into(),
            document_id: "d1".into(),
            status: "recording".into(),
            started_at: 1,
            audio_status: "none".into(),
            ..Default::default()
        }));
        s.event(EventRow {
            recording_id: Some("r".into()),
            document_id: Some("d1".into()),
            direction: "in".into(),
            kind: "Results".into(),
            payload: Some("{}".into()),
            ..Default::default()
        });
        s.history(HistoryItem::Edits {
            document_id: "d1".into(),
            records: vec![EditRecord {
                actor: Actor::Human,
                before_revision: 0,
                after_revision: 1,
                position: 0,
                removed: String::new(),
                inserted: "Hello".into(),
                undo_group: Some(1),
                recording_id: None,
                source: None,
                at_ms: 1,
            }],
        });
        assert!(s.flush(Duration::from_secs(5)));
        let doc = s.document("d1").unwrap().unwrap();
        assert_eq!(doc.text, "Hello 😀");
        assert_eq!(doc.revision, 3);
        assert_eq!(doc.provenance.len(), 1);
        assert_eq!(doc.file_path, None);
        s.set_document_file("d1", "/tmp/notes.txt");
        s.flush(Duration::from_secs(5));
        assert_eq!(
            s.document("d1").unwrap().unwrap().file_path.as_deref(),
            Some("/tmp/notes.txt")
        );
        assert_eq!(s.documents().unwrap()[0].preview, "Hello 😀");
        assert_eq!(s.recordings("d1").unwrap().len(), 1);
        let (events, total) = s.events("r", None, 0, 10).unwrap();
        assert_eq!((events.len(), total), (1, 1));
        assert_eq!(s.edits("d1", None, 0, 10).unwrap().1, 1);
        s.close(Duration::from_secs(5));

        // Reopening marks the unfinished recording interrupted.
        let (s, _) = storage(&dir);
        let rec = s.recording("r").unwrap().unwrap();
        assert_eq!(rec["status"], "interrupted");
        s.delete_document("d1");
        s.flush(Duration::from_secs(5));
        assert!(s.document("d1").unwrap().is_none());
        assert!(s.recordings("d1").unwrap().is_empty());
    }

    #[test]
    fn upgrades_a_version_1_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db.sqlite3");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(&format!(
                "BEGIN; {MIGRATION_1} PRAGMA user_version = 1; COMMIT;"
            ))
            .unwrap();
            conn.execute(
                "INSERT INTO documents (id, title, text, revision, created_at, updated_at)
                 VALUES ('old', 'Old', 'kept', 4, 0, 0)",
                [],
            )
            .unwrap();
        }
        let s = Storage::open(&path, Arc::new(|_| {})).unwrap();
        let doc = s.document("old").unwrap().unwrap();
        assert_eq!(
            (doc.text.as_str(), doc.revision, doc.file_path),
            ("kept", 4, None)
        );
    }

    #[test]
    fn full_queue_drops_history_and_records_gap() {
        let dir = tempfile::tempdir().unwrap();
        let (s, notices) = storage(&dir);
        s.configure(5, 5000);
        for _ in 0..20 {
            s.event(EventRow {
                direction: "local".into(),
                kind: "x".into(),
                ..Default::default()
            });
        }
        assert!(s.flush(Duration::from_secs(5)));
        let gaps = s.gaps().unwrap();
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0]["dropped"], 15);
        assert!(
            notices
                .lock()
                .unwrap()
                .iter()
                .any(|n| matches!(n, WriterNotice::HistoryGap { dropped: 15, .. }))
        );
        // Documents are never dropped, even with a full history queue.
        s.create_document("d", "t", "x");
        s.flush(Duration::from_secs(5));
        assert!(s.document("d").unwrap().is_some());
    }

    #[test]
    fn saves_are_confirmed_with_their_revision() {
        let dir = tempfile::tempdir().unwrap();
        let (s, notices) = storage(&dir);
        s.create_document("d", "t", "");
        s.inject_save_failures(true);
        s.save_document("d", "lost?", 4, &[]);
        s.flush(Duration::from_secs(5));
        s.inject_save_failures(false);
        s.save_document("d", "kept", 5, &[]);
        s.flush(Duration::from_secs(5));
        let n = notices.lock().unwrap();
        assert!(
            n.iter()
                .any(|x| matches!(x, WriterNotice::DocumentSaveFailed { id, .. } if id == "d"))
        );
        assert!(
            n.iter()
                .any(|x| matches!(x, WriterNotice::DocumentSaved { id, revision: 5 } if id == "d"))
        );
        assert!(
            !n.iter()
                .any(|x| matches!(x, WriterNotice::DocumentSaved { revision: 4, .. }))
        );
        assert_eq!(s.document("d").unwrap().unwrap().text, "kept");
    }

    #[test]
    fn save_failure_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let (s, notices) = storage(&dir);
        s.create_document("d", "t", "x");
        s.create_document("d", "t", "x"); // duplicate id: constraint failure
        s.flush(Duration::from_secs(5));
        assert!(
            notices
                .lock()
                .unwrap()
                .iter()
                .any(|n| matches!(n, WriterNotice::DocumentSaveFailed { id, .. } if id == "d"))
        );
    }
}
