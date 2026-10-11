//! Headless live check with the real providers: records from a microphone, streams to Deepgram,
//! applies corrections with the OpenAI model, prints the text and measurements.
//! **This makes paid API calls.** Uses a throwaway database.
//!
//!   DEEPGRAM_API_KEY=... OPENAI_API_KEY=... \
//!   (or ANTHROPIC_API_KEY=... BATSCRIBE_COMP_PROVIDER=anthropic) \
//!     cargo run -p batscribe-core --example live_session -- [seconds] [device-id]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use batscribe_core::credentials::{ApiKey, KeyRing, Provider};
use batscribe_core::editor::EditorModel;
use batscribe_core::engine::{self, Command, Engine, EngineContext, EngineEvent, StartRequest};
use batscribe_core::settings::{Paths, Preferences, Resolved, schema};
use batscribe_core::storage::Storage;
use serde_json::json;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let seconds: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(20);
    let device = args.get(2).cloned().unwrap_or_default();
    let keys = Arc::new(KeyRing::default());
    for (p, var) in [
        (Provider::Deepgram, "DEEPGRAM_API_KEY"),
        (Provider::OpenAi, "OPENAI_API_KEY"),
        (Provider::Anthropic, "ANTHROPIC_API_KEY"),
    ] {
        match std::env::var(var).ok().as_deref().and_then(ApiKey::new) {
            Some(k) => keys.set(p, k, false),
            None => eprintln!("{var} is not set"),
        }
    }
    let dir = std::env::temp_dir().join(format!("batscribe-live-{}", std::process::id()));
    let storage = Storage::open(&dir.join("db.sqlite3"), Arc::new(|_| {})).expect("database");
    let events = Arc::new(Mutex::new(Vec::new()));
    let ev = events.clone();
    let engine = Engine::start(
        &tokio::runtime::Handle::current(),
        EngineContext {
            storage: storage.clone(),
            keys,
            sink: Arc::new(move |e: EngineEvent| {
                match &e {
                    EngineEvent::Notice { level, message } => eprintln!("[{level}] {message}"),
                    EngineEvent::Status(s) if !s.is_empty() => eprintln!("[status] {s}"),
                    _ => {}
                }
                ev.lock().unwrap().push(e);
            }),
            asr: engine::default_asr_factory(),
            composer: engine::default_composer_factory(),
        },
    );
    let paths = Paths {
        data_dir: dir.clone(),
    };
    let mut prefs = Preferences::default();
    prefs.stored.insert("audio.device".into(), json!(device));
    // BATSCRIBE_COMP_PROVIDER=anthropic (or openai) picks the editing provider;
    // BATSCRIBE_COMP_MODEL overrides that provider's model.
    let provider = std::env::var("BATSCRIBE_COMP_PROVIDER").unwrap_or_else(|_| "openai".into());
    prefs.stored.insert("comp.provider".into(), json!(provider));
    // BATSCRIBE_ANTHROPIC_THINKING=off turns the Anthropic model's thinking off.
    if std::env::var("BATSCRIBE_ANTHROPIC_THINKING").is_ok_and(|v| v == "off") {
        prefs
            .stored
            .insert("comp.anthropic_thinking".into(), json!(false));
    }
    if let Ok(model) = std::env::var("BATSCRIBE_COMP_MODEL") {
        let key = if provider == "anthropic" {
            "comp.anthropic_model"
        } else {
            "comp.model"
        };
        prefs.stored.insert(key.into(), json!(model));
    }
    let prefs = Resolved(prefs.resolved(&schema(&paths, &[(device.clone(), device.clone())])));
    let mut model = EditorModel::default();
    model.open_region("live", 0, 0);
    engine.send(Command::Start(StartRequest {
        recording_id: "live".into(),
        document_id: "doc".into(),
        lead: false,
        context: String::new(),
        initial_position: 0,
        prefs,
    }));
    eprintln!("Speak now for {seconds} s (try a spoken correction: \"seven, no actually six\")…");
    tokio::time::sleep(Duration::from_secs(seconds)).await;
    engine.send(Command::Stop);
    loop {
        tokio::time::sleep(Duration::from_millis(200)).await;
        if events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, EngineEvent::State { state: "idle", .. }))
        {
            break;
        }
    }
    for e in events.lock().unwrap().iter() {
        match e {
            EngineEvent::Region { update, .. } => {
                model.apply_region(update, 0).expect("update applies");
            }
            EngineEvent::RegionClosed { recording_id, .. } => {
                model.close_region(recording_id);
            }
            _ => {}
        }
    }
    storage.flush(Duration::from_secs(5));
    println!("\nText:\n{}\n", model.text());
    if let Ok(Some(rec)) = storage.recording("live") {
        println!("Recording: {} ({})", rec["status"], rec["device"]);
        if let Some(m) = rec["metrics"].as_str() {
            let m: serde_json::Value = serde_json::from_str(m).unwrap_or_default();
            println!(
                "Metrics: {}",
                serde_json::to_string_pretty(&m).unwrap_or_default()
            );
        }
    }
    println!("History database: {}", dir.join("db.sqlite3").display());
}
