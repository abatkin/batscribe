//! Thin CXX-Qt adapter between QML and the Rust editor model and core.
//!
//! The GUI thread owns one [`EditorModel`] per open document and is the single place where
//! mutations are applied in order. QML reports user changes (through `TextHelper`) to
//! `userEdited`; programmatic changes go back to QML as `applyEdits` signals, which `TextHelper`
//! applies without echoing them. QML handlers that call back into this object ignore
//! notifications caused by programmatic changes (`TextHelper.applying`), so no invokable is ever
//! re-entered.
//!
//! Engine events, storage notices and history reads arrive on worker threads and are queued
//! onto the Qt thread, where they only update models and properties.

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qproperty(bool, ready)]
        /// JSON array of {id, title, preview, updatedAt}.
        #[qproperty(QString, documents_json)]
        #[qproperty(QString, current_document_id)]
        #[qproperty(QString, current_title)]
        /// The file the displayed document was last saved to, or "".
        #[qproperty(QString, current_file_path)]
        #[qproperty(bool, can_undo)]
        #[qproperty(bool, can_redo)]
        /// "idle", "recording" or "finishing".
        #[qproperty(QString, recording_state)]
        #[qproperty(QString, recording_document_id)]
        #[qproperty(QString, recording_title)]
        /// UTF-16 position of the voice caret in the displayed document, or -1.
        #[qproperty(i32, voice_caret)]
        #[qproperty(bool, following)]
        #[qproperty(f64, level)]
        #[qproperty(QString, status_text)]
        /// {schema, values, groups, devProviders}
        #[qproperty(QString, prefs_json)]
        /// {deepgram: {has, persisted}, openai: {...}}
        #[qproperty(QString, keys_json)]
        #[qproperty(i32, font_size)]
        #[qproperty(i32, save_debounce_ms)]
        #[qproperty(bool, inspector_live)]
        #[qproperty(i32, page_size)]
        /// Increments whenever history was written (live inspector refresh).
        #[qproperty(i32, history_revision)]
        #[qproperty(QString, database_path)]
        /// Open documents whose latest text isn't confirmed saved yet.
        #[qproperty(i32, unsaved_count)]
        /// The most recent save error, or "" (cleared when a save succeeds).
        #[qproperty(QString, save_error)]
        type Backend = super::BackendRust;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qinvokable]
        fn initialize(self: Pin<&mut Backend>);

        /// Saves everything and stops background work (window closing).
        #[qinvokable]
        fn shutdown(self: Pin<&mut Backend>);

        /// Called when the window is asked to close: stops a recording and saves. Returns true
        /// when nothing is pending (quit now); otherwise QML waits, showing progress, until
        /// `recordingState` is "idle" and `unsavedCount` is 0, or the user quits anyway.
        #[qinvokable]
        fn prepare_to_quit(self: Pin<&mut Backend>) -> bool;

        // ----- Editor -----

        /// A user change Qt already applied. Returns false when the model is out of sync; QML
        /// then calls `resync` with the full text.
        #[qinvokable]
        fn user_edited(
            self: Pin<&mut Backend>,
            position: i32,
            removed: i32,
            inserted: &QString,
            new_length: i32,
        ) -> bool;

        #[qinvokable]
        fn resync(self: Pin<&mut Backend>, text: &QString);

        /// Whether a key edit (`insert`, `backspace` or `delete`) would change the voice region.
        #[qinvokable]
        fn edit_blocked(
            self: &Backend,
            kind: &QString,
            cursor: i32,
            selection_start: i32,
            selection_end: i32,
        ) -> bool;

        #[qinvokable]
        fn undo(self: Pin<&mut Backend>);

        #[qinvokable]
        fn redo(self: Pin<&mut Backend>);

        /// Starts dictation with the caret's active end at `cursor`.
        #[qinvokable]
        fn start_recording(self: Pin<&mut Backend>, cursor: i32);

        #[qinvokable]
        fn stop_recording(self: Pin<&mut Backend>);

        /// The user moved the caret (navigation, click, selection, typing or scrolling).
        #[qinvokable]
        fn user_moved_caret(self: Pin<&mut Backend>, cursor: i32);

        #[qinvokable]
        fn follow_dictation(self: Pin<&mut Backend>);

        /// Saves documents and reports edits (called after an idle debounce and periodically).
        #[qinvokable]
        fn flush_edits(self: Pin<&mut Backend>);

        /// JSON array of recording IDs whose dictation overlaps the UTF-16 range.
        #[qinvokable]
        fn recordings_for_selection(self: &Backend, start: i32, end: i32) -> QString;

        // ----- Documents -----

        #[qinvokable]
        fn new_document(self: Pin<&mut Backend>);

        #[qinvokable]
        fn open_document(self: Pin<&mut Backend>, id: &QString);

        #[qinvokable]
        fn rename_document(self: Pin<&mut Backend>, id: &QString, title: &QString);

        /// Writes the displayed document to `location` (a path or file:// URL) and remembers it.
        /// Returns an error message, or "" on success.
        #[qinvokable]
        fn save_to_file(self: Pin<&mut Backend>, location: &QString) -> QString;

        /// Deletes a document and its history (not retained audio files).
        #[qinvokable]
        fn delete_document(self: Pin<&mut Backend>, id: &QString);

        // ----- Preferences and keys -----

        /// Sets a preference from a JSON value. Returns an error message, or "" on success.
        #[qinvokable]
        fn set_preference(self: Pin<&mut Backend>, key: &QString, json_value: &QString) -> QString;

        #[qinvokable]
        fn reset_preference(self: Pin<&mut Backend>, key: &QString);

        #[qinvokable]
        fn set_api_key(self: Pin<&mut Backend>, provider: &QString, key: &QString, remember: bool);

        #[qinvokable]
        fn forget_api_key(self: Pin<&mut Backend>, provider: &QString);

        #[qinvokable]
        fn refresh_devices(self: Pin<&mut Backend>);

        // ----- Inspector -----

        /// Loads history asynchronously; the result arrives as `historyLoaded`.
        #[qinvokable]
        fn request_history(self: Pin<&mut Backend>, kind: &QString, key: &QString, offset: i32);

        #[qinvokable]
        fn set_inspector_live_follow(self: Pin<&mut Backend>, live: bool);

        // ----- Development -----

        /// True in debug builds; gates development aids such as `--qml-script`.
        #[qinvokable]
        fn developer_build(self: &Backend) -> bool;

        /// The displayed model's text (for UI tests: it must always equal the editor's text).
        #[qinvokable]
        fn model_text(self: &Backend) -> QString;

        /// Debug builds only: make document saves fail, to test recovery.
        #[qinvokable]
        fn debug_fail_saves(self: Pin<&mut Backend>, fail: bool);

        /// JSON array of {"position", "remove", "insert"} for TextHelper.applyEdits.
        #[qsignal]
        fn apply_edits(self: Pin<&mut Backend>, json: QString);

        /// Replace the editor's whole text (document switch or load).
        #[qsignal]
        fn load_text(self: Pin<&mut Backend>, text: QString);

        #[qsignal]
        fn highlights_changed(self: Pin<&mut Backend>, json: QString);

        /// Move the user caret programmatically (following dictation).
        #[qsignal]
        fn place_caret(self: Pin<&mut Backend>, position: i32);

        #[qsignal]
        fn notice(self: Pin<&mut Backend>, level: QString, message: QString);

        #[qsignal]
        fn history_loaded(
            self: Pin<&mut Backend>,
            kind: QString,
            key: QString,
            offset: i32,
            json: QString,
        );
    }

    impl cxx_qt::Threading for Backend {}
}

