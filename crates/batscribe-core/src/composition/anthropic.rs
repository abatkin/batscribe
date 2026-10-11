//! Anthropic Messages API adapter (`POST /v1/messages`) with structured output.
//!
//! Raw HTTP: there is no official Rust SDK. By default thinking stays adaptive and `effort`
//! (default `low`) sets how much the model thinks; on short edits it often skips thinking
//! entirely. Turning thinking off (a preference) trades that for lower latency on corrections;
//! Claude Haiku 5.5 accepts it at effort `low` through `high`. Thinking counts toward
//! `max_tokens`, so the cap keeps room for it.

use std::time::Duration;

use serde_json::{Value, json};

use super::{
    Composer, EditFuture, EditOutcome, EditRequest, INSTRUCTIONS, parse_decision, response_schema,
    user_payload,
};
use crate::credentials::ApiKey;

pub const DEFAULT_BASE: &str = "https://api.anthropic.com";
pub const API_VERSION: &str = "2023-06-01";
/// Minimum `max_tokens`, leaving room for thinking before the short JSON answer.
const MIN_MAX_TOKENS: i64 = 4096;

pub struct Anthropic {
    pub base_url: String,
    client: reqwest::Client,
}

impl Default for Anthropic {
    fn default() -> Self {
        Self::new(&std::env::var("BATSCRIBE_ANTHROPIC_URL").unwrap_or_else(|_| DEFAULT_BASE.into()))
    }
}

impl Anthropic {
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
        }
    }
}

/// The request body. Contains no credentials, so it is stored in history as is. `reasoning`
/// carries the effort level.
pub fn request_body(r: &EditRequest) -> Value {
    let mut output_config = json!({
        "format": {"type": "json_schema", "schema": response_schema()}
    });
    if !r.reasoning.is_empty() {
        output_config["effort"] = json!(r.reasoning);
    }
    let mut body = json!({
        "model": r.model,
        "max_tokens": r.max_output_tokens.max(MIN_MAX_TOKENS),
        "system": INSTRUCTIONS,
        "messages": [{"role": "user", "content": user_payload(r).to_string()}],
        "output_config": output_config
    });
    if !r.thinking {
        body["thinking"] = json!({"type": "disabled"});
    }
    body
}

/// Extracts the structured answer from a Messages API response. Content is read by block
/// type: a response can begin with `thinking` blocks.
pub fn output_text(v: &Value) -> Result<String, String> {
    match v.get("stop_reason").and_then(Value::as_str) {
        Some("refusal") => {
            let category = v
                .pointer("/stop_details/category")
                .and_then(Value::as_str)
                .unwrap_or("unspecified");
            return Err(format!("model declined the request ({category})"));
        }
        Some("max_tokens") => return Err("response cut off at max_tokens".into()),
        _ => {}
    }
    let text: String = v
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect();
    if text.trim().is_empty() {
        return Err("response has no text".into());
    }
    Ok(text)
}

impl Composer for Anthropic {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    fn edit(&self, request: EditRequest, key: Option<ApiKey>) -> EditFuture {
        let url = format!("{}/v1/messages", self.base_url);
        let client = self.client.clone();
        Box::pin(async move {
            let body = request_body(&request);
            let mut outcome = EditOutcome {
                request_body: serde_json::to_string_pretty(&body).unwrap_or_default(),
                raw_response: None,
                request_id: None,
                usage: None,
                result: Err(String::new()),
            };
            let Some(key) = key else {
                outcome.result = Err("No Anthropic API key".into());
                return outcome;
            };
            let response = match client
                .post(&url)
                .header("x-api-key", key.expose())
                .header("anthropic-version", API_VERSION)
                .json(&body)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    outcome.result = Err(format!("request failed: {e}"));
                    return outcome;
                }
            };
            let status = response.status();
            outcome.request_id = response
                .headers()
                .get("request-id")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let text = match response.text().await {
                Ok(t) => t,
                Err(e) => {
                    outcome.result = Err(format!("reading the response failed: {e}"));
                    return outcome;
                }
            };
            outcome.raw_response = Some(text.clone());
            let v: Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(_) => {
                    outcome.result = Err(format!("HTTP {status}: response is not JSON"));
                    return outcome;
                }
            };
            outcome.usage = v.get("usage").cloned();
            if !status.is_success() {
                let msg = v
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("request failed");
                outcome.result = Err(format!("HTTP {status}: {msg}"));
                return outcome;
            }
            outcome.result = output_text(&v).and_then(|t| parse_decision(&t));
            outcome
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composition::Decision;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn request() -> EditRequest {
        EditRequest {
            operation_id: "op".into(),
            recording_id: "r".into(),
            composed_version: 1,
            watermark: 0,
            context: String::new(),
            segments: vec!["buy seven apples, no actually six apples".into()],
            composed: "buy seven apples, no actually six apples".into(),
            vocabulary: vec![],
            model: "claude-haiku-5-5".into(),
            reasoning: "low".into(),
            max_output_tokens: 800,
            thinking: true,
        }
    }

    #[tokio::test]
    async fn sends_structured_request_and_reads_text_after_thinking() {
        let server = MockServer::start().await;
        let body = json!({
            "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-haiku-5-5",
            "content": [
                {"type": "thinking", "thinking": "", "signature": "sig"},
                {"type": "text", "text": "{\"decision\":\"replace\",\"text\":\"buy six apples\",\"reason\":\"correction\"}"}
            ],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 410, "output_tokens": 30}
        });
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("x-api-key", "sk-ant-test"))
            .and(header("anthropic-version", API_VERSION))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(body)
                    .insert_header("request-id", "req_7"),
            )
            .mount(&server)
            .await;
        let out = Anthropic::new(&server.uri())
            .edit(request(), ApiKey::new("sk-ant-test"))
            .await;
        assert_eq!(out.result, Ok(Decision::Replace("buy six apples".into())));
        assert_eq!(out.request_id.as_deref(), Some("req_7"));
        assert_eq!(out.usage.unwrap()["output_tokens"], 30);
        assert!(
            !out.request_body.contains("sk-ant-test"),
            "the key never appears in history"
        );
        let sent: Value = serde_json::from_str(&out.request_body).unwrap();
        assert_eq!(sent["output_config"]["format"]["type"], "json_schema");
        assert_eq!(sent["output_config"]["effort"], "low");
        assert_eq!(sent["max_tokens"], MIN_MAX_TOKENS, "room for thinking");
        assert!(sent.get("temperature").is_none() && sent.get("thinking").is_none());
        let mut off = request();
        off.thinking = false;
        assert_eq!(request_body(&off)["thinking"], json!({"type": "disabled"}));
        assert_eq!(
            sent["messages"].as_array().unwrap().len(),
            1,
            "ends with a user turn"
        );
    }

    #[tokio::test]
    async fn errors_refusals_and_truncation_fail_the_edit() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(529).set_body_json(
                json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}),
            ))
            .mount(&server)
            .await;
        let out = Anthropic::new(&server.uri())
            .edit(request(), ApiKey::new("k"))
            .await;
        assert!(out.result.unwrap_err().contains("Overloaded"));
        let refusal = json!({"content": [], "stop_reason": "refusal",
                             "stop_details": {"type": "refusal", "category": "cyber"}});
        assert!(output_text(&refusal).unwrap_err().contains("cyber"));
        let cut =
            json!({"content": [{"type": "thinking", "thinking": ""}], "stop_reason": "max_tokens"});
        assert!(output_text(&cut).is_err());
    }
}
