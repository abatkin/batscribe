//! The dictation engine: one session per recording, which is the single owner of that
//! recording's voice region.
//!
//! A session consumes audio batches, drives a recognition connection (reconnecting with replay
//! from the recovery buffer, or recording an explicit gap), schedules composition requests (one
//! in flight, one coalesced pending), and turns everything into [`RegionUpdate`]s for the editor.
//! Every exchange is recorded in history; history never blocks processing.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::asr::{AsrConfig, AsrInput, AsrMessage, AsrOutput, AsrProvider, contains_correction};
use crate::audio::{self, AudioChunk, AudioEvent, Capture, wav};
use crate::composition::{Composer, Decision, EditOutcome, EditRequest, validate_replacement};
use crate::credentials::{KeyRing, Provider};
use crate::editor::RegionUpdate;
use crate::region::{RegionLimits, Segment, VoiceRegion};
use crate::settings::Resolved;
use crate::storage::{
    EventRow, HistoryItem, OperationRow, RecordingRow, Storage, TranscriptRow, now_ms,
};

/// Transport sample rate (and the recording's sample timeline).
pub const TRANSPORT_RATE: u32 = 16_000;

/// Queue room for live audio batches beyond a replay. When a connection falls this far behind,
/// it is restarted from the recovery buffer.
const LIVE_QUEUE: usize = 512;

/// Sends control messages in order, waiting for queue room rather than dropping them (they
/// follow all audio, and a lost Finalize or Close would leave the connection open).
fn send_controls(input: &mpsc::Sender<AsrInput>, msgs: Vec<AsrInput>) {
    let tx = input.clone();
    tokio::spawn(async move {
        for msg in msgs {
            if tx.send(msg).await.is_err() {
                break;
            }
        }
    });
}

#[derive(Debug, Clone)]
pub enum EngineEvent {
    Region {
        document_id: String,
        update: RegionUpdate,
    },
    /// The region is finished; the editor turns it into ordinary text.
    RegionClosed {
        document_id: String,
        recording_id: String,
    },
    /// "recording", "finishing" or "idle".
    State {
        state: &'static str,
        recording_id: Option<String>,
        document_id: Option<String>,
    },
    Level(f32),
    /// Short status line ("Connecting…", "Reconnecting…"); empty clears it.
    Status(String),
    Notice {
        level: &'static str,
        message: String,
    },
}

pub type Sink = Arc<dyn Fn(EngineEvent) + Send + Sync>;
pub type AsrFactory = Arc<dyn Fn(&str) -> Option<Arc<dyn AsrProvider>> + Send + Sync>;
pub type ComposerFactory = Arc<dyn Fn(&str) -> Option<Arc<dyn Composer>> + Send + Sync>;

#[derive(Clone)]
pub struct EngineContext {
    pub storage: Storage,
    pub keys: Arc<KeyRing>,
    pub sink: Sink,
    pub asr: AsrFactory,
    pub composer: ComposerFactory,
}

/// The default providers.
pub fn default_asr_factory() -> AsrFactory {
    Arc::new(|name| match name {
        "deepgram" => {
            Some(Arc::new(crate::asr::deepgram::Deepgram::default()) as Arc<dyn AsrProvider>)
        }
        "mock" => Some(Arc::new(crate::asr::mock::MockAsr::default()) as Arc<dyn AsrProvider>),
        _ => None,
    })
}

pub fn default_composer_factory() -> ComposerFactory {
    Arc::new(|name| match name {
        "openai" => {
            Some(Arc::new(crate::composition::openai::OpenAi::default()) as Arc<dyn Composer>)
        }
        "anthropic" => {
            Some(Arc::new(crate::composition::anthropic::Anthropic::default()) as Arc<dyn Composer>)
        }
        "mock" => {
            Some(Arc::new(crate::composition::mock::MockComposer::default()) as Arc<dyn Composer>)
        }
        _ => None,
    })
}

/// Repairs audio files left by interrupted recordings and records what was found:
/// `partial` (header fixed from the file length) or `missing`.
pub fn reconcile_audio(storage: &Storage) -> usize {
    let Ok(pending) = storage.unreconciled_audio() else {
        return 0;
    };
    for (id, path) in &pending {
        let path = std::path::Path::new(path);
        if !path.exists() {
            storage.update_audio(id, "missing", None);
            continue;
        }
        match wav::repair(path) {
            Ok(samples) => storage.update_audio(id, "partial", Some(samples as i64)),
            Err(_) => storage.update_audio(id, "failed", None),
        }
    }
    pending.len()
}

#[derive(Debug, Clone)]
pub struct StartRequest {
    pub recording_id: String,
    pub document_id: String,
    /// The character before the region is not whitespace.
    pub lead: bool,
    /// Read-only text before the region.
    pub context: String,
    pub initial_position: i64,
    pub prefs: Resolved,
}

#[derive(Debug)]
pub enum Command {
    Start(StartRequest),
    Stop,
    Context {
        recording_id: String,
        context: String,
    },
}

#[derive(Clone)]
pub struct Engine {
    tx: mpsc::UnboundedSender<Command>,
}

impl Engine {
    pub fn start(handle: &tokio::runtime::Handle, ctx: EngineContext) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        handle.spawn(run(rx, ctx));
        Self { tx }
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }
}