use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use batscribe_core::credentials::{ApiKey, CredentialStore, KeyRing, Provider};
use batscribe_core::editor::{EditorModel, QtEdit, UndoError, UserEditOutcome};
use batscribe_core::engine::{self, Command, Engine, EngineContext, EngineEvent, StartRequest};
use batscribe_core::settings::{self, Paths, PrefDef, Preferences, Resolved};
use batscribe_core::storage::{Storage, WriterNotice, now_ms};
use batscribe_core::text::normalize_newlines;
use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::QString;
use serde_json::{Value, json};

pub fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(4)
            .thread_name("batscribe-worker")
            .enable_all()
            .build()
            .expect("failed to start background runtime")
    })
}

fn to_i32(v: usize) -> i32 {
    i32::try_from(v).unwrap_or(i32::MAX)
}

fn pos(v: i32) -> usize {
    usize::try_from(v).unwrap_or(0)
}

struct Core {
    storage: Storage,
    engine: Engine,
    keys: Arc<KeyRing>,
    credentials: Arc<CredentialStore>,
    paths: Paths,
}

pub struct BackendRust {
    ready: bool,
    documents_json: QString,
    current_document_id: QString,
    current_title: QString,
    current_file_path: QString,
    can_undo: bool,
    can_redo: bool,
    recording_state: QString,
    recording_document_id: QString,
    recording_title: QString,
    voice_caret: i32,
    following: bool,
    level: f64,
    status_text: QString,
    prefs_json: QString,
    keys_json: QString,
    font_size: i32,
    save_debounce_ms: i32,
    inspector_live: bool,
    page_size: i32,
    history_revision: i32,
    database_path: QString,
    unsaved_count: i32,
    save_error: QString,

    core: Option<Core>,
    /// Latest revision confirmed written, per document.
    saved: HashMap<String, u64>,
    editors: HashMap<String, EditorModel>,
    titles: HashMap<String, String>,
    file_paths: HashMap<String, String>,
    current: Option<String>,
    dirty: HashSet<String>,
    /// (recording ID, document ID) of the recording that owns a voice region.
    recording: Option<(String, String)>,
    prefs: Preferences,
    schema: Vec<PrefDef>,
    devices: Vec<(String, String)>,
    gap_warned: bool,
}

impl Default for BackendRust {
    fn default() -> Self {
        Self {
            ready: false,
            documents_json: QString::from("[]"),
            current_document_id: QString::default(),
            current_title: QString::default(),
            current_file_path: QString::default(),
            can_undo: false,
            can_redo: false,
            recording_state: QString::from("idle"),
            recording_document_id: QString::default(),
            recording_title: QString::default(),
            voice_caret: -1,
            following: false,
            level: 0.0,
            status_text: QString::default(),
            prefs_json: QString::from("{}"),
            keys_json: QString::from("{}"),
            font_size: 12,
            save_debounce_ms: 1000,
            inspector_live: true,
            page_size: 100,
            history_revision: 0,
            database_path: QString::default(),
            unsaved_count: 0,
            save_error: QString::default(),
            core: None,
            saved: HashMap::new(),
            editors: HashMap::new(),
            titles: HashMap::new(),
            file_paths: HashMap::new(),
            current: None,
            dirty: HashSet::new(),
            recording: None,
            prefs: Preferences::default(),
            schema: Vec::new(),
            devices: Vec::new(),
            gap_warned: false,
        }
    }
}

impl BackendRust {
    fn resolved(&self) -> Resolved {
        Resolved(self.prefs.resolved(&self.schema))
    }

    fn model(&self) -> Option<&EditorModel> {
        self.current.as_ref().and_then(|id| self.editors.get(id))
    }

    fn model_mut(&mut self) -> Option<&mut EditorModel> {
        let id = self.current.clone()?;
        self.editors.get_mut(&id)
    }

