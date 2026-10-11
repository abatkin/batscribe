//! File locations and typed, validated preferences.
//!
//! Every tunable behavior is a [`PrefDef`] with a type, unit, help, default and validation. Values
//! are stored in SQLite; [`Preferences::resolved`] fills in defaults. A recording snapshots the
//! resolved values at Start, so changes to pipeline settings apply to the next recording.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;
use serde_json::{Value, json};

/// Version of the composition prompt; recorded in settings snapshots.
pub const PROMPT_VERSION: &str = "conservative-1";

#[derive(Debug, Clone)]
pub struct Paths {
    pub data_dir: PathBuf,
}

impl Paths {
    /// `$XDG_DATA_HOME/batscribe` (default `~/.local/share/batscribe`).
    pub fn from_env() -> Self {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .unwrap_or_else(|| PathBuf::from("."));
        Self {
            data_dir: base.join("batscribe"),
        }
    }

    pub fn database(&self) -> PathBuf {
        self.data_dir.join("batscribe.sqlite3")
    }

    pub fn default_audio_dir(&self) -> PathBuf {
        self.data_dir.join("audio")
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PrefKind {
    Bool,
    Int {
        min: i64,
        max: i64,
        unit: &'static str,
    },
    Choice {
        /// (value, label)
        options: Vec<(String, String)>,
    },
    Text {
        multiline: bool,
    },
    Path,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Applies {
    Immediately,
    NextRecording,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrefDef {
    pub key: &'static str,
    pub group: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub kind: PrefKind,
    pub default: Value,
    pub advanced: bool,
    pub applies: Applies,
}

fn int(min: i64, max: i64, unit: &'static str) -> PrefKind {
    PrefKind::Int { min, max, unit }
}

fn choice(options: &[(&str, &str)]) -> PrefKind {
    PrefKind::Choice {
        options: options
            .iter()
            .map(|(v, l)| ((*v).to_owned(), (*l).to_owned()))
            .collect(),
    }
}

/// Whether development-only providers (mock recognition/editing, synthetic audio) are offered.
pub fn dev_providers_enabled() -> bool {
    std::env::var("BATSCRIBE_DEV_PROVIDERS").is_ok_and(|v| v == "1")
}

/// The full preference schema. `audio_devices` are (id, label) choices for the device setting.
pub fn schema(paths: &Paths, audio_devices: &[(String, String)]) -> Vec<PrefDef> {
    use Applies::{Immediately, NextRecording};
    let dev = dev_providers_enabled();
    let mut devices = vec![(String::new(), "System default".to_owned())];
    devices.extend(audio_devices.iter().cloned());
    if dev {
        devices.push(("synthetic".into(), "Synthetic silence (development)".into()));
    }
    let mut asr = vec![("deepgram", "Deepgram (streaming)")];
    let mut comp = vec![
        ("openai", "OpenAI Responses API"),
        ("anthropic", "Anthropic Messages API"),
    ];
    if dev {
        asr.push(("mock", "Scripted mock (development)"));
        comp.push(("mock", "Rule-based mock (development)"));
    }
    let p = |key, group, label, help, kind, default, advanced, applies| PrefDef {
        key,
        group,
        label,
        help,
        kind,
        default,
        advanced,
        applies,
    };
    vec![
        // Audio
        p(
            "audio.device",
            "Audio",
            "Microphone",
            "Input device used for dictation.",
            PrefKind::Choice { options: devices },
            json!(""),
            false,
            NextRecording,
        ),
        p(
            "audio.batch_ms",
            "Audio",
            "Batch duration",
            "Audio sent to recognition per message.",
            int(20, 250, "ms"),
            json!(80),
            true,
            NextRecording,
        ),
        p(
            "audio.recovery_buffer_s",
            "Audio",
            "Recovery buffer",
            "Recent audio kept in memory to resend after a recognition disconnect. Older audio becomes a recorded gap.",
            int(5, 300, "s"),
            json!(30),
            true,
            NextRecording,
        ),
        p(
            "audio.retain",
            "Audio",
            "Keep recordings",
            "Save each recording's audio (16 kHz mono WAV) in the audio folder. History is kept either way.",
            PrefKind::Bool,
            json!(false),
            false,
            NextRecording,
        ),
        p(
            "audio.directory",
            "Audio",
            "Audio folder",
            "Where retained recordings are written. Changes apply to later recordings.",
            PrefKind::Path,
            json!(paths.default_audio_dir().to_string_lossy()),
            false,
            NextRecording,
        ),
        // Recognition
        p(
            "asr.provider",
            "Recognition",
            "Provider",
            "Streaming speech recognition service.",
            choice(&asr),
            json!("deepgram"),
            false,
            NextRecording,
        ),
        p(
            "asr.model",
            "Recognition",
            "Model",
            "Provider model ID.",
            PrefKind::Text { multiline: false },
            json!("nova-3"),
            true,
            NextRecording,
        ),
        p(
            "asr.language",
            "Recognition",
            "Language",
            "Recognition language code.",
            PrefKind::Text { multiline: false },
            json!("en"),
            true,
            NextRecording,
        ),
        p(
            "asr.endpointing_ms",
            "Recognition",
            "Endpointing",
            "Silence after which the provider finalizes speech and marks a boundary.",
            int(10, 5000, "ms"),
            json!(400),
            false,
            NextRecording,
        ),
        p(
            "asr.utterance_end_ms",
            "Recognition",
            "Utterance end",
            "Gap in words that ends an utterance even with background noise (0 disables).",
            int(0, 5000, "ms"),
            json!(1000),
            true,
            NextRecording,
        ),
        p(
            "asr.interim_results",
            "Recognition",
            "Interim results",
            "Show provisional text while speaking.",
            PrefKind::Bool,
            json!(true),
            true,
            NextRecording,
        ),
        p(
            "asr.punctuate",
            "Recognition",
            "Punctuation",
            "Ask the provider to punctuate and capitalize.",
            PrefKind::Bool,
            json!(true),
            true,
            NextRecording,
        ),
        p(
            "asr.smart_format",
            "Recognition",
            "Smart formatting",
            "Provider formatting of numbers, dates and similar. Off leaves formatting to composition.",
            PrefKind::Bool,
            json!(false),
            true,
            NextRecording,
        ),
        p(
            "asr.vocabulary",
            "Recognition",
            "Vocabulary",
            "Names and terms to favor, one per line.",
            PrefKind::Text { multiline: true },
            json!(""),
            false,
            NextRecording,
        ),
        p(
            "asr.extra_params",
            "Recognition",
            "Extra provider options",
            "Additional query parameters for the provider, as key=value pairs separated by &.",
            PrefKind::Text { multiline: false },
            json!(""),
            true,
            NextRecording,
        ),
        // Composition
        p(
            "comp.enabled",
            "Composition",
            "Apply spoken corrections",
            "Revise recent dictated text when you correct yourself. Off shows recognition as is.",
            PrefKind::Bool,
            json!(true),
            false,
            NextRecording,
        ),
        p(
            "comp.provider",
            "Composition",
            "Provider",
            "Text editing service.",
            choice(&comp),
            json!("openai"),
            false,
            NextRecording,
        ),
        p(
            "comp.model",
            "Composition",
            "OpenAI model",
            "Model ID used with the OpenAI provider.",
            PrefKind::Text { multiline: false },
            json!("gpt-6-luna"),
            true,
            NextRecording,
        ),
        p(
            "comp.reasoning",
            "Composition",
            "OpenAI reasoning effort",
            "Higher may improve quality and adds latency.",
            choice(&[
                ("none", "None"),
                ("minimal", "Minimal"),
                ("low", "Low"),
                ("medium", "Medium"),
            ]),
            json!("none"),
            true,
            NextRecording,
        ),
        p(
            "comp.anthropic_model",
            "Composition",
            "Anthropic model",
            "Model ID used with the Anthropic provider.",
            PrefKind::Text { multiline: false },
            json!("claude-haiku-5-5"),
            true,
            NextRecording,
        ),
        p(
            "comp.anthropic_effort",
            "Composition",
            "Anthropic effort",
            "How much the model thinks before answering. Higher may improve quality and adds latency.",
            choice(&[("low", "Low"), ("medium", "Medium"), ("high", "High")]),
            json!("low"),
            true,
            NextRecording,
        ),
        p(
            "comp.anthropic_thinking",
            "Composition",
            "Anthropic thinking",
            "Let the model think before answering. Off is faster on corrections. Some models \
             (Claude Opus 5.5, Claude Sonnet 5.5) don't allow turning it off; with those, edits \
             fail and the text stays as spoken.",
            PrefKind::Bool,
            json!(true),
            true,
            NextRecording,
        ),
        p(
            "comp.cadence_ms",
            "Composition",
            "Cadence",
            "During continuous speech, request an edit about this often when there's new final text.",
            int(500, 10000, "ms"),
            json!(1500),
            true,
            NextRecording,
        ),
        p(
            "comp.debounce_ms",
            "Composition",
            "Debounce",
            "Wait after new final text before requesting an edit.",
            int(0, 5000, "ms"),
            json!(150),
            true,
            NextRecording,
        ),
        p(
            "comp.context_paragraphs",
            "Composition",
            "Context paragraphs",
            "Preceding paragraphs sent as read-only context.",
            int(0, 5, ""),
            json!(2),
            true,
            NextRecording,
        ),
        p(
            "comp.context_max_chars",
            "Composition",
            "Context limit",
            "Maximum read-only context.",
            int(0, 20000, "characters"),
            json!(2000),
            true,
            NextRecording,
        ),
        p(
            "comp.max_output_tokens",
            "Composition",
            "Output limit",
            "Maximum tokens in a model response.",
            int(64, 8000, "tokens"),
            json!(800),
            true,
            NextRecording,
        ),
        p(
            "comp.region_sentences",
            "Composition",
            "Revisable sentences",
            "Complete sentences kept revisable before the oldest is protected.",
            int(1, 10, ""),
            json!(2),
            false,
            NextRecording,
        ),
        p(
            "comp.region_words",
            "Composition",
            "Revisable words",
            "Word limit of the revisable region.",
            int(10, 1000, "words"),
            json!(150),
            true,
            NextRecording,
        ),
        p(
            "comp.region_seconds",
            "Composition",
            "Revisable time",
            "Speech duration limit of the revisable region.",
            int(5, 300, "s"),
            json!(30),
            true,
            NextRecording,
        ),
        p(
            "comp.defer_limit",
            "Composition",
            "Defer attempts",
            "How many times the model may ask to wait for more speech before text stays as spoken.",
            int(0, 10, ""),
            json!(3),
            true,
            NextRecording,
        ),
        p(
            "comp.supersede_on_correction",
            "Composition",
            "Prioritize corrections",
            "When a spoken correction arrives while an edit is in progress, send a new request \
             right away instead of waiting. The older request's result is recorded but not used, \
             so this can cost an extra request.",
            PrefKind::Bool,
            json!(true),
            true,
            NextRecording,
        ),
        p(
            "comp.timeout_ms",
            "Composition",
            "Request timeout",
            "A slower edit is abandoned; the recognized text stays.",
            int(1000, 60000, "ms"),
            json!(8000),
            true,
            NextRecording,
        ),
        // Lifecycle
        p(
            "life.finalize_timeout_ms",
            "Lifecycle",
            "Finishing timeout",
            "After Stop, how long to wait for final recognition and the last edit.",
            int(1000, 60000, "ms"),
            json!(8000),
            false,
            NextRecording,
        ),
        p(
            "life.reconnect_attempts",
            "Lifecycle",
            "Reconnect attempts",
            "Recognition reconnects before giving up.",
            int(0, 20, ""),
            json!(5),
            true,
            NextRecording,
        ),
        p(
            "life.reconnect_backoff_ms",
            "Lifecycle",
            "Reconnect backoff",
            "Initial delay between reconnects (doubles).",
            int(100, 30000, "ms"),
            json!(500),
            true,
            NextRecording,
        ),
        // Editor
        p(
            "editor.start_placement",
            "Editor",
            "Start position",
            "Where dictation begins when you press Start.",
            choice(&[
                ("line_end", "End of the current line"),
                ("caret", "At the caret"),
            ]),
            json!("line_end"),
            false,
            Immediately,
        ),
        p(
            "editor.undo_group_ms",
            "Editor",
            "Undo grouping",
            "Typing within this interval undoes as one step.",
            int(0, 10000, "ms"),
            json!(1000),
            true,
            Immediately,
        ),
        p(
            "editor.report_debounce_ms",
            "Editor",
            "Save delay",
            "Idle time after typing before the document is saved.",
            int(100, 10000, "ms"),
            json!(1000),
            true,
            Immediately,
        ),
        p(
            "editor.font_size",
            "Editor",
            "Font size",
            "Editor text size.",
            int(8, 32, "pt"),
            json!(12),
            false,
            Immediately,
        ),
        // Storage and inspector
        p(
            "storage.history_batch_ms",
            "Storage",
            "History batching",
            "History events are written in batches this often.",
            int(10, 5000, "ms"),
            json!(250),
            true,
            Immediately,
        ),
        p(
            "storage.history_queue",
            "Storage",
            "History queue limit",
            "Unwritten history events kept in memory; beyond this, events are dropped and a gap is recorded.",
            int(100, 1_000_000, "events"),
            json!(20000),
            true,
            Immediately,
        ),
        p(
            "inspector.live_follow",
            "Storage",
            "Inspector follows new events",
            "Show new history as it arrives until you select an older entry.",
            PrefKind::Bool,
            json!(true),
            false,
            Immediately,
        ),
        p(
            "inspector.page_size",
            "Storage",
            "Inspector page size",
            "History rows loaded at a time.",
            int(20, 1000, "rows"),
            json!(100),
            true,
            Immediately,
        ),
    ]
}

/// Validates `value` against `def`, normalizing it (e.g. trimming text).
pub fn validate(def: &PrefDef, value: &Value) -> Result<Value, String> {
    match &def.kind {
        PrefKind::Bool => value
            .as_bool()
            .map(Value::Bool)
            .ok_or_else(|| format!("{} must be on or off", def.label)),
        PrefKind::Int { min, max, unit } => {
            let n = value
                .as_i64()
                .or_else(|| value.as_f64().map(|f| f.round() as i64))
                .or_else(|| value.as_str().and_then(|s| s.trim().parse().ok()))
                .ok_or_else(|| format!("{} must be a whole number", def.label))?;
            if n < *min || n > *max {
                return Err(format!(
                    "{} must be between {min} and {max}{}",
                    def.label,
                    if unit.is_empty() {
                        String::new()
                    } else {
                        format!(" {unit}")
                    }
                ));
            }
            Ok(json!(n))
        }
        PrefKind::Choice { options } => {
            let s = value.as_str().unwrap_or_default();
            // Devices come and go: keep the choice, and let Start report it as unavailable
            // rather than silently recording from another microphone.
            if options.iter().any(|(v, _)| v == s)
                || (def.key == "audio.device" && value.is_string())
            {
                Ok(json!(s))
            } else {
                Err(format!("{s:?} is not available for {}", def.label))
            }
        }
        PrefKind::Text { multiline } => {
            let s = value
                .as_str()
                .ok_or_else(|| format!("{} must be text", def.label))?;
            Ok(json!(if *multiline { s.trim_end() } else { s.trim() }))
        }
        PrefKind::Path => {
            let s = value.as_str().unwrap_or_default().trim();
            if s.is_empty() || !std::path::Path::new(s).is_absolute() {
                return Err(format!("{} must be an absolute path", def.label));
            }
            Ok(json!(s))
        }
    }
}

/// Stored values (possibly partial) on top of the schema defaults.
#[derive(Debug, Clone, Default)]
pub struct Preferences {
    pub stored: BTreeMap<String, Value>,
}

impl Preferences {
    /// All values, with defaults for anything unset or no longer valid.
    pub fn resolved(&self, schema: &[PrefDef]) -> BTreeMap<String, Value> {
        schema
            .iter()
            .map(|d| {
                let v = self
                    .stored
                    .get(d.key)
                    .and_then(|v| validate(d, v).ok())
                    .unwrap_or_else(|| d.default.clone());
                (d.key.to_owned(), v)
            })
            .collect()
    }
}

/// Typed accessors over resolved values.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Resolved(pub BTreeMap<String, Value>);

impl Resolved {
    pub fn int(&self, key: &str) -> i64 {
        self.0.get(key).and_then(Value::as_i64).unwrap_or_default()
    }

    pub fn ms(&self, key: &str) -> std::time::Duration {
        std::time::Duration::from_millis(self.int(key).max(0) as u64)
    }

    pub fn bool(&self, key: &str) -> bool {
        self.0.get(key).and_then(Value::as_bool).unwrap_or_default()
    }

    pub fn str(&self, key: &str) -> &str {
        self.0.get(key).and_then(Value::as_str).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_and_defaults() {
        let paths = Paths {
            data_dir: "/tmp/x".into(),
        };
        let schema = schema(&paths, &[]);
        let def = schema
            .iter()
            .find(|d| d.key == "asr.endpointing_ms")
            .unwrap();
        assert_eq!(validate(def, &json!("300")), Ok(json!(300)));
        assert!(validate(def, &json!(5)).is_err());
        let placement = schema
            .iter()
            .find(|d| d.key == "editor.start_placement")
            .unwrap();
        assert!(validate(placement, &json!("nowhere")).is_err());
        let mut prefs = Preferences::default();
        prefs
            .stored
            .insert("asr.endpointing_ms".into(), json!(9999999));
        prefs.stored.insert("audio.retain".into(), json!(true));
        let r = prefs.resolved(&schema);
        assert_eq!(
            r["asr.endpointing_ms"],
            json!(400),
            "invalid stored value falls back"
        );
        assert_eq!(r["audio.retain"], json!(true));
        let keys: std::collections::HashSet<_> = schema.iter().map(|d| d.key).collect();
        assert_eq!(keys.len(), schema.len(), "keys are unique");
    }
}
