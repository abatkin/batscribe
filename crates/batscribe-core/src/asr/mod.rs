//! Streaming speech recognition: the provider interface and normalized messages.
//!
//! A provider connection is a task that consumes [`AsrInput`] (audio in order, then control
//! messages) and produces [`AsrOutput`]: every raw inbound message (for history) with its
//! normalized form, outbound control messages, and the connection's outcome. Timestamps in
//! messages are seconds from the start of that connection's audio; the session maps them onto
//! the recording's sample timeline.

pub mod deepgram;
pub mod mock;

use serde_json::Value;
use tokio::sync::mpsc;

use crate::credentials::ApiKey;
use crate::settings::Resolved;

#[derive(Debug, Clone)]
pub struct AsrConfig {
    pub model: String,
    pub language: String,
    pub sample_rate: u32,
    pub endpointing_ms: i64,
    pub utterance_end_ms: i64,
    pub interim_results: bool,
    pub punctuate: bool,
    pub smart_format: bool,
    pub vocabulary: Vec<String>,
    pub extra_params: Vec<(String, String)>,
    /// Recording sample where this connection's audio starts (after a reconnect, the replay
    /// point). Informational: real providers time from zero; the mock uses it to model one
    /// continuous speaker.
    pub stream_start_sample: u64,
}

impl AsrConfig {
    pub fn from_prefs(p: &Resolved, sample_rate: u32) -> Self {
        Self {
            model: p.str("asr.model").to_owned(),
            language: p.str("asr.language").to_owned(),
            sample_rate,
            endpointing_ms: p.int("asr.endpointing_ms"),
            utterance_end_ms: p.int("asr.utterance_end_ms"),
            interim_results: p.bool("asr.interim_results"),
            punctuate: p.bool("asr.punctuate"),
            smart_format: p.bool("asr.smart_format"),
            vocabulary: p
                .str("asr.vocabulary")
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_owned)
                .collect(),
            extra_params: p
                .str("asr.extra_params")
                .split('&')
                .filter_map(|kv| {
                    let (k, v) = kv.split_once('=')?;
                    let k = k.trim();
                    (!k.is_empty()).then(|| (k.to_owned(), v.trim().to_owned()))
                })
                .collect(),
            stream_start_sample: 0,
        }
    }
}

#[derive(Debug)]
pub enum AsrInput {
    /// 16-bit mono samples at the configured rate, contiguous with the previous audio.
    Audio(Vec<i16>),
    /// Flush buffered audio into final results.
    Finalize,
    /// No more audio; close after the remaining results.
    Close,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AsrMessage {
    Transcript {
        text: String,
        start_s: f64,
        duration_s: f64,
        is_final: bool,
        speech_final: bool,
        from_finalize: bool,
        confidence: Option<f64>,
        words: Option<Value>,
    },
    UtteranceEnd {
        last_word_end_s: f64,
    },
    SpeechStarted {
        timestamp_s: f64,
    },
    Metadata {
        request_id: Option<String>,
    },
    Error {
        message: String,
    },
    Other,
}

#[derive(Debug)]
pub enum AsrOutput {
    Opened {
        request_id: Option<String>,
        /// The connection URL with credentials removed, for history.
        url: String,
    },
    Message {
        raw: String,
        parsed: AsrMessage,
    },
    /// A control message sent to the provider (audio is not echoed).
    Sent {
        kind: String,
        payload: String,
    },
    Closed {
        clean: bool,
        reason: String,
    },
}

pub trait AsrProvider: Send + Sync {
    fn name(&self) -> &'static str;

    /// Opens a streaming connection and runs it until the input closes or the connection
    /// ends. The task's last output is always `Closed`.
    fn connect(
        &self,
        config: AsrConfig,
        key: Option<ApiKey>,
        input: mpsc::Receiver<AsrInput>,
        output: mpsc::Sender<AsrOutput>,
    ) -> tokio::task::JoinHandle<()>;
}

/// Phrases that make a correction complete enough to request an edit right away. Matching
/// ignores case and punctuation ("Actually, no." matches "actually no").
pub fn contains_correction(text: &str) -> bool {
    let normalized: String = text
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '\'' {
                c
            } else {
                ' '
            }
        })
        .collect();
    let words = format!(
        " {} ",
        normalized.split_whitespace().collect::<Vec<_>>().join(" ")
    );
    [
        "no actually",
        "actually no",
        "scratch that",
        "i mean",
        "correction",
        "make that",
        "no wait",
        "wait no",
        "i meant",
    ]
    .iter()
    .any(|p| words.contains(&format!(" {p} ")))
}

#[cfg(test)]
mod tests {
    #[test]
    fn correction_phrases_ignore_punctuation() {
        assert!(super::contains_correction(
            "Want six apples. Actually, no. I want seven."
        ));
        assert!(super::contains_correction("seven, no — actually six"));
        assert!(!super::contains_correction("Actually, notice the time."));
        assert!(!super::contains_correction("Nothing to see."));
    }
}