    /// The document has changes not yet confirmed written to the database.
    fn unconfirmed(&self, id: &str) -> bool {
        self.dirty.contains(id)
            || self.editors.get(id).is_some_and(|m| {
                m.has_pending_log() || m.revision() > self.saved.get(id).copied().unwrap_or(0)
            })
    }

    fn recording_doc(&self) -> Option<&str> {
        self.recording.as_ref().map(|(_, d)| d.as_str())
    }

    fn state(&self) -> String {
        self.recording_state.to_string()
    }
}

impl qobject::Backend {
    fn emit_edits(self: Pin<&mut Self>, edits: &[QtEdit]) {
        if edits.is_empty() {
            return;
        }
        let json = serde_json::to_string(edits).unwrap_or_else(|_| "[]".into());
        self.apply_edits(QString::from(&json));
    }

    fn notify(self: Pin<&mut Self>, level: &str, message: &str) {
        self.notice(QString::from(level), QString::from(message));
    }

    /// Runs `f` on the displayed document's model.
    fn with_model<R>(self: Pin<&mut Self>, f: impl FnOnce(&mut EditorModel) -> R) -> Option<R> {
        let mut rust = self.rust_mut();
        rust.model_mut().map(f)
    }

    /// Runs `f` on a loaded document's model.
    fn with_doc<R>(
        self: Pin<&mut Self>,
        id: &str,
        f: impl FnOnce(&mut EditorModel) -> R,
    ) -> Option<R> {
        let mut rust = self.rust_mut();
        rust.editors.get_mut(id).map(f)
    }

    fn storage(&self) -> Option<Storage> {
        self.rust().core.as_ref().map(|c| c.storage.clone())
    }

    /// Refreshes everything derived from the displayed model after a change.
    fn refresh(mut self: Pin<&mut Self>) {
        // Every model change ends here (or in the engine handler); keep the count current so
        // closing never mistakes a fresh change for a saved one.
        self.as_mut().update_unsaved();
        let r = self.rust();
        let Some(model) = r.model() else {
            self.as_mut().highlights_changed(QString::from("[]"));
            self.as_mut().set_voice_caret(-1);
            return;
        };
        let highlights = model.highlights();
        let json = serde_json::to_string(&highlights).unwrap_or_else(|_| "[]".into());
        let displayed_recording = r
            .current
            .as_deref()
            .is_some_and(|c| Some(c) == r.recording_doc());
        let caret = match model.region() {
            Some(region) if displayed_recording && r.state() == "recording" => {
                to_i32(model.to_utf16(region.end))
            }
            _ => -1,
        };
        let (undo, redo) = (model.can_undo(), model.can_redo());
        let following = r.following;
        self.as_mut().highlights_changed(QString::from(&json));
        self.as_mut().set_voice_caret(caret);
        self.as_mut().set_can_undo(undo);
        self.as_mut().set_can_redo(redo);
        if following && caret >= 0 {
            self.place_caret(caret);
        }
    }

    // ----- Startup and shutdown -----

    pub fn initialize(mut self: Pin<&mut Self>) {
        if self.rust().core.is_some() {
            return;
        }
        let paths = Paths::from_env();
        let qt = self.qt_thread();
        let storage = match Storage::open(
            &paths.database(),
            Arc::new(move |n| {
                let _ = qt.queue(move |b| b.on_writer_notice(n));
            }),
        ) {
            Ok(s) => s,
            Err(e) => {
                self.as_mut().notify(
                    "error",
                    &format!(
                        "Couldn't open the database at {}: {e}",
                        paths.database().display()
                    ),
                );
                return;
            }
        };
        self.as_mut()
            .set_database_path(QString::from(paths.database().to_string_lossy().as_ref()));
        let prefs = storage.preferences().unwrap_or_default();
        self.as_mut().rust_mut().prefs = prefs;
        self.as_mut().rust_mut().schema = settings::schema(&paths, &[]);

        let keys = Arc::new(KeyRing::default());
        let credentials = Arc::new(CredentialStore::from_env());
        let qt = self.qt_thread();
        let sink: engine::Sink = Arc::new(move |e| {
            let _ = qt.queue(move |b| b.on_engine_event(e));
        });
        let engine = Engine::start(
            runtime().handle(),
            EngineContext {
                storage: storage.clone(),
                keys: keys.clone(),
                sink,
                asr: engine::default_asr_factory(),
                composer: engine::default_composer_factory(),
            },
        );
        self.as_mut().rust_mut().core = Some(Core {
            storage: storage.clone(),
            engine,
            keys: keys.clone(),
            credentials: credentials.clone(),
            paths,
        });
        self.as_mut().apply_preferences();

        // Keys from the keyring, in the background.
        let qt = self.qt_thread();
        runtime().spawn(async move {
            let mut errors = Vec::new();
            for p in Provider::ALL {
                match credentials.load(p).await {
                    Ok(Some(k)) => keys.set(p, k, true),
                    Ok(None) => {}
                    Err(e) => errors.push(e.to_string()),
                }
            }
            let _ = qt.queue(move |mut b| {
                b.as_mut().update_keys_json();
                if let Some(e) = errors.first() {
                    b.notify(
                        "warning",
                        &format!("{e}. Keys you enter are kept for this session only."),
                    );
                }
            });
        });
        self.as_mut().refresh_devices();
        let reconcile = storage.clone();
        runtime().spawn_blocking(move || engine::reconcile_audio(&reconcile));

        // Open the most recent document, or create the first one.
        let docs = storage.documents().unwrap_or_default();
        match docs.first() {
            Some(d) => {
                let id = d.id.clone();
                self.as_mut().set_documents_json(QString::from(
                    &serde_json::to_string(&docs).unwrap_or_default(),
                ));
                for d in &docs {
                    self.as_mut()
                        .rust_mut()
                        .titles
                        .insert(d.id.clone(), d.title.clone());
                }
                self.as_mut().open_document(&QString::from(&id));
            }
            None => self.as_mut().new_document(),
        }
        self.set_ready(true);
    }

