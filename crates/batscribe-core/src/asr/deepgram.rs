//! Deepgram streaming recognition over WebSocket (`/v1/listen`).

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

use super::{AsrConfig, AsrInput, AsrMessage, AsrOutput, AsrProvider};
use crate::credentials::ApiKey;

pub const DEFAULT_URL: &str = "wss://api.deepgram.com/v1/listen";

pub struct Deepgram {
    pub url: String,
}

impl Default for Deepgram {
    fn default() -> Self {
        Self {
            url: std::env::var("BATSCRIBE_DEEPGRAM_URL").unwrap_or_else(|_| DEFAULT_URL.to_owned()),
        }
    }
}

fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The listen URL with all query parameters (no credentials are part of it).
pub fn listen_url(base: &str, c: &AsrConfig) -> String {
    let mut q: Vec<(String, String)> = vec![
        ("model".into(), c.model.clone()),
        ("language".into(), c.language.clone()),
        ("encoding".into(), "linear16".into()),
        ("sample_rate".into(), c.sample_rate.to_string()),
        ("channels".into(), "1".into()),
        ("interim_results".into(), c.interim_results.to_string()),
        ("punctuate".into(), c.punctuate.to_string()),
        ("smart_format".into(), c.smart_format.to_string()),
        ("endpointing".into(), c.endpointing_ms.to_string()),
        ("vad_events".into(), "true".into()),
    ];
    if c.utterance_end_ms > 0 && c.interim_results {
        q.push((
            "utterance_end_ms".into(),
            c.utterance_end_ms.max(1000).to_string(),
        ));
    }
    for term in &c.vocabulary {
        q.push(("keyterm".into(), term.clone()));
    }
    q.extend(c.extra_params.iter().cloned());
    let query: Vec<String> = q
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect();
    format!("{base}?{}", query.join("&"))
}

