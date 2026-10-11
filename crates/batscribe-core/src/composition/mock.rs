//! Rule-based stand-in for the editing model, for development and tests. It handles the
//! "X, no actually Y" pattern by replacing the corrected words, and defers when a correction
//! phrase has nothing after it yet.

use std::time::Duration;

use serde_json::json;

use super::{Composer, Decision, EditFuture, EditOutcome, EditRequest, user_payload};
use crate::credentials::ApiKey;

pub struct MockComposer {
    pub latency: Duration,
}

impl Default for MockComposer {
    fn default() -> Self {
        Self {
            latency: Duration::from_millis(
                std::env::var("BATSCRIBE_MOCK_EDIT_MS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(300),
            ),
        }
    }
}

const PHRASES: &[&str] = &[
    "no, actually",
    "no actually",
    "actually no,",
    "actually no",
    "i mean",
];

fn norm(w: &str) -> String {
    w.trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

/// Applies the first explicit correction in `text`, if any.
pub fn correct(text: &str) -> Decision {
    let lower = text.to_lowercase();
    let Some((at, phrase)) = PHRASES
        .iter()
        .filter_map(|p| lower.find(p).map(|i| (i, *p)))
        .min_by_key(|(i, _)| *i)
    else {
        return Decision::NoChange;
    };
    let before = text[..at].trim_end().trim_end_matches(',').trim_end();
    let after_full = text[at + phrase.len()..].trim_start_matches([',', ' ']);
    // The correction runs to the end of its sentence.
    let end = after_full
        .find(['.', '?', '!'])
        .map_or(after_full.len(), |i| i + 1);
    let (after, rest) = after_full.split_at(end);
    let replacement: Vec<&str> = after.split_whitespace().collect();
    if replacement.is_empty() {
        return Decision::Defer;
    }
    let mut words: Vec<&str> = before.split_whitespace().collect();
    // Replace as many trailing words as the correction has, aligned on its last word when that
    // word repeats ("seven oranges" -> "six oranges").
    let n = replacement.len().min(words.len());
    let last = norm(replacement[replacement.len() - 1]);
    let cut = if words.last().is_some_and(|w| norm(w) == last) {
        n
    } else {
        n.min(1)
    };
    words.truncate(words.len() - cut);
    let mut out = words.join(" ");
    if !out.is_empty() {
        out.push(' ');
    }
    out.push_str(&replacement.join(" "));
    let rest = rest.trim();
    if !rest.is_empty() {
        out.push(' ');
        out.push_str(rest);
    }
    if out == text {
        Decision::NoChange
    } else {
        Decision::Replace(out)
    }
}

impl Composer for MockComposer {
    fn name(&self) -> &'static str {
        "mock"
    }

    fn edit(&self, request: EditRequest, _key: Option<ApiKey>) -> EditFuture {
        let latency = self.latency;
        Box::pin(async move {
            tokio::time::sleep(latency).await;
            let decision = correct(&request.composed);
            let text = match &decision {
                Decision::Replace(t) => t.clone(),
                _ => String::new(),
            };
            let raw = json!({"decision": decision.as_str(), "text": text, "reason": "mock rule"});
            EditOutcome {
                request_body: json!({"model": "mock", "input": user_payload(&request)}).to_string(),
                raw_response: Some(raw.to_string()),
                request_id: None,
                usage: Some(json!({"input_tokens": 0, "output_tokens": 0})),
                result: Ok(decision),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrections() {
        assert_eq!(
            correct("At the store, please buy seven oranges, no, actually six oranges."),
            Decision::Replace("At the store, please buy six oranges.".into())
        );
        assert_eq!(
            correct("Meet at three, no actually four. Bring snacks."),
            Decision::Replace("Meet at four. Bring snacks.".into())
        );
        assert_eq!(correct("Nothing to fix here."), Decision::NoChange);
    }

    #[test]
    fn bare_phrase_defers() {
        assert_eq!(correct("Buy seven, no actually"), Decision::Defer);
    }
}