    fn update_unsaved(mut self: Pin<&mut Self>) {
        let n = self
            .rust()
            .editors
            .keys()
            .filter(|k| self.rust().unconfirmed(k))
            .count();
        self.as_mut().set_unsaved_count(to_i32(n));
    }

    pub fn prepare_to_quit(mut self: Pin<&mut Self>) -> bool {
        if self.rust().state() == "recording" {
            self.as_mut().stop_recording();
        }
        self.as_mut().flush_edits();
        self.as_mut().update_unsaved();
        self.rust().state() == "idle" && self.rust().unsaved_count == 0
    }

    pub fn debug_fail_saves(self: Pin<&mut Self>, fail: bool) {
        if cfg!(debug_assertions)
            && let Some(storage) = self.storage()
        {
            storage.inject_save_failures(fail);
        }
    }

    pub fn shutdown(mut self: Pin<&mut Self>) {
        if self.rust().recording.is_some()
            && let Some(core) = &self.rust().core
        {
            core.engine.send(Command::Stop);
        }
        self.as_mut().flush_edits();
        if let Some(core) = &self.rust().core {
            core.storage.close(Duration::from_secs(5));
        }
    }

    fn on_writer_notice(mut self: Pin<&mut Self>, n: WriterNotice) {
        match n {
            WriterNotice::DocumentSaveFailed { id, message } => {
                let title = self.rust().titles.get(&id).cloned().unwrap_or_default();
                self.as_mut().rust_mut().dirty.insert(id);
                let message = format!("Couldn't save “{title}”: {message}. It will be retried.");
                self.as_mut().set_save_error(QString::from(&message));
                self.as_mut().notify("error", &message);
                self.update_unsaved();
            }
            WriterNotice::DocumentSaved { id, revision } => {
                let saved = self
                    .rust()
                    .saved
                    .get(&id)
                    .copied()
                    .unwrap_or(0)
                    .max(revision);
                self.as_mut().rust_mut().saved.insert(id.clone(), saved);
                // A document that is neither shown nor recording can leave memory once its
                // latest text is safely written.
                let shown = self.rust().current.as_deref() == Some(&id)
                    || self.rust().recording_doc() == Some(&id);
                if !shown && !self.rust().unconfirmed(&id) {
                    self.as_mut().rust_mut().editors.remove(&id);
                }
                if self
                    .rust()
                    .editors
                    .keys()
                    .all(|k| !self.rust().unconfirmed(k))
                {
                    self.as_mut().set_save_error(QString::default());
                }
                self.update_unsaved();
            }
            WriterNotice::HistoryGap { dropped, reason } => {
                if !self.rust().gap_warned {
                    self.as_mut().rust_mut().gap_warned = true;
                    self.notify(
                        "warning",
                        &format!("{dropped} history events were not saved ({reason}). Dictation is unaffected."),
                    );
                }
            }
            WriterNotice::HistoryWritten => {
                let r = self.history_revision().wrapping_add(1);
                self.set_history_revision(r);
            }
        }
    }

    // ----- Editor -----

    pub fn user_edited(
        mut self: Pin<&mut Self>,
        position: i32,
        removed: i32,
        inserted: &QString,
        new_length: i32,
    ) -> bool {
        let inserted = normalize_newlines(&inserted.to_string());
        let Some(outcome) = self.as_mut().with_model(|m| {
            m.user_edit(
                pos(position),
                pos(removed),
                &inserted,
                pos(new_length),
                now_ms(),
            )
        }) else {
            return true;
        };
        self.handle_outcome(outcome)
    }

    fn handle_outcome(mut self: Pin<&mut Self>, outcome: UserEditOutcome) -> bool {
        match outcome {
            UserEditOutcome::NoOp => true,
            UserEditOutcome::Accepted => {
                if let Some(id) = self.rust().current.clone() {
                    self.as_mut().rust_mut().dirty.insert(id);
                }
                self.refresh();
                true
            }
            UserEditOutcome::Revert(edit) => {
                self.as_mut().emit_edits(&[edit]);
                self.as_mut().notify(
                    "info",
                    "Text that is still being dictated can't be edited yet.",
                );
                self.refresh();
                true
            }
            UserEditOutcome::Desync => false,
        }
    }

    pub fn resync(mut self: Pin<&mut Self>, text: &QString) {
        let text = normalize_newlines(&text.to_string());
        let Some(outcome) = self.as_mut().with_model(|m| m.resync(&text, now_ms())) else {
            return;
        };
        if !self.as_mut().handle_outcome(outcome) {
            let text = self
                .rust()
                .model()
                .map(|m| m.text().to_owned())
                .unwrap_or_default();
            self.load_text(QString::from(&text));
        }
    }

    pub fn edit_blocked(&self, kind: &QString, cursor: i32, start: i32, end: i32) -> bool {
        self.rust().model().is_some_and(|m| {
            m.edit_would_touch_region(&kind.to_string(), pos(cursor), pos(start), pos(end))
        })
    }

    pub fn undo(mut self: Pin<&mut Self>) {
        let Some(result) = self.as_mut().with_model(|m| m.undo(now_ms())) else {
            return;
        };
        self.finish_undo(result);
    }

    pub fn redo(mut self: Pin<&mut Self>) {
        let Some(result) = self.as_mut().with_model(|m| m.redo(now_ms())) else {
            return;
        };
        self.finish_undo(result);
    }

