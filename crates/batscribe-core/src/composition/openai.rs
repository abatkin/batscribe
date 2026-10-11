//! OpenAI Responses API adapter (`POST /v1/responses`) with structured output.

use std::time::Duration;

use serde_json::{Value, json};

use super::{
    Composer, EditFuture, EditOutcome, EditRequest, INSTRUCTIONS, parse_decision, response_schema,
    user_payload,
};
use crate::credentials::ApiKey;

pub const DEFAULT_BASE: &str = "https://api.openai.com/v1";

pub struct OpenAi {
    pub base_url: String,
    client: reqwest::Client,
}

impl Default for OpenAi {
    fn default() -> Self {
        Self::new(&std::env::var("BATSCRIBE_OPENAI_URL").unwrap_or_else(|_| DEFAULT_BASE.into()))
    }
}

impl OpenAi {
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

/// The request body. Contains no credentials, so it is stored in history as is.
pub fn request_body(r: &EditRequest) -> Value {
    let mut body = json!({
        "model": r.model,
        "instructions": INSTRUCTIONS,
        "input": [{
            "role": "user",
            "content": [{"type": "input_text", "text": user_payload(r).to_string()}]
        }],
        "text": {"format": {
            "type": "json_schema",
            "name": "region_edit",
            "strict": true,
            "schema": response_schema()
        }},
        "max_output_tokens": r.max_output_tokens,
        "store": false
    });
    if !r.reasoning.is_empty() {
        body["reasoning"] = json!({"effort": r.reasoning});
    }
    body
}

/// Extracts the output text (or refusal) from a Responses API response.
pub fn output_text(v: &Value) -> Result<String, String> {
    if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
        return Err(err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("error")
            .to_owned());
    }
    if v.get("status").and_then(Value::as_str) == Some("incomplete") {
        let reason = v
            .pointer("/incomplete_details/reason")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        return Err(format!("response incomplete ({reason})"));
    }
    for item in v
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if item.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        for c in item
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            match c.get("type").and_then(Value::as_str) {
                Some("output_text") => {
                    return c
                        .get("text")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .ok_or_else(|| "output_text without text".into());
                }
                Some("refusal") => {
                    return Err(format!(
                        "model refused: {}",
                        c.get("refusal").and_then(Value::as_str).unwrap_or("")
                    ));
                }
                _ => {}
            }
        }
    }
    Err("response has no output text".into())
}

impl Composer for OpenAi {
    fn name(&self) -> &'static str {
        "openai"
    }

    fn edit(&self, request: EditRequest, key: Option<ApiKey>) -> EditFuture {
        let url = format!("{}/responses", self.base_url);
        let client = self.client.clone();
        Box::pin(async move {
            let body = request_body(&request);
            let request_body = serde_json::to_string_pretty(&body).unwrap_or_default();
            let mut outcome = EditOutcome {
                request_body,
                raw_response: None,
                request_id: None,
                usage: None,
                result: Err(String::new()),
            };
            let Some(key) = key else {
                outcome.result = Err("No OpenAI API key".into());
                return outcome;
            };
            let response = match client
                .post(&url)
                .bearer_auth(key.expose())
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
                .get("x-request-id")
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
            segments: vec!["buy seven oranges, no actually six oranges".into()],
            composed: "buy seven oranges, no actually six oranges".into(),
            vocabulary: vec![],
            model: "gpt-6-luna".into(),
            reasoning: "none".into(),
            max_output_tokens: 200,
            thinking: true,
        }
    }

    #[tokio::test]
    async fn parses_structured_output_and_usage() {
        let server = MockServer::start().await;
        let body = json!({
            "id": "resp_1", "status": "completed",
            "output": [{"type": "message", "role": "assistant", "content": [
                {"type": "output_text", "text": "{\"decision\":\"replace\",\"text\":\"buy six oranges\",\"reason\":\"correction\"}"}
            ]}],
            "usage": {"input_tokens": 120, "output_tokens": 12}
        });
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(header("authorization", "Bearer sk-test"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(body)
                    .insert_header("x-request-id", "req_9"),
            )
            .mount(&server)
            .await;
        let out = OpenAi::new(&server.uri())
            .edit(request(), ApiKey::new("sk-test"))
            .await;
        assert_eq!(out.result, Ok(Decision::Replace("buy six oranges".into())));
        assert_eq!(out.request_id.as_deref(), Some("req_9"));
        assert_eq!(out.usage.unwrap()["output_tokens"], 12);
        assert!(
            !out.request_body.contains("sk-test"),
            "the key never appears in history"
        );
        let sent: Value = serde_json::from_str(&out.request_body).unwrap();
        assert_eq!(sent["reasoning"]["effort"], "none");
        assert_eq!(sent["text"]["format"]["type"], "json_schema");
    }

    #[tokio::test]
    async fn http_errors_and_refusals_are_failures() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(429)
                    .set_body_json(json!({"error": {"message": "slow down"}})),
            )
            .mount(&server)
            .await;
        let out = OpenAi::new(&server.uri())
            .edit(request(), ApiKey::new("k"))
            .await;
        assert!(out.result.unwrap_err().contains("slow down"));
        let refusal = json!({"output": [{"type": "message", "content": [{"type": "refusal", "refusal": "no"}]}]});
        assert!(output_text(&refusal).is_err());
    }
}