/// Normalizes one Deepgram JSON message.
pub fn parse_message(raw: &str) -> AsrMessage {
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return AsrMessage::Other;
    };
    let f = |v: &Value, k: &str| v.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    let b = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
    match v.get("type").and_then(Value::as_str) {
        Some("Results") => {
            let alt = v.pointer("/channel/alternatives/0");
            AsrMessage::Transcript {
                text: alt
                    .and_then(|a| a.get("transcript"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                start_s: f(&v, "start"),
                duration_s: f(&v, "duration"),
                is_final: b("is_final"),
                speech_final: b("speech_final"),
                from_finalize: b("from_finalize"),
                confidence: alt
                    .and_then(|a| a.get("confidence"))
                    .and_then(Value::as_f64),
                words: alt.and_then(|a| a.get("words")).cloned(),
            }
        }
        Some("UtteranceEnd") => AsrMessage::UtteranceEnd {
            last_word_end_s: f(&v, "last_word_end"),
        },
        Some("SpeechStarted") => AsrMessage::SpeechStarted {
            timestamp_s: f(&v, "timestamp"),
        },
        Some("Metadata") => AsrMessage::Metadata {
            request_id: v
                .get("request_id")
                .and_then(Value::as_str)
                .map(str::to_owned),
        },
        Some("Error") => AsrMessage::Error {
            message: v
                .get("description")
                .or_else(|| v.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
                .to_owned(),
        },
        _ if v.get("err_msg").is_some() => AsrMessage::Error {
            message: v["err_msg"].as_str().unwrap_or("unknown error").to_owned(),
        },
        _ => AsrMessage::Other,
    }
}

impl AsrProvider for Deepgram {
    fn name(&self) -> &'static str {
        "deepgram"
    }

    fn connect(
        &self,
        config: AsrConfig,
        key: Option<ApiKey>,
        mut input: mpsc::Receiver<AsrInput>,
        output: mpsc::Sender<AsrOutput>,
    ) -> tokio::task::JoinHandle<()> {
        let url = listen_url(&self.url, &config);
        tokio::spawn(async move {
            let closed = |clean: bool, reason: String| AsrOutput::Closed { clean, reason };
            let Some(key) = key else {
                let _ = output
                    .send(closed(false, "No Deepgram API key".into()))
                    .await;
                return;
            };
            let mut request = match url.as_str().into_client_request() {
                Ok(r) => r,
                Err(e) => {
                    let _ = output.send(closed(false, e.to_string())).await;
                    return;
                }
            };
            match HeaderValue::from_str(&format!("Token {}", key.expose())) {
                Ok(h) => {
                    request.headers_mut().insert("Authorization", h);
                }
                Err(_) => {
                    let _ = output.send(closed(false, "Invalid API key".into())).await;
                    return;
                }
            }
            let (ws, response) = match tokio_tungstenite::connect_async(request).await {
                Ok(x) => x,
                Err(e) => {
                    let reason = match &e {
                        tokio_tungstenite::tungstenite::Error::Http(r) => {
                            format!("Deepgram refused the connection (HTTP {})", r.status())
                        }
                        other => format!("Couldn't connect to Deepgram: {other}"),
                    };
                    let _ = output.send(closed(false, reason)).await;
                    return;
                }
            };
            let request_id = response
                .headers()
                .get("dg-request-id")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let _ = output
                .send(AsrOutput::Opened {
                    request_id,
                    url: url.clone(),
                })
                .await;
            let (mut write, mut read) = ws.split();
            let mut input_open = true;
            loop {
                tokio::select! {
                    msg = input.recv(), if input_open => {
                        let result = match msg {
                            Some(AsrInput::Audio(samples)) => {
                                let mut bytes = Vec::with_capacity(samples.len() * 2);
                                for s in samples {
                                    bytes.extend_from_slice(&s.to_le_bytes());
                                }
                                write.send(Message::binary(bytes)).await
                            }
                            Some(AsrInput::Finalize) => {
                                let payload = json!({"type": "Finalize"}).to_string();
                                let _ = output.send(AsrOutput::Sent { kind: "Finalize".into(), payload: payload.clone() }).await;
                                write.send(Message::text(payload)).await
                            }
                            Some(AsrInput::Close) | None => {
                                input_open = false;
                                let payload = json!({"type": "CloseStream"}).to_string();
                                let _ = output.send(AsrOutput::Sent { kind: "CloseStream".into(), payload: payload.clone() }).await;
                                write.send(Message::text(payload)).await
                            }
                        };
                        if let Err(e) = result {
                            let _ = output.send(closed(false, format!("Sending to Deepgram failed: {e}"))).await;
                            return;
                        }
                    }
                    msg = read.next() => match msg {
                        Some(Ok(Message::Text(text))) => {
                            let raw = text.to_string();
                            let parsed = parse_message(&raw);
                            if output.send(AsrOutput::Message { raw, parsed }).await.is_err() {
                                return;
                            }
                        }
                        Some(Ok(Message::Close(frame))) => {
                            let reason = frame.map(|f| format!("{} {}", f.code, f.reason)).unwrap_or_default();
                            let _ = output.send(closed(!input_open, reason)).await;
                            return;
                        }
                        Some(Ok(_)) => {}
                        Some(Err(e)) => {
                            let _ = output.send(closed(false, format!("Deepgram connection lost: {e}"))).await;
                            return;
                        }
                        None => {
                            let _ = output.send(closed(!input_open, "connection ended".into())).await;
                            return;
                        }
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_results_and_events() {
        let raw = r#"{"type":"Results","channel_index":[0,1],"duration":1.5,"start":2.25,
            "is_final":true,"speech_final":false,"from_finalize":false,
            "channel":{"alternatives":[{"transcript":"buy seven oranges","confidence":0.93,
            "words":[{"word":"buy","start":2.3,"end":2.5}]}]}}"#;
        match parse_message(raw) {
            AsrMessage::Transcript {
                text,
                start_s,
                is_final,
                speech_final,
                ..
            } => {
                assert_eq!(text, "buy seven oranges");
                assert_eq!(start_s, 2.25);
                assert!(is_final && !speech_final);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            parse_message(r#"{"type":"UtteranceEnd","channel":[0,1],"last_word_end":3.1}"#),
            AsrMessage::UtteranceEnd {
                last_word_end_s: 3.1
            }
        );
        assert_eq!(parse_message("not json"), AsrMessage::Other);
    }

    #[test]
    fn url_has_options_and_vocabulary() {
        let c = AsrConfig {
            model: "nova-3".into(),
            language: "en".into(),
            sample_rate: 16000,
            endpointing_ms: 400,
            utterance_end_ms: 1000,
            interim_results: true,
            punctuate: true,
            smart_format: false,
            vocabulary: vec!["Batscribe".into(), "Nova 3".into()],
            extra_params: vec![("diarize".into(), "false".into())],
            stream_start_sample: 0,
        };
        let url = listen_url(DEFAULT_URL, &c);
        assert!(url.contains("model=nova-3"));
        assert!(url.contains("endpointing=400"));
        assert!(url.contains("utterance_end_ms=1000"));
        assert!(url.contains("keyterm=Nova%203"));
        assert!(url.ends_with("diarize=false"));
    }

    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

    fn config() -> AsrConfig {
        AsrConfig {
            model: "nova-3".into(),
            language: "en".into(),
            sample_rate: 16000,
            endpointing_ms: 300,
            utterance_end_ms: 0,
            interim_results: true,
            punctuate: true,
            smart_format: false,
            vocabulary: vec![],
            extra_params: vec![],
            stream_start_sample: 0,
        }
    }

    fn results(text: &str, is_final: bool, from_finalize: bool) -> String {
        json!({"type": "Results", "start": 0.0, "duration": 0.2, "is_final": is_final,
               "speech_final": is_final, "from_finalize": from_finalize,
               "channel": {"alternatives": [{"transcript": text, "confidence": 0.9, "words": []}]}})
        .to_string()
    }

    async fn collect_until_closed(rx: &mut mpsc::Receiver<AsrOutput>) -> Vec<AsrOutput> {
        let mut out = Vec::new();
        while let Some(o) = rx.recv().await {
            let done = matches!(o, AsrOutput::Closed { .. });
            out.push(o);
            if done {
                break;
            }
        }
        out
    }

    // The handshake callback's error type is tungstenite's, not ours.
    #[allow(clippy::result_large_err)]
    #[tokio::test]
    async fn streams_audio_and_control_messages_over_websocket() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new((String::new(), String::new())));
        let seen2 = seen.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let ws = tokio_tungstenite::accept_hdr_async(
                stream,
                move |req: &Request, mut resp: Response| {
                    let auth = req
                        .headers()
                        .get("authorization")
                        .map(|v| v.to_str().unwrap().to_owned());
                    *seen2.lock().unwrap() = (
                        auth.unwrap_or_default(),
                        req.uri().query().unwrap_or("").to_owned(),
                    );
                    resp.headers_mut()
                        .insert("dg-request-id", "req-123".parse().unwrap());
                    Ok(resp)
                },
            )
            .await
            .unwrap();
            let (mut w, mut r) = ws.split();
            let (mut audio, mut controls, mut sent_interim) = (0usize, Vec::new(), false);
            while let Some(Ok(m)) = r.next().await {
                match m {
                    Message::Binary(b) => {
                        audio += b.len();
                        if audio >= 3200 && !sent_interim {
                            sent_interim = true;
                            w.send(Message::text(results("hello", false, false)))
                                .await
                                .unwrap();
                        }
                    }
                    Message::Text(t) => {
                        let t = t.to_string();
                        controls.push(t.clone());
                        if t.contains("Finalize") {
                            w.send(Message::text(results("hello world", true, true)))
                                .await
                                .unwrap();
                        } else if t.contains("CloseStream") {
                            w.send(Message::text(
                                json!({"type": "Metadata", "request_id": "req-123"}).to_string(),
                            ))
                            .await
                            .unwrap();
                            w.send(Message::Close(None)).await.unwrap();
                            break;
                        }
                    }
                    _ => {}
                }
            }
            (audio, controls)
        });

        let (in_tx, in_rx) = mpsc::channel(16);
        let (out_tx, mut out_rx) = mpsc::channel(64);
        Deepgram {
            url: format!("ws://{addr}/v1/listen"),
        }
        .connect(config(), ApiKey::new("dg-secret"), in_rx, out_tx);
        in_tx.send(AsrInput::Audio(vec![1; 1600])).await.unwrap();
        in_tx.send(AsrInput::Audio(vec![2; 1600])).await.unwrap();
        in_tx.send(AsrInput::Finalize).await.unwrap();
        in_tx.send(AsrInput::Close).await.unwrap();
        let out = collect_until_closed(&mut out_rx).await;

        let (audio, controls) = server.await.unwrap();
        assert_eq!(audio, 6400, "PCM16 little-endian frames");
        assert_eq!(controls.len(), 2);
        let (auth, query) = seen.lock().unwrap().clone();
        assert_eq!(auth, "Token dg-secret");
        assert!(query.contains("model=nova-3") && query.contains("encoding=linear16"));
        assert!(
            matches!(&out[0], AsrOutput::Opened { request_id: Some(id), .. } if id == "req-123")
        );
        let texts: Vec<_> = out
            .iter()
            .filter_map(|o| match o {
                AsrOutput::Message {
                    parsed: AsrMessage::Transcript { text, is_final, .. },
                    ..
                } => Some((text.clone(), *is_final)),
                _ => None,
            })
            .collect();
        assert_eq!(
            texts,
            vec![("hello".into(), false), ("hello world".into(), true)]
        );
        assert!(
            out.iter()
                .any(|o| matches!(o, AsrOutput::Sent { kind, .. } if kind == "Finalize"))
        );
        assert!(matches!(
            out.last(),
            Some(AsrOutput::Closed { clean: true, .. })
        ));
        for o in &out {
            if let AsrOutput::Opened { url, .. } = o {
                assert!(
                    !url.contains("dg-secret"),
                    "the key never appears in history"
                );
            }
        }
    }

    #[tokio::test]
    async fn dropped_connection_and_rejected_key_are_unclean_closes() {
        // The server accepts, then drops the TCP connection mid-stream.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            drop(ws);
        });
        let (in_tx, in_rx) = mpsc::channel(16);
        let (out_tx, mut out_rx) = mpsc::channel(64);
        Deepgram {
            url: format!("ws://{addr}/"),
        }
        .connect(config(), ApiKey::new("k"), in_rx, out_tx);
        let _ = in_tx.send(AsrInput::Audio(vec![0; 160])).await;
        let out = collect_until_closed(&mut out_rx).await;
        assert!(
            matches!(out.last(), Some(AsrOutput::Closed { clean: false, .. })),
            "{out:?}"
        );

        // A plain HTTP server answering 401 to the upgrade.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf).await;
            let _ = stream
                .write_all(b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\n\r\n")
                .await;
        });
        let (_in_tx, in_rx) = mpsc::channel(16);
        let (out_tx, mut out_rx) = mpsc::channel(64);
        Deepgram {
            url: format!("ws://{addr}/"),
        }
        .connect(config(), ApiKey::new("bad"), in_rx, out_tx);
        let out = collect_until_closed(&mut out_rx).await;
        match out.last() {
            Some(AsrOutput::Closed {
                clean: false,
                reason,
            }) => assert!(reason.contains("401"), "{reason}"),
            other => panic!("{other:?}"),
        }
    }
}