    fn finish_undo(mut self: Pin<&mut Self>, result: Result<Vec<QtEdit>, UndoError>) {
        match result {
            Ok(edits) => {
                self.as_mut().emit_edits(&edits);
                if let Some(id) = self.rust().current.clone() {
                    self.as_mut().rust_mut().dirty.insert(id);
                }
            }
            Err(UndoError::Empty) => {}
            Err(e) => self.as_mut().notify("warning", &e.to_string()),
        }
        self.refresh();
    }

    pub fn start_recording(mut self: Pin<&mut Self>, cursor: i32) {
        if self.rust().recording.is_some() || self.rust().state() != "idle" {
            return;
        }
        let Some(doc_id) = self.rust().current.clone() else {
            return;
        };
        if self.rust().core.is_none() {
            return;
        }
        self.as_mut().flush_edits();
        let prefs = self.rust().resolved();
        let exact = prefs.str("editor.start_placement") == "caret";
        let recording_id = uuid::Uuid::new_v4().to_string();
        let Some((edits, lead, context, position)) = self.as_mut().with_model(|model| {
            let byte = model.from_utf16(pos(cursor)).unwrap_or(model.text().len());
            let at = model.start_position(byte, exact);
            let lead = model.needs_leading_space(at);
            let edits = model.open_region(&recording_id, at, now_ms());
            let context = model.context_before_region(
                prefs.int("comp.context_paragraphs").max(0) as usize,
                prefs.int("comp.context_max_chars").max(0) as usize,
            );
            (edits, lead, context, at as i64)
        }) else {
            return;
        };
        self.as_mut().rust_mut().recording = Some((recording_id.clone(), doc_id.clone()));
        self.as_mut().rust_mut().dirty.insert(doc_id.clone());
        let title = self.rust().titles.get(&doc_id).cloned().unwrap_or_default();
        self.as_mut()
            .set_recording_document_id(QString::from(&doc_id));
        self.as_mut().set_recording_title(QString::from(&title));
        self.as_mut()
            .set_recording_state(QString::from("recording"));
        self.as_mut().set_following(true);
        self.as_mut().emit_edits(&edits);
        self.as_mut().refresh();
        if let Some(core) = &self.rust().core {
            core.engine.send(Command::Start(StartRequest {
                recording_id,
                document_id: doc_id,
                lead,
                context,
                initial_position: position,
                prefs,
            }));
        }
    }

    pub fn stop_recording(mut self: Pin<&mut Self>) {
        if self.rust().state() != "recording" {
            return;
        }
        if let Some(core) = &self.rust().core {
            core.engine.send(Command::Stop);
        }
        // The voice caret disappears at once; the region stays until finishing completes.
        self.as_mut()
            .set_recording_state(QString::from("finishing"));
        self.as_mut().set_following(false);
        self.as_mut().flush_edits();
        self.refresh();
    }

    pub fn user_moved_caret(mut self: Pin<&mut Self>, cursor: i32) {
        if self.rust().following && cursor != self.rust().voice_caret {
            self.as_mut().set_following(false);
        }
    }

    pub fn follow_dictation(mut self: Pin<&mut Self>) {
        if self.rust().state() != "recording" {
            return;
        }
        if let Some(doc) = self.rust().recording_doc().map(str::to_owned)
            && self.rust().current.as_deref() != Some(&doc)
        {
            self.as_mut().open_document(&QString::from(&doc));
        }
        self.as_mut().set_following(true);
        self.refresh();
    }

    fn on_engine_event(mut self: Pin<&mut Self>, event: EngineEvent) {
        self.as_mut().handle_engine_event(event);
        // Voice updates may change a document that isn't displayed (no refresh).
        self.update_unsaved();
    }

    fn handle_engine_event(mut self: Pin<&mut Self>, event: EngineEvent) {
        match event {
            EngineEvent::Region {
                document_id,
                update,
            } => {
                let displayed = self.rust().current.as_deref() == Some(&document_id);
                let Some(result) = self
                    .as_mut()
                    .with_doc(&document_id, |m| m.apply_region(&update, now_ms()))
                else {
                    return;
                };
                match result {
                    Ok(edits) => {
                        self.as_mut().rust_mut().dirty.insert(document_id);
                        if displayed {
                            self.as_mut().emit_edits(&edits);
                        }
                    }
                    Err(e) => self.as_mut().notify(
                        "error",
                        &format!("A dictation update couldn't be applied: {e}"),
                    ),
                }
                if displayed {
                    self.refresh();
                }
            }
            EngineEvent::RegionClosed {
                document_id,
                recording_id,
            } => {
                self.as_mut()
                    .with_doc(&document_id, |m| m.close_region(&recording_id));
                self.as_mut().rust_mut().dirty.insert(document_id);
                if self
                    .rust()
                    .recording
                    .as_ref()
                    .is_some_and(|(r, _)| *r == recording_id)
                {
                    self.as_mut().rust_mut().recording = None;
                }
                self.as_mut().flush_edits();
                self.refresh();
            }
            EngineEvent::State { state, .. } => {
                self.as_mut().set_recording_state(QString::from(state));
                if state != "recording" {
                    self.as_mut().set_following(false);
                    self.as_mut().set_level(0.0);
                }
                if state == "idle" {
                    self.as_mut().set_recording_document_id(QString::default());
                    self.as_mut().set_recording_title(QString::default());
                    self.as_mut().set_status_text(QString::default());
                }
                self.refresh();
            }
            EngineEvent::Level(l) => self.set_level(f64::from(l)),
            EngineEvent::Status(s) => self.set_status_text(QString::from(&s)),
            EngineEvent::Notice { level, message } => self.notify(level, &message),
        }
    }

