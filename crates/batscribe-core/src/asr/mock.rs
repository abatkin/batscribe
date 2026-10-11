//! Scripted recognizer for development and tests. It "hears" a fixed script at a steady pace
//! measured in received audio, and emits Deepgram-format messages so history and parsing behave
//! exactly as with the real service.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::json;
use tokio::sync::mpsc;

use super::{AsrConfig, AsrInput, AsrOutput, AsrProvider, deepgram::parse_message};
use crate::credentials::ApiKey;

pub const DEFAULT_SCRIPT: &[&str] = &[
    "At the store, please buy seven oranges,",
    "no, actually six oranges.",
    "Then come home.",
];

pub struct MockAsr {
    pub script: Vec<String>,
    pub words_per_second: f64,
    /// Close the first connection uncleanly after this much audio (seconds).
    pub disconnect_after_s: Option<f64>,
    /// Close a connection uncleanly when it receives Finalize (a failure while finishing).
    pub fail_on_finalize: bool,
    /// Samples received by each connection, in connection order (for tests).
    pub received: Arc<std::sync::Mutex<Vec<u64>>>,
    disconnected: Arc<AtomicBool>,
}

impl Default for MockAsr {
    fn default() -> Self {
        let script = std::env::var("BATSCRIBE_MOCK_SCRIPT")
            .ok()
            .map(|s| s.split('|').map(str::to_owned).collect())
            .unwrap_or_else(|| DEFAULT_SCRIPT.iter().map(|s| (*s).to_owned()).collect());
        Self {
            script,
            words_per_second: 3.0,
            disconnect_after_s: std::env::var("BATSCRIBE_MOCK_DISCONNECT_AT")
                .ok()
                .and_then(|v| v.parse().ok()),
            fail_on_finalize: false,
            received: Arc::default(),
            disconnected: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl MockAsr {
    pub fn new(script: &[&str]) -> Self {
        Self {
            script: script.iter().map(|s| (*s).to_owned()).collect(),
            words_per_second: 3.0,
            disconnect_after_s: None,
            fail_on_finalize: false,
            received: Arc::default(),
            disconnected: Arc::new(AtomicBool::new(false)),
        }
    }
}

struct Word {
    text: String,
    utterance: usize,
    at: f64,
}

fn results(
    text: &str,
    start: f64,
    end: f64,
    is_final: bool,
    speech_final: bool,
    from_finalize: bool,
) -> String {
    json!({
        "type": "Results",
        "channel_index": [0, 1],
        "duration": (end - start).max(0.0),
        "start": start,
        "is_final": is_final,
        "speech_final": speech_final,
        "from_finalize": from_finalize,
        "channel": {"alternatives": [{"transcript": text, "confidence": 0.99, "words": []}]},
        "metadata": {"request_id": "mock"}
    })
    .to_string()
}

fn interim_form(word: &str) -> String {
    word.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
        .to_lowercase()
}

impl AsrProvider for MockAsr {
    fn name(&self) -> &'static str {
        "mock"
    }

    fn connect(
        &self,
        config: AsrConfig,
        _key: Option<ApiKey>,
        mut input: mpsc::Receiver<AsrInput>,
        output: mpsc::Sender<AsrOutput>,
    ) -> tokio::task::JoinHandle<()> {
        let rate = f64::from(config.sample_rate.max(1));
        let endpoint = config.endpointing_ms as f64 / 1000.0;
        let mut words = Vec::new();
        let mut t = 0.3;
        for (u, utterance) in self.script.iter().enumerate() {
            for w in utterance.split_whitespace() {
                t += 1.0 / self.words_per_second;
                words.push(Word {
                    text: w.to_owned(),
                    utterance: u,
                    at: t,
                });
            }
            t += 0.9;
        }
        let script = self.script.clone();
        let fail_on_finalize = self.fail_on_finalize;
        let received = self.received.clone();
        let index = {
            let mut r = received.lock().unwrap();
            r.push(0);
            r.len() - 1
        };
        let disconnect = self
            .disconnect_after_s
            .filter(|_| !self.disconnected.swap(true, Ordering::Relaxed));
        // This connection's audio starts here on the recording timeline.
        let offset = config.stream_start_sample as f64 / rate;
        let utterance_ends: Vec<f64> = (0..script.len())
            .map(|u| {
                words
                    .iter()
                    .filter(|w| w.utterance == u)
                    .map(|w| w.at)
                    .fold(f64::MIN, f64::max)
            })
            .collect();
        // Utterances finished before this connection's audio were finalized earlier.
        let mut finalized_utterances = utterance_ends
            .iter()
            .take_while(|&&end| end + 0.1 <= offset)
            .count();
        tokio::spawn(async move {
            let send = |raw: String| {
                let output = output.clone();
                async move {
                    let parsed = parse_message(&raw);
                    output
                        .send(AsrOutput::Message { raw, parsed })
                        .await
                        .is_ok()
                }
            };
            let _ = output
                .send(AsrOutput::Opened {
                    request_id: Some("mock".into()),
                    url: "mock://listen".into(),
                })
                .await;
            let mut samples = 0u64;
            let mut last_interim = String::new();
            let mut last_interim_at = f64::MIN;
            // Connection-relative time up to which final results were sent.
            let mut final_until = 0.0f64;
            let heard = |u: usize, now: f64| -> Vec<&Word> {
                words
                    .iter()
                    .filter(|w| w.utterance == u && w.at >= offset && w.at <= now)
                    .collect()
            };
            while let Some(msg) = input.recv().await {
                match msg {
                    AsrInput::Audio(chunk) => {
                        samples += chunk.len() as u64;
                        received.lock().unwrap()[index] = samples;
                        let local = samples as f64 / rate;
                        let now = offset + local;
                        if disconnect.is_some_and(|d| local >= d) {
                            let _ = output
                                .send(AsrOutput::Closed {
                                    clean: false,
                                    reason: "mock disconnect".into(),
                                })
                                .await;
                            return;
                        }
                        // Finalize every utterance whose last word is followed by the endpoint.
                        while finalized_utterances < script.len() {
                            let last = utterance_ends[finalized_utterances];
                            if now < last + endpoint {
                                break;
                            }
                            let end = last + 0.1 - offset;
                            let text: Vec<&str> = words
                                .iter()
                                .filter(|w| w.utterance == finalized_utterances && w.at >= offset)
                                .map(|w| w.text.as_str())
                                .collect();
                            let raw = results(&text.join(" "), final_until, end, true, true, false);
                            final_until = end;
                            finalized_utterances += 1;
                            last_interim.clear();
                            if !send(raw).await {
                                return;
                            }
                        }
                        let interim: Vec<String> = heard(finalized_utterances, now)
                            .iter()
                            .map(|w| interim_form(&w.text))
                            .collect();
                        let interim = interim.join(" ");
                        if config.interim_results
                            && !interim.is_empty()
                            && interim != last_interim
                            && local - last_interim_at >= 0.25
                        {
                            last_interim_at = local;
                            last_interim = interim.clone();
                            if !send(results(&interim, final_until, local, false, false, false))
                                .await
                            {
                                return;
                            }
                        }
                    }
                    AsrInput::Finalize if fail_on_finalize => {
                        let _ = output
                            .send(AsrOutput::Closed {
                                clean: false,
                                reason: "mock failure while finalizing".into(),
                            })
                            .await;
                        return;
                    }
                    AsrInput::Finalize => {
                        let local = samples as f64 / rate;
                        let pending: Vec<&str> = heard(finalized_utterances, offset + local)
                            .iter()
                            .map(|w| w.text.as_str())
                            .collect();
                        let text = pending.join(" ");
                        if !pending.is_empty() {
                            finalized_utterances += 1;
                        }
                        if !send(results(&text, final_until, local, true, true, true)).await {
                            return;
                        }
                        final_until = local;
                    }
                    AsrInput::Close => break,
                }
            }
            let _ = send(json!({"type": "Metadata", "request_id": "mock"}).to_string()).await;
            let _ = output
                .send(AsrOutput::Closed {
                    clean: true,
                    reason: "closed".into(),
                })
                .await;
        })
    }
}