async fn run(mut rx: mpsc::UnboundedReceiver<Command>, ctx: EngineContext) {
    let mut active: Option<(String, mpsc::UnboundedSender<Command>)> = None;
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<String>();
    loop {
        tokio::select! {
            cmd = rx.recv() => match cmd {
                None => break,
                Some(Command::Start(req)) => {
                    if active.is_some() {
                        (ctx.sink)(EngineEvent::Notice { level: "warning", message: "A recording is already in progress.".into() });
                        (ctx.sink)(EngineEvent::RegionClosed { document_id: req.document_id, recording_id: req.recording_id });
                        continue;
                    }
                    let (stx, srx) = mpsc::unbounded_channel();
                    active = Some((req.recording_id.clone(), stx));
                    let ctx = ctx.clone();
                    let done = done_tx.clone();
                    tokio::spawn(async move {
                        let id = req.recording_id.clone();
                        Session::run(ctx, req, srx).await;
                        let _ = done.send(id);
                    });
                }
                Some(cmd) => {
                    if let Some((_, stx)) = &active {
                        let _ = stx.send(cmd);
                    }
                }
            },
            Some(id) = done_rx.recv() => {
                if active.as_ref().is_some_and(|(a, _)| *a == id) {
                    active = None;
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Recording,
    Finishing,
}

/// Latency and outcome measurements for one recording. Lags are measured from when the audio
/// was captured (sample time on the capture clock), so they include endpointing delay.
#[derive(Default)]
struct Metrics {
    /// Capture-clock time of sample 0.
    audio_zero: Option<Instant>,
    first_text_ms: Option<f64>,
    interim_lag: Vec<f64>,
    final_lag: Vec<f64>,
    edit_latency: Vec<f64>,
    correction_delay: Vec<f64>,
    /// A final segment containing a complete correction: when it arrived, and when its last
    /// word was spoken (capture clock).
    pending_correction: Option<(Instant, Instant)>,
    outcomes: std::collections::BTreeMap<&'static str, u32>,
}

fn stats(v: &[f64]) -> serde_json::Value {
    if v.is_empty() {
        return serde_json::Value::Null;
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let at = |q: f64| s[((s.len() - 1) as f64 * q).round() as usize].round();
    json!({"count": s.len(), "median": at(0.5), "p95": at(0.95), "max": at(1.0)})
}

impl Metrics {
    fn sample_lag_ms(&self, now: Instant, sample: u64) -> Option<f64> {
        let zero = self.audio_zero?;
        let captured = zero + Duration::from_secs_f64(sample as f64 / TRANSPORT_RATE as f64);
        Some(now.saturating_duration_since(captured).as_secs_f64() * 1000.0)
    }

    fn to_json(&self) -> serde_json::Value {
        json!({
            "firstTextLagMs": self.first_text_ms.map(f64::round),
            "interimLagMs": stats(&self.interim_lag),
            "finalLagMs": stats(&self.final_lag),
            "editLatencyMs": stats(&self.edit_latency),
            "correctionDelayMs": stats(&self.correction_delay),
            "edits": self.outcomes,
        })
    }
}

struct InFlight {
    op_id: String,
    expected: String,
    started: Instant,
}

struct Connection {
    epoch: u32,
    input: mpsc::Sender<AsrInput>,
    op: OperationRow,
}

struct Session {
    ctx: EngineContext,
    req: StartRequest,
    region: VoiceRegion,
    limits: RegionLimits,
    rec: RecordingRow,
    phase: Phase,
    finish_deadline: Option<Instant>,
    failures: Vec<String>,

    // Audio
    capture: Option<Capture>,
    received: u64,
    ring: VecDeque<AudioChunk>,
    ring_limit: u64,
    audio_ended: bool,
    wav: Option<std::sync::mpsc::Sender<wav::WavMsg>>,
    /// The audio file hasn't reported its final state yet.
    wav_pending: bool,
    audio_summary: (u64, u64, u32),

    // Recognition
    asr: Arc<dyn AsrProvider>,
    asr_config: AsrConfig,
    conn: Option<Connection>,
    epoch: u32,
    epoch_start: u64,
    first_asr_op: Option<String>,
    reconnects: u32,
    reconnect_at: Option<Instant>,
    asr_done: bool,
    last_final_end: u64,
    segment_seq: u64,
    interim_rev: i64,

    // Composition
    composer: Option<Arc<dyn Composer>>,
    in_flight: Option<InFlight>,
    last_sent_version: Option<u64>,
    dirty: bool,
    due: Option<Instant>,
    last_request_at: Option<Instant>,
    defers: i64,
    context: String,
    edit_failures: u32,
    /// Requests abandoned for a newer one, by operation ID, with their start time.
    superseded: std::collections::HashMap<String, Instant>,
    metrics: Metrics,
}

type AsrTagged = (u32, AsrOutput);

impl Session {
    async fn run(
        ctx: EngineContext,
        req: StartRequest,
        mut cmds: mpsc::UnboundedReceiver<Command>,
    ) {
        let sink = ctx.sink.clone();
        let p = req.prefs.clone();
        let snapshot_id = uuid::Uuid::new_v4().to_string();
        let mut snapshot = p.0.clone();
        snapshot.insert("transport.sample_rate".into(), json!(TRANSPORT_RATE));
        snapshot.insert("transport.encoding".into(), json!("linear16 mono"));
        ctx.storage.history(HistoryItem::Snapshot {
            id: snapshot_id.clone(),
            json: serde_json::to_string_pretty(&snapshot).unwrap_or_default(),
        });
        let rec = RecordingRow {
            id: req.recording_id.clone(),
            document_id: req.document_id.clone(),
            status: "recording".into(),
            started_at: now_ms(),
            settings_snapshot_id: Some(snapshot_id),
            initial_position: Some(req.initial_position),
            audio_status: "none".into(),
            sample_rate: Some(TRANSPORT_RATE.into()),
            channels: Some(1),
            ..Default::default()
        };
        ctx.storage.history(HistoryItem::Recording(rec.clone()));

        let fail = |message: String, rec: &mut RecordingRow| {
            rec.status = "failed".into();
            rec.ended_at = Some(now_ms());
            rec.note = Some(message.clone());
            ctx.storage.history(HistoryItem::Recording(rec.clone()));
            sink(EngineEvent::Notice {
                level: "error",
                message,
            });
            sink(EngineEvent::RegionClosed {
                document_id: req.document_id.clone(),
                recording_id: req.recording_id.clone(),
            });
            sink(EngineEvent::State {
                state: "idle",
                recording_id: None,
                document_id: None,
            });
        };
        let mut rec = rec;

        let asr_name = p.str("asr.provider").to_owned();
        let Some(asr) = (ctx.asr)(&asr_name) else {
            fail(
                format!("Recognition provider {asr_name:?} isn't available."),
                &mut rec,
            );
            return;
        };
        if asr_name == "deepgram" && ctx.keys.get(Provider::Deepgram).is_none() {
            fail(
                "Add your Deepgram API key in Preferences to dictate.".into(),
                &mut rec,
            );
            return;
        }
        let mut composer = None;
        if p.bool("comp.enabled") {
            let name = p.str("comp.provider");
            composer = (ctx.composer)(name);
            if composer.is_none() {
                sink(EngineEvent::Notice {
                    level: "warning",
                    message: format!(
                        "Editing provider {name:?} isn't available; corrections are off for this recording."
                    ),
                });
            } else if let Some(key) = Provider::for_composer(name)
                && ctx.keys.get(key).is_none()
            {
                composer = None;
                sink(EngineEvent::Notice {
                    level: "warning",
                    message: format!(
                        "No {}: spoken corrections are off for this recording.",
                        key.label()
                    ),
                });
            }
        }

        let (audio_tx, mut audio_rx) = mpsc::channel(256);
        let batch_ms = p.int("audio.batch_ms").clamp(10, 1000) as u32;
        let (capture, info) =
            match audio::start(p.str("audio.device"), TRANSPORT_RATE, batch_ms, audio_tx) {
                Ok(x) => x,
                Err(e) => {
                    fail(e, &mut rec);
                    return;
                }
            };
        rec.device = Some(format!(
            "{} ({} Hz, {} ch, {})",
            info.device, info.device_rate, info.device_channels, info.sample_format
        ));

        let (wav_status_tx, mut wav_status_rx) = mpsc::unbounded_channel();
        let mut wav_tx = None;
        if p.bool("audio.retain") {
            let path = wav::path_for(
                std::path::Path::new(p.str("audio.directory")),
                &req.recording_id,
            );
            let status_tx = wav_status_tx.clone();
            match wav::start(&path, TRANSPORT_RATE, move |s| {
                let _ = status_tx.send(s);
            }) {
                Ok(tx) => {
                    wav_tx = Some(tx);
                    rec.audio_path = Some(path.to_string_lossy().into_owned());
                    rec.audio_status = "recording".into();
                }
                Err(e) => {
                    rec.audio_status = "failed".into();
                    rec.note = Some(e.clone());
                    sink(EngineEvent::Notice {
                        level: "warning",
                        message: format!("{e}. Dictation continues without saving audio."),
                    });
                }
            }
        }
        ctx.storage.history(HistoryItem::Recording(rec.clone()));
        drop(wav_status_tx);

        let limits = RegionLimits {
            max_sentences: p.int("comp.region_sentences").max(1) as usize,
            max_words: p.int("comp.region_words").max(1) as usize,
            max_seconds: p.int("comp.region_seconds").max(1) as f64,
            sample_rate: TRANSPORT_RATE,
        };
        let mut s = Session {
            region: VoiceRegion::new(&req.recording_id, req.lead),
            limits,
            rec,
            phase: Phase::Recording,
            finish_deadline: None,
            failures: Vec::new(),
            capture: Some(capture),
            received: 0,
            ring: VecDeque::new(),
            ring_limit: p.int("audio.recovery_buffer_s").max(1) as u64 * TRANSPORT_RATE as u64,
            audio_ended: false,
            wav_pending: wav_tx.is_some(),
            wav: wav_tx,
            audio_summary: (0, 0, 0),
            asr_config: AsrConfig::from_prefs(&p, TRANSPORT_RATE),
            asr,
            conn: None,
            epoch: 0,
            epoch_start: 0,
            first_asr_op: None,
            reconnects: 0,
            reconnect_at: None,
            asr_done: false,
            last_final_end: 0,
            segment_seq: 0,
            interim_rev: 0,
            composer,
            in_flight: None,
            last_sent_version: None,
            dirty: false,
            due: None,
            last_request_at: None,
            defers: 0,
            context: req.context.clone(),
            edit_failures: 0,
            superseded: std::collections::HashMap::new(),
            metrics: Metrics::default(),
            ctx,
            req,
        };
        s.local_event(
            "recording_started",
            "Recording started",
            Some(json!({
                "device": s.rec.device, "audioPath": s.rec.audio_path, "lead": s.req.lead
            })),
        );
        sink(EngineEvent::State {
            state: "recording",
            recording_id: Some(s.req.recording_id.clone()),
            document_id: Some(s.req.document_id.clone()),
        });

        let (asr_tx, mut asr_rx) = mpsc::channel::<AsrTagged>(256);
        let (comp_tx, mut comp_rx) = mpsc::unbounded_channel::<(String, Option<EditOutcome>)>();
        s.connect(&asr_tx, 0);

        loop {
            // A due edit waits for the in-flight one; its response wakes the loop, so don't
            // spin on a due time that has already passed.
            let due = s.due.filter(|_| s.in_flight.is_none());
            let wake = [due, s.reconnect_at, s.finish_deadline]
                .into_iter()
                .flatten()
                .min()
                .unwrap_or_else(|| Instant::now() + Duration::from_secs(3600));
            tokio::select! {
                cmd = cmds.recv() => match cmd {
                    Some(Command::Stop) | None => s.begin_finish(),
                    Some(Command::Context { recording_id, context }) => {
                        if recording_id == s.req.recording_id {
                            s.context = context;
                        }
                    }
                    Some(Command::Start(_)) => {}
                },
                Some(ev) = audio_rx.recv() => s.on_audio(ev),
                Some((epoch, out)) = asr_rx.recv() => s.on_asr(epoch, out),
                Some((op, outcome)) = comp_rx.recv() => s.on_composition(&op, outcome),
                Some(status) = wav_status_rx.recv() => s.on_wav(status),
                _ = tokio::time::sleep_until(wake) => {}
            }
            let now = Instant::now();
            if s.reconnect_at.is_some_and(|t| now >= t) {
                s.reconnect_at = None;
                s.reconnect(&asr_tx);
            }
            if s.finish_deadline.is_some_and(|t| now >= t) {
                s.complete(true);
                break;
            }
            s.maybe_request(&comp_tx);
            if s.ready_to_complete() {
                s.complete(false);
                break;
            }
        }
    }

    fn sink(&self, e: EngineEvent) {
        (self.ctx.sink)(e)
    }

    fn emit(&self, update: Option<RegionUpdate>) {
        if let Some(update) = update {
            self.sink(EngineEvent::Region {
                document_id: self.req.document_id.clone(),
                update,
            });
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn event(
        &self,
        direction: &str,
        kind: &str,
        operation_id: Option<&str>,
        summary: String,
        payload: Option<String>,
        normalized: Option<serde_json::Value>,
        span: Option<(u64, u64)>,
    ) -> i64 {
        self.ctx.storage.event(EventRow {
            document_id: Some(self.req.document_id.clone()),
            recording_id: Some(self.req.recording_id.clone()),
            operation_id: operation_id.map(str::to_owned),
            direction: direction.into(),
            kind: kind.into(),
            summary: Some(summary),
            payload,
            normalized: normalized.map(|n| n.to_string()),
            audio_start: span.map(|s| s.0 as i64),
            audio_end: span.map(|s| s.1 as i64),
            ..Default::default()
        })
    }

    fn local_event(&self, kind: &str, summary: &str, detail: Option<serde_json::Value>) {
        self.event("local", kind, None, summary.to_owned(), None, detail, None);
    }

    // ----- Audio -----

    fn on_audio(&mut self, ev: AudioEvent) {
        match ev {
            AudioEvent::Chunk(chunk) => {
                self.received = chunk.end_sample();
                if self.metrics.audio_zero.is_none() {
                    let age =
                        Duration::from_secs_f64(chunk.end_sample() as f64 / TRANSPORT_RATE as f64);
                    self.metrics.audio_zero = Instant::now().checked_sub(age);
                }
                if let Some(wav) = &self.wav {
                    let _ = wav.send(wav::WavMsg::Samples(chunk.samples.clone()));
                }
                if let Some(conn) = &self.conn
                    && conn
                        .input
                        .try_send(AsrInput::Audio(chunk.samples.clone()))
                        .is_err()
                {
                    // The connection can't keep up; restart it and replay from the ring.
                    self.event(
                        "local",
                        "asr_backpressure",
                        None,
                        "Recognition fell behind; reconnecting".into(),
                        None,
                        None,
                        None,
                    );
                    self.conn = None;
                    self.reconnect_at = Some(Instant::now());
                }
                let (start, end, n) = &mut self.audio_summary;
                if *n == 0 {
                    *start = chunk.first_sample;
                }
                *end = chunk.end_sample();
                *n += 1;
                if *end - *start >= TRANSPORT_RATE as u64 {
                    let (a, b, n) = (*start, *end, *n);
                    self.audio_summary = (0, 0, 0);
                    self.event(
                        "out",
                        "audio",
                        self.conn.as_ref().map(|c| c.op.id.as_str()),
                        format!(
                            "{n} batches, {} bytes PCM16 at {} Hz",
                            (b - a) * 2,
                            TRANSPORT_RATE
                        ),
                        None,
                        Some(json!({"batches": n, "bytes": (b - a) * 2, "encoding": "linear16"})),
                        Some((a, b)),
                    );
                }
                self.ring.push_back(chunk);
                while self.ring.len() > 1
                    && self.received - self.ring.front().map_or(0, |c| c.first_sample)
                        > self.ring_limit
                {
                    self.ring.pop_front();
                }
            }
            AudioEvent::Level(l) => self.sink(EngineEvent::Level(l)),
            AudioEvent::Overflow { at_sample, frames } => {
                self.event(
                    "local",
                    "audio_overflow",
                    None,
                    format!("Audio processing fell behind; {frames} frames replaced with silence"),
                    None,
                    None,
                    Some((at_sample, at_sample)),
                );
                self.failures.push("audio overflow".into());
            }
            AudioEvent::Error(e) => {
                self.local_event("audio_error", &e, None);
                self.sink(EngineEvent::Notice {
                    level: "warning",
                    message: e,
                });
            }
            AudioEvent::Ended => {
                self.audio_ended = true;
                self.capture = None;
                self.local_event(
                    "audio_ended",
                    "Capture stopped",
                    Some(json!({"samples": self.received})),
                );
                if let Some(wav) = self.wav.take() {
                    let _ = wav.send(wav::WavMsg::Finish);
                }
                if let Some(conn) = &self.conn {
                    send_controls(&conn.input, vec![AsrInput::Finalize, AsrInput::Close]);
                } else if self.reconnect_at.is_none() {
                    self.asr_done = true;
                }
            }
        }
    }

    fn on_wav(&mut self, status: wav::WavStatus) {
        match status {
            wav::WavStatus::Progress(n) => self.rec.samples = n as i64,
            wav::WavStatus::Finished(n) => {
                self.wav_pending = false;
                self.rec.samples = n as i64;
                self.rec.audio_status = "complete".into();
                self.ctx
                    .storage
                    .history(HistoryItem::Recording(self.rec.clone()));
            }
            wav::WavStatus::Failed { samples, message } => {
                self.wav_pending = false;
                self.rec.samples = samples as i64;
                self.rec.audio_status = "partial".into();
                self.rec.note = Some(format!("Audio file: {message}"));
                self.wav = None;
                self.ctx
                    .storage
                    .history(HistoryItem::Recording(self.rec.clone()));
                self.sink(EngineEvent::Notice {
                    level: "warning",
                    message: format!("Saving audio failed ({message}). Dictation continues."),
                });
            }
        }
    }

    // ----- Recognition -----

    fn connect(&mut self, out: &mpsc::Sender<AsrTagged>, replay_from: u64) {
        let epoch = self.epoch;
        // Retained audio from `replay_from`, merged into blocks of up to a second so that even
        // a long recovery buffer is a few hundred messages. The queue is sized to hold all of it
        // plus room for live audio; nothing of the replay may be dropped (it would also shift
        // every later provider timestamp off the recording's timeline).
        let mut replay: Vec<Vec<i16>> = Vec::new();
        let mut block: Vec<i16> = Vec::new();
        for chunk in &self.ring {
            if chunk.end_sample() <= replay_from {
                continue;
            }
            let skip = replay_from.saturating_sub(chunk.first_sample) as usize;
            block.extend_from_slice(&chunk.samples[skip..]);
            if block.len() >= TRANSPORT_RATE as usize {
                replay.push(std::mem::take(&mut block));
            }
        }
        if !block.is_empty() {
            replay.push(block);
        }
        let (in_tx, in_rx) = mpsc::channel(replay.len() + LIVE_QUEUE);
        let (raw_tx, mut raw_rx) = mpsc::channel(256);
        let key = self.ctx.keys.get(Provider::Deepgram);
        self.asr_config.stream_start_sample = replay_from;
        self.asr
            .connect(self.asr_config.clone(), key, in_rx, raw_tx);
        let out = out.clone();
        tokio::spawn(async move {
            while let Some(o) = raw_rx.recv().await {
                if out.send((epoch, o)).await.is_err() {
                    break;
                }
            }
        });
        let op = OperationRow {
            id: uuid::Uuid::new_v4().to_string(),
            document_id: self.req.document_id.clone(),
            recording_id: Some(self.req.recording_id.clone()),
            parent_id: self.first_asr_op.clone(),
            attempt: epoch as i64 + 1,
            kind: "asr_connection".into(),
            provider: Some(self.asr.name().into()),
            model: Some(self.asr_config.model.clone()),
            status: "running".into(),
            started_at: now_ms(),
            settings_snapshot_id: self.rec.settings_snapshot_id.clone(),
            request: Some(
                json!({
                    "epoch": epoch,
                    "epochStartSample": replay_from,
                    "config": format!("{:?}", self.asr_config),
                })
                .to_string(),
            ),
            ..Default::default()
        };
        if self.first_asr_op.is_none() {
            self.first_asr_op = Some(op.id.clone());
        }
        self.ctx.storage.history(HistoryItem::Operation(op.clone()));
        self.epoch_start = replay_from;
        for samples in replay {
            // Can't fail: the queue was sized for the replay and is still empty.
            if in_tx.try_send(AsrInput::Audio(samples)).is_err() {
                self.failures.push("recognition replay incomplete".into());
            }
        }
        if self.audio_ended {
            send_controls(&in_tx, vec![AsrInput::Finalize, AsrInput::Close]);
        }
        self.sink(EngineEvent::Status(if epoch == 0 {
            "Connecting…".into()
        } else {
            "Reconnecting…".into()
        }));
        self.conn = Some(Connection {
            epoch,
            input: in_tx,
            op,
        });
    }

    fn reconnect(&mut self, out: &mpsc::Sender<AsrTagged>) {
        self.epoch += 1;
        let ring_start = self.ring.front().map_or(self.received, |c| c.first_sample);
        let mut from = self.last_final_end;
        if from < ring_start {
            let lost = (ring_start - from) as f64 / TRANSPORT_RATE as f64;
            self.event(
                "local",
                "gap",
                None,
                format!("{lost:.1} s of speech could not be resent after the disconnect"),
                None,
                Some(json!({"from": from, "to": ring_start})),
                Some((from, ring_start)),
            );
            self.sink(EngineEvent::Notice {
                level: "warning",
                message: format!("Recognition reconnected, but {lost:.1} s of speech was lost."),
            });
            self.failures.push("recognition gap".into());
            from = ring_start;
        }
        self.local_event(
            "reconnect",
            &format!(
                "Reconnecting (epoch {}), replaying from sample {from}",
                self.epoch
            ),
            None,
        );
        self.connect(out, from);
    }

    fn samples_at(&self, seconds: f64) -> u64 {
        self.epoch_start + (seconds.max(0.0) * TRANSPORT_RATE as f64).round() as u64
    }

    fn on_asr(&mut self, epoch: u32, out: AsrOutput) {
        let current = self.conn.as_ref().is_some_and(|c| c.epoch == epoch);
        let op_id = self
            .conn
            .as_ref()
            .filter(|_| current)
            .map(|c| c.op.id.clone());
        let obsolete = if current {
            ""
        } else {
            "(obsolete connection) "
        };
        match out {
            AsrOutput::Opened { request_id, url } => {
                if let Some(conn) = self.conn.as_mut().filter(|_| current) {
                    conn.op.request_id = request_id.clone();
                    self.ctx
                        .storage
                        .history(HistoryItem::Operation(conn.op.clone()));
                }
                self.event(
                    "in",
                    "open",
                    op_id.as_deref(),
                    format!("{obsolete}Connected"),
                    Some(url),
                    Some(json!({"requestId": request_id})),
                    None,
                );
                if current {
                    self.sink(EngineEvent::Status(String::new()));
                }
            }
            AsrOutput::Sent { kind, payload } => {
                self.event(
                    "out",
                    &kind,
                    op_id.as_deref(),
                    format!("{obsolete}{kind}"),
                    Some(payload),
                    None,
                    None,
                );
            }
            AsrOutput::Message { raw, parsed } => {
                let (kind, summary, span) = match &parsed {
                    AsrMessage::Transcript {
                        text,
                        start_s,
                        duration_s,
                        is_final,
                        speech_final,
                        ..
                    } => (
                        "Results",
                        format!(
                            "{}{}{} {:?}",
                            obsolete,
                            if *is_final { "final" } else { "interim" },
                            if *speech_final { " (speech final)" } else { "" },
                            text
                        ),
                        Some((
                            self.samples_at(*start_s),
                            self.samples_at(start_s + duration_s),
                        )),
                    ),
                    AsrMessage::UtteranceEnd { .. } => {
                        ("UtteranceEnd", format!("{obsolete}Utterance end"), None)
                    }
                    AsrMessage::SpeechStarted { .. } => {
                        ("SpeechStarted", format!("{obsolete}Speech started"), None)
                    }
                    AsrMessage::Metadata { .. } => {
                        ("Metadata", format!("{obsolete}Metadata"), None)
                    }
                    AsrMessage::Error { message } => {
                        ("Error", format!("{obsolete}Error: {message}"), None)
                    }
                    AsrMessage::Other => ("Other", format!("{obsolete}Message"), None),
                };
                let seq = self.event(
                    "in",
                    kind,
                    op_id.as_deref(),
                    summary,
                    Some(raw),
                    Some(json!(format!("{parsed:?}"))),
                    span,
                );
                if current {
                    self.handle_message(parsed, seq, span);
                }
            }
            AsrOutput::Closed { clean, reason } => {
                self.event(
                    "in",
                    "close",
                    op_id.as_deref(),
                    format!(
                        "{obsolete}Closed{}: {reason}",
                        if clean { "" } else { " unexpectedly" }
                    ),
                    None,
                    None,
                    None,
                );
                if !current {
                    return;
                }
                let Some(mut conn) = self.conn.take() else {
                    return;
                };
                conn.op.status = if clean {
                    "completed".into()
                } else {
                    "failed".into()
                };
                conn.op.ended_at = Some(now_ms());
                conn.op.error = (!clean).then(|| reason.clone());
                conn.op.outcome = Some(reason.clone());
                self.ctx.storage.history(HistoryItem::Operation(conn.op));
                if self.audio_ended || clean {
                    if !clean {
                        // Lost while finishing: the text so far stays, but say so.
                        self.failures
                            .push(format!("recognition ended early while finishing: {reason}"));
                        self.sink(EngineEvent::Notice {
                            level: "warning",
                            message: format!(
                                "Recognition stopped before it finished ({reason}); the last words may be missing."
                            ),
                        });
                    }
                    self.asr_done = true;
                    self.sink(EngineEvent::Status(String::new()));
                    return;
                }
                // Unexpected loss while recording: provisional text is withdrawn and the
                // connection restarts from the end of the last final result.
                let u = self.region.set_provisional("", Some("reconnect".into()));
                self.emit(u);
                let max = self.req.prefs.int("life.reconnect_attempts").max(0) as u32;
                if self.reconnects >= max {
                    self.failures.push(format!("recognition stopped: {reason}"));
                    self.sink(EngineEvent::Notice {
                        level: "error",
                        message: format!("Recognition stopped ({reason}). Your text is kept."),
                    });
                    self.asr_done = true;
                    self.begin_finish();
                    return;
                }
                let backoff = self.req.prefs.int("life.reconnect_backoff_ms").max(1) as u64
                    * 2u64.saturating_pow(self.reconnects);
                self.reconnects += 1;
                self.sink(EngineEvent::Status(format!(
                    "Connection lost; reconnecting (attempt {})…",
                    self.reconnects
                )));
                self.reconnect_at =
                    Some(Instant::now() + Duration::from_millis(backoff.min(30_000)));
            }
        }
    }

    fn handle_message(&mut self, msg: AsrMessage, seq: i64, span: Option<(u64, u64)>) {
        let now = Instant::now();
        let debounce = self.req.prefs.ms("comp.debounce_ms");
        match msg {
            AsrMessage::Transcript {
                text,
                is_final,
                speech_final,
                confidence,
                words,
                ..
            } => {
                self.reconnects = 0;
                let (start, end) = span.unwrap_or((self.last_final_end, self.received));
                // Lag is measured from the end of the last spoken word: the segment itself can
                // extend over the silence the provider waited through before finalizing.
                let speech_end = words
                    .as_ref()
                    .and_then(|w| w.as_array()?.last()?.get("end")?.as_f64())
                    .map_or(end, |e| self.samples_at(e).min(end));
                if !text.trim().is_empty()
                    && let Some(lag) = self.metrics.sample_lag_ms(now, speech_end)
                {
                    self.metrics.first_text_ms.get_or_insert(lag);
                    if is_final {
                        self.metrics.final_lag.push(lag);
                    } else {
                        self.metrics.interim_lag.push(lag);
                    }
                }
                if is_final && contains_correction(&text) {
                    let spoken = self.metrics.audio_zero.map_or(now, |z| {
                        z + Duration::from_secs_f64(speech_end as f64 / TRANSPORT_RATE as f64)
                    });
                    self.metrics.pending_correction.get_or_insert((now, spoken));
                }
                let segment_id = format!("e{}-{}", self.epoch, self.segment_seq);
                if !is_final {
                    self.interim_rev += 1;
                    self.ctx
                        .storage
                        .history(HistoryItem::Transcript(TranscriptRow {
                            recording_id: self.req.recording_id.clone(),
                            segment_id,
                            revision: self.interim_rev,
                            event_app_seq: Some(seq),
                            epoch: Some(self.epoch.into()),
                            text: text.clone(),
                            finality: "interim".into(),
                            audio_start: Some(start as i64),
                            audio_end: Some(end as i64),
                            confidence,
                            words: words.map(|w| w.to_string()),
                        }));
                    let u = self
                        .region
                        .set_provisional(&text, Some(format!("event:{seq}")));
                    self.emit(u);
                    return;
                }
                self.last_final_end = self.last_final_end.max(end);
                if !text.trim().is_empty() {
                    self.segment_seq += 1;
                    self.interim_rev += 1;
                    self.ctx
                        .storage
                        .history(HistoryItem::Transcript(TranscriptRow {
                            recording_id: self.req.recording_id.clone(),
                            segment_id: segment_id.clone(),
                            revision: self.interim_rev,
                            event_app_seq: Some(seq),
                            epoch: Some(self.epoch.into()),
                            text: text.clone(),
                            finality: if speech_final {
                                "speech_final".into()
                            } else {
                                "final".into()
                            },
                            audio_start: Some(start as i64),
                            audio_end: Some(end as i64),
                            confidence,
                            words: words.map(|w| w.to_string()),
                        }));
                    let u = self.region.append_final(
                        Segment {
                            id: segment_id,
                            text: text.clone(),
                            start_sample: start,
                            end_sample: end,
                        },
                        Some(format!("event:{seq}")),
                    );
                    self.emit(u);
                    self.dirty = true;
                    let soon = now + debounce;
                    let due = if speech_final || contains_correction(&text) {
                        soon
                    } else {
                        let cadence = self.req.prefs.ms("comp.cadence_ms");
                        self.last_request_at
                            .map_or(soon, |t| (t + cadence).max(soon))
                    };
                    self.due = Some(self.due.map_or(due, |d| d.min(due)));
                    if contains_correction(&text) {
                        self.supersede_in_flight();
                    }
                } else if !self.region.provisional().is_empty() {
                    let u = self
                        .region
                        .set_provisional("", Some(format!("event:{seq}")));
                    self.emit(u);
                }
                if speech_final && self.dirty {
                    self.due = Some(now + debounce);
                }
                if self.composer.is_none() {
                    self.region.mark_all_reviewed();
                    let u = self.region.release_over_limit(&self.limits);
                    self.emit(u);
                }
            }
            AsrMessage::UtteranceEnd { .. } => {
                if self.dirty {
                    self.due = Some(now + debounce);
                }
            }
            AsrMessage::Error { message } => {
                self.sink(EngineEvent::Notice {
                    level: "warning",
                    message: format!("Recognition: {message}"),
                });
            }
            AsrMessage::SpeechStarted { .. } | AsrMessage::Metadata { .. } | AsrMessage::Other => {}
        }
    }

    // ----- Composition -----

    fn maybe_request(&mut self, comp_tx: &mpsc::UnboundedSender<(String, Option<EditOutcome>)>) {
        let Some(composer) = self.composer.clone() else {
            self.dirty = false;
            self.due = None;
            return;
        };
        if self.in_flight.is_some() {
            return;
        }
        // Between requests is the only time text may leave the region.
        let u = self.region.release_over_limit(&self.limits);
        self.emit(u);
        let now = Instant::now();
        if !self.dirty || self.due.is_none_or(|d| now < d) {
            return;
        }
        self.due = None;
        self.dirty = false;
        let composed = self.region.composed().to_owned();
        if composed.trim().is_empty() {
            return;
        }
        if self.last_sent_version == Some(self.region.composed_version()) {
            self.local_event(
                "edit_skipped",
                "Edit skipped: text unchanged since the last request",
                None,
            );
            return;
        }
        let p = &self.req.prefs;
        let op_id = uuid::Uuid::new_v4().to_string();
        let request = EditRequest {
            operation_id: op_id.clone(),
            recording_id: self.req.recording_id.clone(),
            composed_version: self.region.composed_version(),
            watermark: self.region.consumed(),
            context: self.context.clone(),
            segments: self
                .region
                .segments()
                .iter()
                .map(|s| s.text.clone())
                .collect(),
            composed: composed.clone(),
            vocabulary: self.asr_config.vocabulary.clone(),
            model: p
                .str(if composer.name() == "anthropic" {
                    "comp.anthropic_model"
                } else {
                    "comp.model"
                })
                .to_owned(),
            reasoning: p
                .str(if composer.name() == "anthropic" {
                    "comp.anthropic_effort"
                } else {
                    "comp.reasoning"
                })
                .to_owned(),
            max_output_tokens: p.int("comp.max_output_tokens"),
            thinking: p.bool("comp.anthropic_thinking"),
        };
        self.ctx
            .storage
            .history(HistoryItem::Operation(OperationRow {
                id: op_id.clone(),
                document_id: self.req.document_id.clone(),
                recording_id: Some(self.req.recording_id.clone()),
                attempt: 1,
                kind: "composition".into(),
                provider: Some(composer.name().into()),
                model: Some(request.model.clone()),
                status: "running".into(),
                started_at: now_ms(),
                region_revision: Some(request.composed_version as i64),
                source_watermark: Some(request.watermark as i64),
                settings_snapshot_id: self.rec.settings_snapshot_id.clone(),
                request: serde_json::to_string_pretty(&request).ok(),
                ..Default::default()
            }));
        self.event(
            "out",
            "edit_request",
            Some(&op_id),
            format!(
                "Edit request for {} words",
                crate::text::word_count(&composed)
            ),
            None,
            None,
            None,
        );
        self.last_sent_version = Some(request.composed_version);
        self.last_request_at = Some(now);
        self.in_flight = Some(InFlight {
            op_id: op_id.clone(),
            expected: composed,
            started: now,
        });
        let key = Provider::for_composer(composer.name()).and_then(|p| self.ctx.keys.get(p));
        let timeout = p.ms("comp.timeout_ms");
        let tx = comp_tx.clone();
        tokio::spawn(async move {
            let outcome = tokio::time::timeout(timeout, composer.edit(request, key))
                .await
                .ok();
            let _ = tx.send((op_id, outcome));
        });
    }

    /// A correction just arrived: an in-flight request can't include it, so stop waiting for it
    /// and let a new request go out now. The old one finishes in the background and is
    /// recorded as superseded.
    fn supersede_in_flight(&mut self) {
        if !self.req.prefs.bool("comp.supersede_on_correction") {
            return;
        }
        let Some(flight) = self.in_flight.take() else {
            return;
        };
        self.event(
            "local",
            "edit_superseded",
            Some(&flight.op_id),
            "A correction arrived; requesting a new edit instead of waiting for this one".into(),
            None,
            None,
            None,
        );
        *self.metrics.outcomes.entry("superseded").or_default() += 1;
        self.superseded.insert(flight.op_id, flight.started);
    }

    fn on_composition(&mut self, op_id: &str, outcome: Option<EditOutcome>) {
        if let Some(started) = self.superseded.remove(op_id) {
            // Keep the full exchange (and its usage) in history; never apply it.
            let latency = started.elapsed().as_millis() as i64;
            let mut op = OperationRow {
                id: op_id.to_owned(),
                document_id: self.req.document_id.clone(),
                recording_id: Some(self.req.recording_id.clone()),
                kind: "composition".into(),
                status: "superseded".into(),
                started_at: now_ms() - latency,
                ended_at: Some(now_ms()),
                latency_ms: Some(latency),
                outcome: Some(
                    "Superseded by a request that includes a correction; not applied".into(),
                ),
                ..Default::default()
            };
            if let Some(o) = outcome {
                op.request = Some(o.request_body);
                op.response = o.raw_response;
                op.usage = o.usage.map(|u| u.to_string());
                op.request_id = o.request_id;
                op.error = o.result.err();
            }
            self.ctx.storage.history(HistoryItem::Operation(op));
            return;
        }
        let Some(flight) = self.in_flight.take_if(|f| f.op_id == op_id) else {
            self.event(
                "in",
                "edit_late",
                Some(op_id),
                "Response arrived after the request was abandoned; ignored".into(),
                None,
                None,
                None,
            );
            return;
        };
        let latency = flight.started.elapsed().as_millis() as i64;
        let mut op = OperationRow {
            id: op_id.to_owned(),
            document_id: self.req.document_id.clone(),
            recording_id: Some(self.req.recording_id.clone()),
            kind: "composition".into(),
            started_at: now_ms() - latency,
            ended_at: Some(now_ms()),
            latency_ms: Some(latency),
            ..Default::default()
        };
        // Whether the model is done with this text, so it may leave the region.
        let mut reviewed = true;
        let (status, outcome_text) = match outcome {
            None => ("timeout", "Timed out; the recognized text stays".to_owned()),
            Some(o) => {
                op.request = Some(o.request_body.clone());
                op.response = o.raw_response.clone();
                op.usage = o.usage.as_ref().map(|u| u.to_string());
                op.request_id = o.request_id.clone();
                match o.result {
                    Err(e) => {
                        op.error = Some(e.clone());
                        self.edit_failures += 1;
                        if self.edit_failures == 1 {
                            self.sink(EngineEvent::Notice {
                                level: "warning",
                                message: format!("Correction request failed: {e}"),
                            });
                        }
                        ("failed", format!("Failed: {e}"))
                    }
                    Ok(Decision::NoChange) => {
                        self.defers = 0;
                        ("completed", "No change needed".to_owned())
                    }
                    Ok(Decision::Defer) => {
                        self.defers += 1;
                        if self.defers > self.req.prefs.int("comp.defer_limit") {
                            (
                                "completed",
                                "Deferred too often; the text stays as spoken".to_owned(),
                            )
                        } else {
                            reviewed = false;
                            ("completed", "Deferred until more speech arrives".to_owned())
                        }
                    }
                    Ok(Decision::Replace(text)) => {
                        self.defers = 0;
                        match validate_replacement(&flight.expected, &text) {
                            Err(e) => ("rejected", format!("Rejected: {e}")),
                            Ok(()) => match self.region.replace_composed_prefix(
                                &flight.expected,
                                &text,
                                Some(op_id.to_owned()),
                            ) {
                                Err(_) => {
                                    reviewed = false;
                                    (
                                        "stale",
                                        "Stale: the region changed during the request".to_owned(),
                                    )
                                }
                                Ok(None) => (
                                    "completed",
                                    "Replacement identical to the current text".to_owned(),
                                ),
                                Ok(Some(update)) => {
                                    self.emit(Some(update));
                                    // Only a request sent after the correction arrived applied it.
                                    if let Some((arrived, spoken)) = self.metrics.pending_correction
                                        && flight.started >= arrived
                                    {
                                        self.metrics
                                            .correction_delay
                                            .push(spoken.elapsed().as_secs_f64() * 1000.0);
                                    }
                                    ("applied", "Applied".to_owned())
                                }
                            },
                        }
                    }
                }
            }
        };
        if reviewed {
            // Text the model kept, rejected or couldn't process stays as is and may now leave
            // the region; a pending correction was handled one way or the other.
            self.region.mark_reviewed(&flight.expected);
            // An older request that didn't include the correction doesn't settle it.
            if self
                .metrics
                .pending_correction
                .is_some_and(|(arrived, _)| flight.started >= arrived)
            {
                self.metrics.pending_correction = None;
            }
        }
        self.metrics.edit_latency.push(latency as f64);
        *self.metrics.outcomes.entry(status).or_default() += 1;
        op.status = status.into();
        op.outcome = Some(outcome_text.clone());
        self.ctx.storage.history(HistoryItem::Operation(op));
        self.event(
            "in",
            "edit_result",
            Some(op_id),
            format!("{status}: {outcome_text}"),
            None,
            None,
            None,
        );
        // Text that arrived during the request is still pending (dirty); ask again soon.
        if self.dirty && self.due.is_none() {
            self.due = Some(Instant::now() + self.req.prefs.ms("comp.debounce_ms"));
        }
    }

    // ----- Lifecycle -----

    fn begin_finish(&mut self) {
        if self.phase == Phase::Finishing {
            return;
        }
        self.phase = Phase::Finishing;
        self.finish_deadline = Some(Instant::now() + self.req.prefs.ms("life.finalize_timeout_ms"));
        self.local_event(
            "stop",
            "Stop: finishing recognition and the last edit",
            None,
        );
        self.sink(EngineEvent::State {
            state: "finishing",
            recording_id: Some(self.req.recording_id.clone()),
            document_id: Some(self.req.document_id.clone()),
        });
        match &self.capture {
            Some(c) => c.stop(),
            None => self.audio_ended = true,
        }
        self.reconnect_at = None;
        if self.conn.is_none() {
            self.asr_done = true;
        }
    }

    fn ready_to_complete(&mut self) -> bool {
        if self.phase != Phase::Finishing
            || !self.audio_ended
            || !self.asr_done
            || self.in_flight.is_some()
        {
            return false;
        }
        if self.dirty && self.composer.is_some() {
            // One last edit for text that arrived since the previous request.
            self.due = Some(Instant::now());
            return false;
        }
        true
    }

    fn complete(&mut self, timed_out: bool) {
        if let Some(f) = self.in_flight.take() {
            self.ctx
                .storage
                .history(HistoryItem::Operation(OperationRow {
                    id: f.op_id.clone(),
                    document_id: self.req.document_id.clone(),
                    recording_id: Some(self.req.recording_id.clone()),
                    kind: "composition".into(),
                    status: "abandoned".into(),
                    started_at: now_ms(),
                    ended_at: Some(now_ms()),
                    outcome: Some("Abandoned when finishing timed out".into()),
                    ..Default::default()
                }));
        }
        self.conn = None;
        if let Some(c) = &self.capture {
            c.stop();
        }
        if let Some(wav) = self.wav.take() {
            let _ = wav.send(wav::WavMsg::Finish);
        }
        if self.wav_pending {
            // The writer finalizes on its own; reconciliation fixes the state next start if not.
            self.rec.audio_status = "partial".into();
        }
        let u = self.region.release_all();
        self.emit(u);
        self.sink(EngineEvent::RegionClosed {
            document_id: self.req.document_id.clone(),
            recording_id: self.req.recording_id.clone(),
        });
        self.rec.status = if timed_out {
            "incomplete".into()
        } else if self.failures.is_empty() {
            "completed".into()
        } else {
            "completed_with_errors".into()
        };
        if timed_out {
            self.rec.note = Some("Finishing timed out; available text was kept.".into());
            self.sink(EngineEvent::Notice {
                level: "warning",
                message: "Finishing took too long; the text so far was kept.".into(),
            });
        } else if !self.failures.is_empty() {
            self.rec.note = Some(self.failures.join("; "));
        }
        self.rec.ended_at = Some(now_ms());
        let metrics = self.metrics.to_json();
        self.rec.metrics = Some(metrics.to_string());
        self.event(
            "local",
            "metrics",
            None,
            "Latency and edit measurements".into(),
            None,
            Some(metrics),
            None,
        );
        if self.rec.audio_path.is_none() {
            self.rec.samples = self.received as i64;
        }
        self.ctx
            .storage
            .history(HistoryItem::Recording(self.rec.clone()));
        self.local_event(
            "recording_finished",
            &format!("Recording {}", self.rec.status),
            None,
        );
        self.sink(EngineEvent::Status(String::new()));
        self.sink(EngineEvent::State {
            state: "idle",
            recording_id: None,
            document_id: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asr::mock::MockAsr;
    use crate::composition::mock::MockComposer;
    use crate::editor::EditorModel;
    use crate::settings::{Paths, Preferences, schema};
    use std::sync::Mutex;

    fn prefs(overrides: &[(&str, serde_json::Value)]) -> Resolved {
        let paths = Paths {
            data_dir: "/tmp/batscribe-test".into(),
        };
        let mut p = Preferences::default();
        for (k, v) in overrides {
            p.stored.insert((*k).into(), v.clone());
        }
        // Mock providers and synthetic audio aren't in the schema without the dev flag, so
        // resolve first and then override.
        let mut r = p.resolved(&schema(&paths, &[]));
        r.insert("audio.device".into(), json!("synthetic"));
        r.insert("asr.provider".into(), json!("mock"));
        r.insert("comp.provider".into(), json!("mock"));
        for (k, v) in overrides {
            r.insert((*k).into(), v.clone());
        }
        Resolved(r)
    }

    struct Harness {
        events: Arc<Mutex<Vec<EngineEvent>>>,
        engine: Engine,
        storage: Storage,
        _dir: tempfile::TempDir,
    }

    fn harness(asr: MockAsr, edit_ms: u64) -> Harness {
        harness_with(
            asr,
            Arc::new(MockComposer {
                latency: Duration::from_millis(edit_ms),
            }),
        )
    }

    /// Returns preset decisions in order (then `no_change`) and records each request's text.
    struct Scripted {
        decisions: Mutex<std::collections::VecDeque<Decision>>,
        seen: Arc<Mutex<Vec<String>>>,
        latency: Duration,
    }

    impl Composer for Scripted {
        fn name(&self) -> &'static str {
            "scripted"
        }
        fn edit(
            &self,
            request: EditRequest,
            _key: Option<crate::credentials::ApiKey>,
        ) -> crate::composition::EditFuture {
            self.seen.lock().unwrap().push(request.composed.clone());
            let decision = self
                .decisions
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Decision::NoChange);
            let latency = self.latency;
            Box::pin(async move {
                tokio::time::sleep(latency).await;
                EditOutcome {
                    request_body: "{}".into(),
                    raw_response: Some(format!("{decision:?}")),
                    request_id: None,
                    usage: None,
                    result: Ok(decision),
                }
            })
        }
    }

    fn scripted(
        decisions: Vec<Decision>,
        latency_ms: u64,
    ) -> (Arc<dyn Composer>, Arc<Mutex<Vec<String>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let c = Scripted {
            decisions: Mutex::new(decisions.into()),
            seen: seen.clone(),
            latency: Duration::from_millis(latency_ms),
        };
        (Arc::new(c), seen)
    }

    fn harness_with(asr: MockAsr, composer: Arc<dyn Composer>) -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("db"), Arc::new(|_| {})).unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let ev = events.clone();
        let asr: Arc<dyn AsrProvider> = Arc::new(asr);
        let ctx = EngineContext {
            storage: storage.clone(),
            keys: Arc::new(KeyRing::default()),
            sink: Arc::new(move |e| ev.lock().unwrap().push(e)),
            asr: Arc::new(move |_| Some(asr.clone())),
            composer: Arc::new(move |_| Some(composer.clone())),
        };
        let engine = Engine::start(&tokio::runtime::Handle::current(), ctx);
        Harness {
            events,
            engine,
            storage,
            _dir: dir,
        }
    }

    /// Applies engine events to an editor model, as the GUI does.
    fn replay(events: &[EngineEvent], model: &mut EditorModel) -> bool {
        let mut closed = false;
        for e in events {
            match e {
                EngineEvent::Region { update, .. } => {
                    model
                        .apply_region(update, 0)
                        .expect("update applies in order");
                }
                EngineEvent::RegionClosed { recording_id, .. } => {
                    model.close_region(recording_id);
                    closed = true;
                }
                _ => {}
            }
        }
        closed
    }

    async fn dictate(h: &Harness, prefs: Resolved, seconds: u64) -> EditorModel {
        let mut model = EditorModel::new("Shopping:".into(), 0);
        let pos = model.start_position(0, false);
        let lead = model.needs_leading_space(pos);
        model.open_region("rec", pos, 0);
        h.engine.send(Command::Start(StartRequest {
            recording_id: "rec".into(),
            document_id: "doc".into(),
            lead,
            context: String::new(),
            initial_position: pos as i64,
            prefs,
        }));
        tokio::time::sleep(Duration::from_secs(seconds)).await;
        h.engine.send(Command::Stop);
        for _ in 0..200 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if h.events
                .lock()
                .unwrap()
                .iter()
                .any(|e| matches!(e, EngineEvent::State { state: "idle", .. }))
            {
                break;
            }
        }
        let events = h.events.lock().unwrap().clone();
        assert!(replay(&events, &mut model), "region closed");
        model
    }

    #[tokio::test(start_paused = true)]
    async fn end_to_end_dictation_with_correction() {
        let h = harness(MockAsr::new(crate::asr::mock::DEFAULT_SCRIPT), 300);
        let model = dictate(&h, prefs(&[]), 12).await;
        assert_eq!(
            model.text(),
            "Shopping: At the store, please buy six oranges. Then come home."
        );
        assert!(model.region().is_none());
        h.storage.flush(Duration::from_secs(5));
        let ops = h.storage.operations("rec").unwrap();
        assert!(
            ops.iter()
                .any(|o| o["kind"] == "composition" && o["status"] == "applied")
        );
        assert_eq!(
            h.storage.recording("rec").unwrap().unwrap()["status"],
            "completed"
        );
        let (events, total) = h.storage.events("rec", None, 0, 1000).unwrap();
        assert!(total > 10);
        assert!(events.iter().any(|e| e["kind"] == "Results"));
        // Provisional (interim) text was shown, and marked as such, before the final result.
        let mut m = EditorModel::new("Shopping:".into(), 0);
        m.open_region("rec", 9, 0);
        let mut saw_provisional = false;
        for e in h.events.lock().unwrap().iter() {
            if let EngineEvent::Region { update, .. } = e {
                m.apply_region(update, 0).unwrap();
                let r = m.region().unwrap();
                let tail = &m.text()[r.start + r.provisional_from..r.end];
                saw_provisional |= tail.contains("at the store please");
            }
        }
        assert!(saw_provisional);
    }

    #[tokio::test(start_paused = true)]
    async fn stop_mid_utterance_keeps_text_via_finalize() {
        let h = harness(
            MockAsr::new(&["one two three four five six seven eight."]),
            50,
        );
        let model = dictate(&h, prefs(&[]), 2).await;
        // Finalize turned the words heard so far into text.
        assert!(
            model.text().starts_with("Shopping: one two three"),
            "{}",
            model.text()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn disconnect_reconnects_and_replays_without_duplicates() {
        let mut asr = MockAsr::new(&["First sentence here.", "Second one follows."]);
        asr.disconnect_after_s = Some(3.0);
        let h = harness(asr, 50);
        let model = dictate(&h, prefs(&[("comp.enabled", json!(false))]), 10).await;
        // The connection drops mid-way through the second sentence. Replay resumes after the
        // last final result, so the first sentence isn't repeated and the second isn't lost.
        assert_eq!(
            model.text(),
            "Shopping: First sentence here. Second one follows."
        );
        h.storage.flush(Duration::from_secs(5));
        let ops = h.storage.operations("rec").unwrap();
        let conns: Vec<_> = ops
            .iter()
            .filter(|o| o["kind"] == "asr_connection")
            .collect();
        assert_eq!(conns.len(), 2, "{ops:?}");
        assert_eq!(conns[0]["status"], "failed");
        assert_eq!(conns[1]["attempt"], 2);
    }

    // Real time: the WAV writer is a real thread, which paused tokio time would outrun.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn retained_audio_is_written_and_interrupted_files_are_repaired() {
        let h = harness(MockAsr::new(&["Keep this audio."]), 10);
        let dir = tempfile::tempdir().unwrap();
        let audio_dir = dir.path().to_string_lossy().to_string();
        let p = prefs(&[
            ("audio.retain", json!(true)),
            ("audio.directory", json!(audio_dir)),
        ]);
        dictate(&h, p, 3).await;
        h.storage.flush(Duration::from_secs(5));
        let rec = h.storage.recording("rec").unwrap().unwrap();
        assert_eq!(rec["audio_status"], "complete", "{rec}");
        let path = rec["audio_path"].as_str().unwrap().to_owned();
        let samples = hound::WavReader::open(&path).unwrap().len() as i64;
        assert_eq!(rec["samples"], samples);
        assert!(samples >= 3 * 16_000, "{samples}");

        // Simulate an interruption: the database still says "recording" for the file.
        h.storage.update_audio("rec", "recording", None);
        h.storage.flush(Duration::from_secs(5));
        assert_eq!(reconcile_audio(&h.storage), 1);
        h.storage.flush(Duration::from_secs(5));
        assert_eq!(
            h.storage.recording("rec").unwrap().unwrap()["audio_status"],
            "partial"
        );
        std::fs::remove_file(&path).unwrap();
        h.storage.update_audio("rec", "recording", None);
        h.storage.flush(Duration::from_secs(5));
        reconcile_audio(&h.storage);
        h.storage.flush(Duration::from_secs(5));
        assert_eq!(
            h.storage.recording("rec").unwrap().unwrap()["audio_status"],
            "missing"
        );
    }

    fn op_outcomes(h: &Harness) -> Vec<(String, String)> {
        h.storage.flush(Duration::from_secs(5));
        h.storage
            .operations("rec")
            .unwrap()
            .iter()
            .filter(|o| o["kind"] == "composition")
            .map(|o| {
                (
                    o["status"].as_str().unwrap_or("").to_owned(),
                    o["outcome"].as_str().unwrap_or("").to_owned(),
                )
            })
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn speech_during_a_request_stays_as_a_suffix() {
        let (c, seen) = scripted(vec![Decision::Replace("Buy six apples.".into())], 1500);
        let h = harness_with(
            MockAsr::new(&["Buy seven, no actually six apples.", "And pears."]),
            c,
        );
        let model = dictate(&h, prefs(&[]), 8).await;
        assert_eq!(model.text(), "Shopping: Buy six apples. And pears.");
        let seen = seen.lock().unwrap().clone();
        assert_eq!(
            seen[0], "Buy seven, no actually six apples.",
            "first request saw only the first segment"
        );
        assert!(
            seen.iter().any(|s| s == "Buy six apples. And pears."),
            "{seen:?}"
        );
        assert_eq!(op_outcomes(&h)[0].0, "applied");
    }

    #[tokio::test(start_paused = true)]
    async fn defer_waits_for_more_speech() {
        let (c, seen) = scripted(
            vec![
                Decision::Defer,
                Decision::Replace("Get five apples.".into()),
            ],
            100,
        );
        let h = harness_with(
            MockAsr::new(&["Get six apples, actually no, get", "five apples."]),
            c,
        );
        let model = dictate(&h, prefs(&[]), 8).await;
        assert_eq!(model.text(), "Shopping: Get five apples.");
        assert_eq!(
            seen.lock().unwrap().len(),
            2,
            "re-requested only when new speech arrived"
        );
        let outcomes = op_outcomes(&h);
        assert!(outcomes[0].1.starts_with("Deferred"), "{outcomes:?}");
        assert_eq!(outcomes[1].0, "applied");
    }

    #[tokio::test(start_paused = true)]
    async fn invalid_replacements_are_rejected_and_text_stays() {
        let (c, _) = scripted(vec![Decision::Replace("   ".into())], 50);
        let h = harness_with(MockAsr::new(&["Some words stay here."]), c);
        let model = dictate(&h, prefs(&[]), 5).await;
        assert_eq!(model.text(), "Shopping: Some words stay here.");
        assert_eq!(op_outcomes(&h)[0].0, "rejected");
    }

    #[tokio::test(start_paused = true)]
    async fn unchanged_text_is_not_requested_again() {
        let (c, seen) = scripted(vec![], 50);
        let h = harness_with(MockAsr::new(&["One thing.", "Two things."]), c);
        dictate(&h, prefs(&[]), 8).await;
        // One request per new final segment; boundaries and Stop add none.
        assert_eq!(seen.lock().unwrap().len(), 2);
        h.storage.flush(Duration::from_secs(5));
        let rec = h.storage.recording("rec").unwrap().unwrap();
        let metrics: serde_json::Value =
            serde_json::from_str(rec["metrics"].as_str().unwrap()).unwrap();
        assert!(metrics["firstTextLagMs"].as_f64().is_some(), "{metrics}");
        assert_eq!(metrics["edits"]["completed"], 2);
    }

    #[tokio::test(start_paused = true)]
    async fn long_dictation_protects_older_sentences_while_recording() {
        let script = [
            "First one.",
            "Second one.",
            "Third one.",
            "Fourth one.",
            "Fifth one.",
        ];
        let (c, seen) = scripted(vec![], 50);
        let h = harness_with(MockAsr::new(&script), c);
        let model = dictate(&h, prefs(&[]), 12).await;
        assert_eq!(
            model.text(),
            "Shopping: First one. Second one. Third one. Fourth one. Fifth one."
        );
        let released_while_recording = h.events.lock().unwrap().iter().any(|e| {
            matches!(e, EngineEvent::Region { update, .. } if update.release > 0 && update.edit.is_none())
        });
        assert!(released_while_recording);
        // The model never saw more than the revisable sentences.
        assert!(
            seen.lock()
                .unwrap()
                .iter()
                .all(|t| crate::text::sentence_count(t) <= 3),
            "{seen:?}"
        );
        // Released text stays linked to its recording.
        assert_eq!(model.recordings_in(10, 15), vec!["rec".to_owned()]);
    }

    /// Regression (first live run): Deepgram finals hold several sentences. Text used to leave
    /// the region before the model saw it, splitting a correction from what it corrects.
    #[tokio::test(start_paused = true)]
    async fn multi_sentence_finals_reach_the_model_whole() {
        let (c, seen) = scripted(
            vec![
                Decision::NoChange,
                Decision::Replace("I want seven apples.".into()),
            ],
            1300,
        );
        let h = harness_with(
            MockAsr::new(&[
                "Hello. This is a test. This is just a test.",
                "Want six apples. Actually, no. I don't want six apples. I want seven apples.",
            ]),
            c,
        );
        let model = dictate(&h, prefs(&[]), 10).await;
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen[0], "Hello. This is a test. This is just a test.");
        assert_eq!(
            seen[1], "Want six apples. Actually, no. I don't want six apples. I want seven apples.",
            "the correction and its target arrive together"
        );
        assert_eq!(
            model.text(),
            "Shopping: Hello. This is a test. This is just a test. I want seven apples."
        );
        h.storage.flush(Duration::from_secs(5));
        let rec = h.storage.recording("rec").unwrap().unwrap();
        let metrics: serde_json::Value =
            serde_json::from_str(rec["metrics"].as_str().unwrap()).unwrap();
        assert!(metrics["correctionDelayMs"]["count"] == 1, "{metrics}");
    }

    /// Regression (second live run): an older request finishing first cleared the pending
    /// correction, so no correction delay was recorded.
    #[tokio::test(start_paused = true)]
    async fn correction_delay_survives_an_older_request() {
        let (c, _) = scripted(
            vec![
                Decision::NoChange,
                Decision::Replace("First part here. Buy seven apples.".into()),
            ],
            3000,
        );
        let h = harness_with(
            MockAsr::new(&["First part here.", "Buy six, no actually seven apples."]),
            c,
        );
        let model = dictate(&h, prefs(&[]), 12).await;
        assert_eq!(model.text(), "Shopping: First part here. Buy seven apples.");
        h.storage.flush(Duration::from_secs(5));
        let rec = h.storage.recording("rec").unwrap().unwrap();
        let metrics: serde_json::Value =
            serde_json::from_str(rec["metrics"].as_str().unwrap()).unwrap();
        assert_eq!(metrics["correctionDelayMs"]["count"], 1, "{metrics}");
    }

    #[tokio::test(start_paused = true)]
    async fn corrections_supersede_an_in_flight_request() {
        let script = [
            "First part of this note here.",
            "Buy six, no actually seven apples.",
        ];
        let run = |supersede: bool| async move {
            let (c, seen) = scripted(
                vec![
                    Decision::NoChange,
                    Decision::Replace("First part of this note here. Buy seven apples.".into()),
                ],
                6000,
            );
            let h = harness_with(MockAsr::new(&script), c);
            let p = prefs(&[("comp.supersede_on_correction", json!(supersede))]);
            let p = Resolved({
                let mut m = p.0;
                m.insert("life.finalize_timeout_ms".into(), json!(20000));
                m
            });
            let model = dictate(&h, p, 15).await;
            assert_eq!(
                model.text(),
                "Shopping: First part of this note here. Buy seven apples."
            );
            h.storage.flush(Duration::from_secs(5));
            let rec = h.storage.recording("rec").unwrap().unwrap();
            let metrics: serde_json::Value =
                serde_json::from_str(rec["metrics"].as_str().unwrap()).unwrap();
            let statuses: Vec<String> = op_outcomes(&h).into_iter().map(|(s, _)| s).collect();
            let seen = seen.lock().unwrap().len();
            (
                metrics["correctionDelayMs"]["max"].as_f64().unwrap(),
                statuses,
                seen,
            )
        };
        let (fast, statuses, seen) = run(true).await;
        assert!(statuses.contains(&"superseded".to_owned()), "{statuses:?}");
        assert_eq!(seen, 2);
        let (slow, statuses, _) = run(false).await;
        assert!(!statuses.contains(&"superseded".to_owned()));
        assert!(
            fast + 1000.0 < slow,
            "superseding saves time: {fast} vs {slow}"
        );
    }

    // Real time: the mock HTTP server runs on real sockets.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn anthropic_provider_uses_its_key_model_and_effort() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, Request, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("x-api-key", "sk-ant-test"))
            .respond_with(|req: &Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
                let fixed =
                    body["model"] == "claude-haiku-5-5" && body["output_config"]["effort"] == "low";
                let answer = if fixed {
                    json!({"decision": "replace", "text": "Buy six apples.", "reason": "c"})
                } else {
                    json!({"decision": "no_change", "text": "", "reason": "wrong request"})
                };
                ResponseTemplate::new(200).set_body_json(json!({
                    "content": [{"type": "text", "text": answer.to_string()}],
                    "stop_reason": "end_turn",
                    "usage": {"input_tokens": 1, "output_tokens": 1}
                }))
            })
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("db"), Arc::new(|_| {})).unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let ev = events.clone();
        let keys = Arc::new(KeyRing::default());
        keys.set(
            Provider::Anthropic,
            crate::credentials::ApiKey::new("sk-ant-test").unwrap(),
            false,
        );
        let asr: Arc<dyn AsrProvider> =
            Arc::new(MockAsr::new(&["Buy seven, no actually six apples."]));
        let uri = server.uri();
        let engine = Engine::start(
            &tokio::runtime::Handle::current(),
            EngineContext {
                storage: storage.clone(),
                keys,
                sink: Arc::new(move |e| ev.lock().unwrap().push(e)),
                asr: Arc::new(move |_| Some(asr.clone())),
                composer: Arc::new(move |name| {
                    (name == "anthropic").then(|| {
                        Arc::new(crate::composition::anthropic::Anthropic::new(&uri))
                            as Arc<dyn Composer>
                    })
                }),
            },
        );
        let h = Harness {
            events,
            engine,
            storage,
            _dir: dir,
        };
        let model = dictate(&h, prefs(&[("comp.provider", json!("anthropic"))]), 4).await;
        assert_eq!(model.text(), "Shopping: Buy six apples.");
        assert_eq!(op_outcomes(&h)[0].0, "applied");
    }

    #[tokio::test(start_paused = true)]
    async fn five_minute_session_stays_complete_and_bounded() {
        let sentences: Vec<String> = (1..=80)
            .map(|i| format!("Sentence number {i} has a few more words."))
            .collect();
        let script: Vec<&str> = sentences.iter().map(String::as_str).collect();
        let (c, seen) = scripted(vec![], 400);
        let h = harness_with(MockAsr::new(&script), c);
        let model = dictate(&h, prefs(&[]), 300).await;
        assert_eq!(model.text(), format!("Shopping: {}", sentences.join(" ")));
        let seen = seen.lock().unwrap();
        assert!(seen.len() >= 70, "{} requests", seen.len());
        assert!(seen.iter().all(|t| crate::text::word_count(t) <= 40));
        h.storage.flush(Duration::from_secs(10));
        let (_, total) = h.storage.events("rec", None, 0, 1).unwrap();
        assert!(total > 500, "{total} history events");
        assert_eq!(
            h.storage.recording("rec").unwrap().unwrap()["status"],
            "completed"
        );
    }

    /// Review finding: replay used try_send into a bounded channel and dropped whatever didn't
    /// fit. With 20 ms batches and no final result for 15 s, the replay is ~750 batches.
    #[tokio::test(start_paused = true)]
    async fn reconnect_replays_every_sample_with_small_batches() {
        let words: Vec<String> = (0..60).map(|i| format!("word{i}")).collect();
        let utterance = words.join(" ") + ".";
        let mut asr = MockAsr::new(&[utterance.as_str()]);
        asr.disconnect_after_s = Some(15.0);
        let received = asr.received.clone();
        let h = harness(asr, 10);
        let p = prefs(&[
            ("audio.batch_ms", json!(20)),
            ("audio.recovery_buffer_s", json!(60)),
            ("comp.enabled", json!(false)),
        ]);
        dictate(&h, p, 25).await;
        h.storage.flush(Duration::from_secs(5));
        let rec = h.storage.recording("rec").unwrap().unwrap();
        let total = rec["samples"].as_u64().unwrap();
        let per_conn = received.lock().unwrap().clone();
        assert_eq!(per_conn.len(), 2, "{per_conn:?}");
        // Nothing was final before the drop, so the new connection replays from sample 0.
        assert_eq!(
            per_conn[1], total,
            "every sample reached the new connection: {per_conn:?}"
        );
        assert_eq!(rec["status"], "completed");
    }

    /// Review finding: an unclean close while finishing was reported as a clean completion.
    #[tokio::test(start_paused = true)]
    async fn recognition_failure_while_finishing_is_reported() {
        let mut asr = MockAsr::new(&["one two three four five six seven eight."]);
        asr.fail_on_finalize = true;
        let h = harness(asr, 10);
        dictate(&h, prefs(&[]), 2).await;
        h.storage.flush(Duration::from_secs(5));
        let rec = h.storage.recording("rec").unwrap().unwrap();
        assert_eq!(rec["status"], "completed_with_errors", "{rec}");
        assert!(
            rec["note"].as_str().unwrap_or("").contains("finishing"),
            "{rec}"
        );
        assert!(h.events.lock().unwrap().iter().any(|e| matches!(e,
            EngineEvent::Notice { level: "warning", message } if message.contains("last words"))));
    }

    #[tokio::test(start_paused = true)]
    async fn slow_edits_time_out_and_text_stays_readable() {
        let h = harness(
            MockAsr::new(&["Buy seven, no actually six apples."]),
            60_000,
        );
        let model = dictate(&h, prefs(&[("comp.timeout_ms", json!(1000))]), 6).await;
        assert_eq!(model.text(), "Shopping: Buy seven, no actually six apples.");
        h.storage.flush(Duration::from_secs(5));
        let ops = h.storage.operations("rec").unwrap();
        assert!(ops.iter().any(|o| o["status"] == "timeout"), "{ops:?}");
    }
}