    pub fn flush_edits(mut self: Pin<&mut Self>) {
        let Some(storage) = self.storage() else {
            return;
        };
        let prefs = self.rust().resolved();
        let mut ids: Vec<String> = self.rust().dirty.iter().cloned().collect();
        for (id, m) in &self.rust().editors {
            if m.has_pending_log() && !ids.contains(id) {
                ids.push(id.clone());
            }
        }
        if ids.is_empty() {
            return;
        }
        for id in &ids {
            let Some((log, text, revision, provenance)) = self.as_mut().with_doc(id, |m| {
                (
                    m.take_log(),
                    m.text().to_owned(),
                    m.revision(),
                    m.provenance().to_vec(),
                )
            }) else {
                continue;
            };
            if !log.is_empty() {
                storage.history(batscribe_core::storage::HistoryItem::Edits {
                    document_id: id.clone(),
                    records: log,
                });
            }
            storage.save_document(id, &text, revision, &provenance);
        }
        self.as_mut().rust_mut().dirty.clear();
        // Fresh read-only context for the recording's next edit requests.
        if let Some((rec, doc)) = self.rust().recording.clone()
            && let Some(model) = self.rust().editors.get(&doc)
            && let Some(core) = &self.rust().core
        {
            core.engine.send(Command::Context {
                recording_id: rec,
                context: model.context_before_region(
                    prefs.int("comp.context_paragraphs").max(0) as usize,
                    prefs.int("comp.context_max_chars").max(0) as usize,
                ),
            });
        }
        self.as_mut().update_unsaved();
        self.reload_documents();
    }

    pub fn recordings_for_selection(&self, start: i32, end: i32) -> QString {
        let ids = self.rust().model().map_or_else(Vec::new, |m| {
            let (a, b) = (m.from_utf16(pos(start)), m.from_utf16(pos(end)));
            match (a, b) {
                (Some(a), Some(b)) => m.recordings_in(a.min(b), a.max(b)),
                _ => Vec::new(),
            }
        });
        QString::from(&serde_json::to_string(&ids).unwrap_or_else(|_| "[]".into()))
    }

    // ----- Documents -----

    fn reload_documents(self: Pin<&mut Self>) {
        let Some(storage) = self.storage() else {
            return;
        };
        let qt = self.qt_thread();
        runtime().spawn_blocking(move || {
            // Saves are queued ahead of this read; let them land first.
            storage.flush(Duration::from_secs(2));
            if let Ok(docs) = storage.documents() {
                let _ = qt.queue(move |mut b| {
                    for d in &docs {
                        b.as_mut()
                            .rust_mut()
                            .titles
                            .insert(d.id.clone(), d.title.clone());
                    }
                    let json = serde_json::to_string(&docs).unwrap_or_else(|_| "[]".into());
                    b.set_documents_json(QString::from(&json));
                });
            }
        });
    }

    pub fn new_document(mut self: Pin<&mut Self>) {
        let Some(storage) = self.storage() else {
            return;
        };
        let id = uuid::Uuid::new_v4().to_string();
        let title = "Untitled";
        storage.create_document(&id, title, "");
        let mut model = EditorModel::default();
        model.set_group_interval_ms(self.rust().resolved().int("editor.undo_group_ms"));
        self.as_mut().rust_mut().editors.insert(id.clone(), model);
        self.as_mut()
            .rust_mut()
            .titles
            .insert(id.clone(), title.to_owned());
        self.as_mut().open_document(&QString::from(&id));
        self.reload_documents();
    }

    pub fn open_document(mut self: Pin<&mut Self>, id: &QString) {
        let id = id.to_string();
        if self.rust().current.as_deref() == Some(&id) {
            return;
        }
        self.as_mut().flush_edits();
        if !self.rust().editors.contains_key(&id) {
            let Some(storage) = self.storage() else {
                return;
            };
            match storage.document(&id) {
                Ok(Some(doc)) => {
                    let mut model =
                        EditorModel::new(doc.text, doc.revision).with_provenance(doc.provenance);
                    model.set_group_interval_ms(self.rust().resolved().int("editor.undo_group_ms"));
                    self.as_mut()
                        .rust_mut()
                        .titles
                        .insert(id.clone(), doc.title);
                    if let Some(path) = doc.file_path {
                        self.as_mut().rust_mut().file_paths.insert(id.clone(), path);
                    }
                    self.as_mut()
                        .rust_mut()
                        .saved
                        .insert(id.clone(), doc.revision);
                    self.as_mut().rust_mut().editors.insert(id.clone(), model);
                }
                Ok(None) => {
                    self.as_mut()
                        .notify("warning", "That document no longer exists.");
                    return;
                }
                Err(e) => {
                    self.as_mut()
                        .notify("error", &format!("Couldn't open the document: {e}"));
                    return;
                }
            }
        }
        // Keep the displayed document, the recording's document, and any document whose latest
        // text isn't confirmed saved (a failed save must not lose it); drop the rest.
        let mut keep: Vec<String> = [
            Some(id.clone()),
            self.rust().recording_doc().map(str::to_owned),
        ]
        .into_iter()
        .flatten()
        .collect();
        keep.extend(
            self.rust()
                .editors
                .keys()
                .filter(|k| self.rust().unconfirmed(k))
                .cloned()
                .collect::<Vec<_>>(),
        );
        self.as_mut()
            .rust_mut()
            .editors
            .retain(|k, _| keep.contains(k));
        self.as_mut().rust_mut().current = Some(id.clone());
        let title = self.rust().titles.get(&id).cloned().unwrap_or_default();
        let text = self
            .rust()
            .model()
            .map(|m| m.text().to_owned())
            .unwrap_or_default();
        self.as_mut().set_current_document_id(QString::from(&id));
        self.as_mut().set_current_title(QString::from(&title));
        let file = self.rust().file_paths.get(&id).cloned().unwrap_or_default();
        self.as_mut().set_current_file_path(QString::from(&file));
        if self.rust().recording_doc() != Some(&id) {
            self.as_mut().set_following(false);
        }
        self.as_mut().load_text(QString::from(&text));
        self.refresh();
    }

    pub fn save_to_file(mut self: Pin<&mut Self>, location: &QString) -> QString {
        let Some(path) = batscribe_core::export::path_from_url(&location.to_string()) else {
            return QString::from("That location isn't a local file.");
        };
        let (Some(id), Some(text)) = (
            self.rust().current.clone(),
            self.rust().model().map(|m| m.text().to_owned()),
        ) else {
            return QString::from("No document is open.");
        };
        if let Err(e) = batscribe_core::export::write_text_file(&path, &text) {
            return QString::from(&format!("Couldn't save to {}: {e}", path.display()));
        }
        let shown = path.to_string_lossy().into_owned();
        if let Some(storage) = self.storage() {
            storage.set_document_file(&id, &shown);
        }
        self.as_mut()
            .rust_mut()
            .file_paths
            .insert(id.clone(), shown.clone());
        self.as_mut().set_current_file_path(QString::from(&shown));
        // An untitled document takes the file's name.
        if self.rust().titles.get(&id).is_none_or(|t| t == "Untitled")
            && let Some(title) = batscribe_core::export::title_from_path(&path)
        {
            self.as_mut()
                .rename_document(&QString::from(&id), &QString::from(&title));
        }
        self.notify("info", &format!("Saved to {shown}"));
        QString::default()
    }

    pub fn rename_document(mut self: Pin<&mut Self>, id: &QString, title: &QString) {
        let (id, title) = (id.to_string(), title.to_string().trim().to_owned());
        if title.is_empty() {
            return;
        }
        if let Some(storage) = self.storage() {
            storage.rename_document(&id, &title);
        }
        self.as_mut()
            .rust_mut()
            .titles
            .insert(id.clone(), title.clone());
        if self.rust().current.as_deref() == Some(&id) {
            self.as_mut().set_current_title(QString::from(&title));
        }
        if self.rust().recording_doc() == Some(&id) {
            self.as_mut().set_recording_title(QString::from(&title));
        }
        self.reload_documents();
    }

    pub fn delete_document(mut self: Pin<&mut Self>, id: &QString) {
        let id = id.to_string();
        if self.rust().recording_doc() == Some(&id) {
            self.notify(
                "warning",
                "Stop the recording before deleting its document.",
            );
            return;
        }
        let Some(storage) = self.storage() else {
            return;
        };
        storage.delete_document(&id);
        self.as_mut().rust_mut().editors.remove(&id);
        self.as_mut().rust_mut().dirty.remove(&id);
        self.as_mut().rust_mut().titles.remove(&id);
        if self.rust().current.as_deref() == Some(&id) {
            self.as_mut().rust_mut().current = None;
            storage.flush(Duration::from_secs(2));
            let next = storage
                .documents()
                .ok()
                .and_then(|d| d.into_iter().find(|d| d.id != id))
                .map(|d| d.id);
            match next {
                Some(next) => self.as_mut().open_document(&QString::from(&next)),
                None => self.as_mut().new_document(),
            }
        }
        self.reload_documents();
    }

    // ----- Preferences and keys -----

    fn apply_preferences(mut self: Pin<&mut Self>) {
        let r = self.rust().resolved();
        self.as_mut()
            .set_font_size(r.int("editor.font_size") as i32);
        self.as_mut()
            .set_save_debounce_ms(r.int("editor.report_debounce_ms") as i32);
        self.as_mut()
            .set_inspector_live(r.bool("inspector.live_follow"));
        self.as_mut()
            .set_page_size(r.int("inspector.page_size") as i32);
        let interval = r.int("editor.undo_group_ms");
        for m in self.as_mut().rust_mut().editors.values_mut() {
            m.set_group_interval_ms(interval);
        }
        if let Some(core) = &self.rust().core {
            core.storage.configure(
                r.int("storage.history_queue").max(1) as usize,
                r.int("storage.history_batch_ms").max(1) as u64,
            );
        }
        let mut groups: Vec<&str> = Vec::new();
        for d in &self.rust().schema {
            if !groups.contains(&d.group) {
                groups.push(d.group);
            }
        }
        let json = json!({
            "schema": self.rust().schema,
            "values": r.0,
            "stored": self.rust().prefs.stored.keys().collect::<Vec<_>>(),
            "groups": groups,
            "devProviders": settings::dev_providers_enabled(),
        });
        self.set_prefs_json(QString::from(&json.to_string()));
    }

    pub fn set_preference(
        mut self: Pin<&mut Self>,
        key: &QString,
        json_value: &QString,
    ) -> QString {
        let key = key.to_string();
        let Some(def) = self.rust().schema.iter().find(|d| d.key == key).cloned() else {
            return QString::from("Unknown setting");
        };
        let value: Value = match serde_json::from_str(&json_value.to_string()) {
            Ok(v) => v,
            Err(_) => Value::String(json_value.to_string()),
        };
        let value = match settings::validate(&def, &value) {
            Ok(v) => v,
            Err(e) => return QString::from(&e),
        };
        if let Some(storage) = self.storage() {
            storage.set_preference(&key, Some(&value));
        }
        self.as_mut().rust_mut().prefs.stored.insert(key, value);
        self.apply_preferences();
        QString::default()
    }

    pub fn reset_preference(mut self: Pin<&mut Self>, key: &QString) {
        let key = key.to_string();
        if let Some(storage) = self.storage() {
            storage.set_preference(&key, None);
        }
        self.as_mut().rust_mut().prefs.stored.remove(&key);
        self.apply_preferences();
    }

    pub fn refresh_devices(self: Pin<&mut Self>) {
        let qt = self.qt_thread();
        runtime().spawn_blocking(move || {
            let devices = batscribe_core::audio::input_devices();
            let _ = qt.queue(move |mut b| {
                if let Some(core) = &b.rust().core {
                    let schema = settings::schema(&core.paths, &devices);
                    b.as_mut().rust_mut().schema = schema;
                }
                b.as_mut().rust_mut().devices = devices;
                b.apply_preferences();
            });
        });
    }

    fn update_keys_json(self: Pin<&mut Self>) {
        let Some(core) = &self.rust().core else {
            return;
        };
        let mut obj = serde_json::Map::new();
        for p in Provider::ALL {
            let (has, persisted) = core.keys.status(p);
            obj.insert(
                p.id().into(),
                json!({"has": has, "persisted": persisted, "label": p.label()}),
            );
        }
        self.set_keys_json(QString::from(&Value::Object(obj).to_string()));
    }

    pub fn set_api_key(
        mut self: Pin<&mut Self>,
        provider: &QString,
        key: &QString,
        remember: bool,
    ) {
        let (Some(provider), Some(key)) = (
            Provider::from_id(&provider.to_string()),
            ApiKey::new(&key.to_string()),
        ) else {
            return;
        };
        let Some(core) = &self.rust().core else {
            return;
        };
        core.keys.set(provider, key.clone(), false);
        let (keys, credentials) = (core.keys.clone(), core.credentials.clone());
        let qt = self.qt_thread();
        runtime().spawn(async move {
            let result = if remember {
                let _ = credentials.delete(provider).await;
                credentials.save(provider, &key).await.map(|()| true)
            } else {
                credentials.delete(provider).await.map(|()| false)
            };
            let error = match result {
                Ok(persisted) => {
                    keys.set(provider, key, persisted);
                    None
                }
                Err(e) => Some(e.to_string()),
            };
            let _ = qt.queue(move |mut b| {
                b.as_mut().update_keys_json();
                if let Some(e) = error {
                    b.notify(
                        "warning",
                        &format!("{e}. The key is kept for this session only."),
                    );
                }
            });
        });
        self.as_mut().update_keys_json();
    }

    pub fn forget_api_key(mut self: Pin<&mut Self>, provider: &QString) {
        let Some(provider) = Provider::from_id(&provider.to_string()) else {
            return;
        };
        let Some(core) = &self.rust().core else {
            return;
        };
        core.keys.remove(provider);
        let credentials = core.credentials.clone();
        let qt = self.qt_thread();
        runtime().spawn(async move {
            if let Err(e) = credentials.delete(provider).await {
                let _ = qt.queue(move |b| b.notify("warning", &e.to_string()));
            }
        });
        self.as_mut().update_keys_json();
    }

    // ----- Inspector -----

    pub fn request_history(self: Pin<&mut Self>, kind: &QString, key: &QString, offset: i32) {
        let Some(storage) = self.storage() else {
            return;
        };
        let (kind, key) = (kind.to_string(), key.to_string());
        let page = i64::from(self.rust().page_size.max(1));
        let qt = self.qt_thread();
        runtime().spawn_blocking(move || {
            let offset = i64::from(offset);
            let paged = |f: &dyn Fn(
                i64,
            ) -> Result<
                (Vec<Value>, i64),
                batscribe_core::storage::StorageError,
            >| {
                // A negative offset asks for the last page (live view).
                let (rows, total) = if offset < 0 {
                    let (_, total) = f(0)?;
                    let start = (total - page).max(0);
                    let (rows, total) = f(start)?;
                    return Ok(json!({"rows": rows, "total": total, "offset": start}));
                } else {
                    f(offset)?
                };
                Ok(json!({"rows": rows, "total": total, "offset": offset}))
            };
            let result: Result<Value, batscribe_core::storage::StorageError> = match kind.as_str() {
                "recordings" => storage.recordings(&key).map(Value::from),
                "recording" => storage.recording(&key).map(|v| {
                    let mut v = v.unwrap_or(Value::Null);
                    if let Some(path) = v.get("audio_path").and_then(Value::as_str) {
                        v["audioExists"] = json!(std::path::Path::new(path).exists());
                    }
                    v
                }),
                "operations" => storage.operations(&key).map(Value::from),
                "operation" => storage.operation(&key).map(|v| v.unwrap_or(Value::Null)),
                "events" => paged(&|o| storage.events(&key, None, o, page)),
                "operationEvents" => paged(&|o| storage.events("", Some(&key), o, page)),
                "event" => storage
                    .event_detail(key.parse().unwrap_or(-1))
                    .map(|v| v.unwrap_or(Value::Null)),
                "edits" => paged(&|o| storage.edits(&key, None, o, page)),
                "recordingEdits" => {
                    let (doc, rec) = key.split_once('|').unwrap_or((&key, ""));
                    paged(&|o| storage.edits(doc, Some(rec), o, page))
                }
                "transcripts" => storage
                    .transcripts(&key, offset.max(0), page)
                    .map(|rows| json!({"rows": rows, "offset": offset.max(0)})),
                "gaps" => storage.gaps().map(Value::from),
                _ => Ok(Value::Null),
            };
            let json = match result {
                Ok(v) => v.to_string(),
                Err(e) => json!({"error": e.to_string()}).to_string(),
            };
            let _ = qt.queue(move |b| {
                b.history_loaded(
                    QString::from(&kind),
                    QString::from(&key),
                    offset as i32,
                    QString::from(&json),
                );
            });
        });
    }

    pub fn set_inspector_live_follow(self: Pin<&mut Self>, live: bool) {
        self.set_inspector_live(live);
    }

    // ----- Development -----

    pub fn developer_build(&self) -> bool {
        cfg!(debug_assertions)
    }

    pub fn model_text(&self) -> QString {
        QString::from(self.rust().model().map_or("", |m| m.text()))
    }
}
