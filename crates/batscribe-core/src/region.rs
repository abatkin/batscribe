//! The core's authoritative copy of one recording's voice region.
//!
//! The region holds composed text (finalized recognition, possibly rewritten by the editing
//! model) followed by the provisional tail (the current interim hypothesis). Every change is
//! turned into a minimal [`RegionUpdate`] for the editor, which applies it to its own copy. Only
//! this type changes region contents, so the two copies stay identical.

use crate::editor::{RegionEdit, RegionUpdate};
use crate::text::{first_sentence_end, minimal_diff, sentence_count, word_count};

/// A finalized recognition segment still inside the region.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub id: String,
    pub text: String,
    /// Recording-relative sample positions, half-open.
    pub start_sample: u64,
    pub end_sample: u64,
}

/// Bounds on the revisable part of the region.
#[derive(Debug, Clone, Copy)]
pub struct RegionLimits {
    pub max_sentences: usize,
    pub max_words: usize,
    pub max_seconds: f64,
    pub sample_rate: u32,
}

impl Default for RegionLimits {
    fn default() -> Self {
        Self {
            max_sentences: 2,
            max_words: 150,
            max_seconds: 30.0,
            sample_rate: 16_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the region changed since the request was made")]
pub struct StaleTarget;

fn join(a: &str, b: &str) -> String {
    if a.is_empty() || b.is_empty() || a.ends_with(char::is_whitespace) {
        format!("{a}{b}")
    } else {
        format!("{a} {b}")
    }
}

#[derive(Debug, Clone)]
pub struct VoiceRegion {
    recording_id: String,
    revision: u64,
    /// The character before the region is not whitespace, so text needs a leading space.
    lead: bool,
    composed: String,
    provisional: String,
    /// What the editor currently shows, and where its provisional tail starts.
    displayed: String,
    displayed_from: usize,
    /// Raw segments that the composed text covers.
    segments: Vec<Segment>,
    /// Incremented whenever composed text changes; requests carry it to detect staleness.
    composed_version: u64,
    /// Segments that left the region; their text is never emitted again.
    consumed: usize,
    /// Bytes at the start of `composed` that the editing model has already seen (or that need
    /// no review). Only reviewed text may leave the region, so a correction can't be split from
    /// what it corrects before the model sees both.
    reviewed: usize,
    /// Bytes at the start of `composed` written by the model. Their raw segments were consumed
    /// when the rewrite was applied, so raw segments line up with the text after this prefix.
    rewritten: usize,
}

impl VoiceRegion {
    pub fn new(recording_id: &str, lead: bool) -> Self {
        Self {
            recording_id: recording_id.to_owned(),
            revision: 0,
            lead,
            composed: String::new(),
            provisional: String::new(),
            displayed: String::new(),
            displayed_from: 0,
            segments: Vec::new(),
            composed_version: 0,
            consumed: 0,
            reviewed: 0,
            rewritten: 0,
        }
    }

    pub fn recording_id(&self) -> &str {
        &self.recording_id
    }

    pub fn composed(&self) -> &str {
        &self.composed
    }

    pub fn provisional(&self) -> &str {
        &self.provisional
    }

    pub fn displayed(&self) -> &str {
        &self.displayed
    }

    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    pub fn composed_version(&self) -> u64 {
        self.composed_version
    }

    /// Number of segments that have left the region (the source-consumption watermark).
    pub fn consumed(&self) -> usize {
        self.consumed
    }

    pub fn reviewed(&self) -> usize {
        self.reviewed
    }

    /// Records that `prefix` (a prefix of the composed text) was reviewed. Ignored if the text
    /// changed since.
    pub fn mark_reviewed(&mut self, prefix: &str) {
        if self.composed.starts_with(prefix) {
            self.reviewed = self.reviewed.max(prefix.len());
        }
    }

    /// Marks all composed text reviewed (no editing model, or it gave up on this text).
    pub fn mark_all_reviewed(&mut self) {
        self.reviewed = self.composed.len();
    }

    pub fn is_empty(&self) -> bool {
        self.displayed.is_empty()
    }

    fn render(&self) -> (String, usize) {
        let body = join(&self.composed, &self.provisional);
        let lead = if self.lead && !body.is_empty() {
            " "
        } else {
            ""
        };
        let shown = format!("{lead}{body}");
        let from = shown.len() - self.provisional.len();
        (shown, from)
    }

    /// Emits the difference between what the editor shows (after `release` bytes leave) and the
    /// current rendering.
    fn emit(&mut self, release: usize, source: Option<String>) -> Option<RegionUpdate> {
        let (shown, from) = self.render();
        let old = &self.displayed[release..];
        if release == 0 && old == shown && from == self.displayed_from {
            return None;
        }
        let edit = (old != shown).then(|| {
            let (p, o, n) = minimal_diff(old, &shown);
            RegionEdit {
                start: p,
                end: o,
                text: shown[p..n].to_owned(),
            }
        });
        self.displayed = shown;
        self.displayed_from = from;
        self.revision += 1;
        Some(RegionUpdate {
            recording_id: self.recording_id.clone(),
            revision: self.revision,
            release,
            edit,
            provisional_from: from,
            source,
        })
    }

    /// Replaces the provisional tail with the current interim hypothesis.
    pub fn set_provisional(&mut self, text: &str, source: Option<String>) -> Option<RegionUpdate> {
        self.provisional = text.trim().to_owned();
        self.emit(0, source)
    }

    /// Appends a finalized segment to the composed text and clears the provisional tail.
    pub fn append_final(
        &mut self,
        segment: Segment,
        source: Option<String>,
    ) -> Option<RegionUpdate> {
        let text = segment.text.trim();
        if !text.is_empty() {
            self.composed = join(&self.composed, text);
            self.composed_version += 1;
        }
        self.segments.push(segment);
        self.provisional.clear();
        self.emit(0, source)
    }

    /// Replaces composed text that a model request saw as `expected` (a prefix of the current
    /// composed text) with `replacement`. Text appended since the request stays as a suffix.
    pub fn replace_composed_prefix(
        &mut self,
        expected: &str,
        replacement: &str,
        source: Option<String>,
    ) -> Result<Option<RegionUpdate>, StaleTarget> {
        if !self.composed.starts_with(expected) {
            return Err(StaleTarget);
        }
        let suffix = self.composed[expected.len()..].trim_start();
        let replacement = replacement.trim();
        let composed = join(replacement, suffix);
        if composed == self.composed {
            self.mark_reviewed(expected);
            return Ok(None);
        }
        // The raw segments behind the replaced text no longer match it; drop them so the model
        // isn't shown speech that is gone from the text.
        let raw_end = expected.len().max(self.rewritten.min(expected.len()));
        let raw_words = word_count(&expected[self.rewritten.min(raw_end)..raw_end]);
        self.consume_raw_words(raw_words);
        self.composed = composed;
        self.composed_version += 1;
        self.reviewed = replacement.len();
        self.rewritten = replacement.len();
        Ok(self.emit(0, source))
    }

    /// If the region exceeds its limits, releases the oldest complete sentence from its start.
    /// Call only when no request covering the region is in flight.
    pub fn release_over_limit(&mut self, limits: &RegionLimits) -> Option<RegionUpdate> {
        let sentences = sentence_count(&self.composed);
        let over = sentences > limits.max_sentences
            || word_count(&self.composed) > limits.max_words
            || self.span_seconds(limits.sample_rate) > limits.max_seconds;
        if !over || sentences < 2 {
            // Never release the only (or current) sentence while it might still be corrected.
            return None;
        }
        let end = first_sentence_end(&self.composed)?;
        if self.composed[..end].trim_end().len() > self.reviewed {
            // The model hasn't seen this sentence yet; it may still be corrected.
            return None;
        }
        self.release_composed(end)
    }

    /// Ends the region: any provisional text is kept as typed text and everything is released.
    pub fn release_all(&mut self) -> Option<RegionUpdate> {
        if !self.provisional.is_empty() {
            self.composed = join(&self.composed, &self.provisional);
            self.provisional.clear();
            self.composed_version += 1;
        }
        let end = self.composed.len();
        if end == 0 && self.displayed.is_empty() {
            return None;
        }
        self.release_composed(end)
    }

    fn release_composed(&mut self, end: usize) -> Option<RegionUpdate> {
        let lead = usize::from(self.lead && !self.displayed.is_empty());
        let released = self.composed[..end].to_owned();
        let release = (lead + end).min(self.displayed.len());
        self.composed = self.composed[end..].to_owned();
        self.composed_version += 1;
        if !released.is_empty() {
            self.lead = !released.ends_with(char::is_whitespace);
        }
        self.reviewed = self.reviewed.saturating_sub(end);
        // Rewritten text has no raw segments; the rest lines up with them word for word.
        let from_rewritten = end.min(self.rewritten);
        self.rewritten -= from_rewritten;
        self.consume_raw_words(word_count(&released[from_rewritten..]));
        self.emit(release, None)
    }

    /// Drops the raw recognition covering the first `words` words of the raw-backed text (a
    /// segment can hold several sentences, so it may be trimmed rather than dropped). With no
    /// composed text left, all raw segments go.
    fn consume_raw_words(&mut self, mut words: usize) {
        while words > 0 || self.composed.is_empty() {
            let Some(seg) = self.segments.first_mut() else {
                break;
            };
            let n = word_count(&seg.text);
            if n <= words || self.composed.is_empty() {
                words = words.saturating_sub(n);
                self.segments.remove(0);
                self.consumed += 1;
            } else {
                seg.text = seg
                    .text
                    .split_whitespace()
                    .skip(words)
                    .collect::<Vec<_>>()
                    .join(" ");
                break;
            }
        }
    }

    fn span_seconds(&self, rate: u32) -> f64 {
        match (self.segments.first(), self.segments.last()) {
            (Some(a), Some(b)) if rate > 0 => {
                b.end_sample.saturating_sub(a.start_sample) as f64 / rate as f64
            }
            _ => 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::EditorModel;

    fn seg(text: &str, start: u64, end: u64) -> Segment {
        Segment {
            id: format!("s{start}"),
            text: text.to_owned(),
            start_sample: start,
            end_sample: end,
        }
    }

    /// Applies every update to an editor model and checks the copies agree.
    fn sync(m: &mut EditorModel, r: &VoiceRegion, u: Option<RegionUpdate>) {
        if let Some(u) = u {
            m.apply_region(&u, 0).expect("update applies");
        }
        assert_eq!(m.region_text(), Some(r.displayed()));
    }

    #[test]
    fn interim_final_and_correction_flow() {
        let mut m = EditorModel::new("Note:".into(), 0);
        let pos = m.start_position(0, false);
        let lead = m.needs_leading_space(pos);
        m.open_region("rec", pos, 0);
        let mut r = VoiceRegion::new("rec", lead);

        let u = r.set_provisional("at the", None);
        sync(&mut m, &r, u);
        assert_eq!(m.text(), "Note: at the");
        let u = r.set_provisional("at the store please", None);
        sync(&mut m, &r, u);
        let u = r.append_final(seg("At the store, please buy seven oranges,", 0, 100), None);
        sync(&mut m, &r, u);
        let u = r.set_provisional("no actually", None);
        sync(&mut m, &r, u);
        assert_eq!(m.highlights().len(), 2);
        let snapshot = r.composed().to_owned();
        let u = r.append_final(seg("no, actually six oranges.", 100, 200), None);
        sync(&mut m, &r, u);
        // The model saw only the first segment; the second remains a suffix.
        let u = r
            .replace_composed_prefix(&snapshot, "At the store, please buy seven oranges,", None)
            .unwrap();
        assert!(u.is_none(), "unchanged replacement emits nothing");
        let full = r.composed().to_owned();
        let u = r
            .replace_composed_prefix(&full, "At the store, please buy six oranges.", None)
            .unwrap();
        sync(&mut m, &r, u);
        assert_eq!(m.text(), "Note: At the store, please buy six oranges.");
        assert_eq!(
            r.replace_composed_prefix(&full, "x", None),
            Err(StaleTarget),
            "stale target is rejected"
        );
        let u = r.release_all();
        sync(&mut m, &r, u);
        assert_eq!(m.region_text(), Some(""));
        assert!(m.close_region("rec"));
        assert_eq!(m.text(), "Note: At the store, please buy six oranges.");
    }

    #[test]
    fn only_reviewed_text_is_released() {
        let mut m = EditorModel::new(String::new(), 0);
        m.open_region("rec", 0, 0);
        let mut r = VoiceRegion::new("rec", false);
        let limits = RegionLimits::default();
        // One final holding four sentences, the correction in the middle.
        let u = r.append_final(
            seg(
                "Want six apples. Actually, no. I don't want six. I want seven apples.",
                0,
                10,
            ),
            None,
        );
        sync(&mut m, &r, u);
        assert!(r.release_over_limit(&limits).is_none(), "not reviewed yet");
        let seen = r.composed().to_owned();
        let u = r
            .replace_composed_prefix(&seen, "I want seven apples.", None)
            .unwrap();
        sync(&mut m, &r, u);
        assert_eq!(m.text(), "I want seven apples.");
        // A long reviewed text is released one sentence at a time, and the raw segment is
        // trimmed to match.
        let mut r = VoiceRegion::new("rec2", false);
        r.append_final(seg("One a. Two b. Three c. Four d.", 0, 10), None);
        r.mark_reviewed("One a. Two b. Three c. Four d.");
        assert!(r.release_over_limit(&limits).is_some());
        assert!(r.release_over_limit(&limits).is_some());
        assert!(r.release_over_limit(&limits).is_none());
        assert_eq!(r.composed(), "Three c. Four d.");
        assert_eq!(r.segments()[0].text, "Three c. Four d.");
    }

    /// Regression (third live run): after a correction was applied, later requests still showed
    /// the model the removed "Actually, no." as raw speech.
    #[test]
    fn raw_segments_follow_rewrites_and_releases() {
        let mut r = VoiceRegion::new("rec", false);
        r.append_final(seg("Hello there.", 0, 1), None);
        r.mark_all_reviewed();
        r.append_final(
            seg(
                "I want six apples. Actually, no. I want seven apples.",
                1,
                2,
            ),
            None,
        );
        let seen = r.composed().to_owned();
        r.replace_composed_prefix(&seen, "Hello there. I want seven apples.", None)
            .unwrap();
        assert!(r.segments().is_empty(), "{:?}", r.segments());
        r.append_final(seg("Now check the key.", 2, 3), None);
        r.mark_all_reviewed();
        let limits = RegionLimits::default();
        assert!(r.release_over_limit(&limits).is_some());
        assert_eq!(r.composed(), "I want seven apples. Now check the key.");
        assert_eq!(r.segments().len(), 1);
        assert_eq!(
            r.segments()[0].text,
            "Now check the key.",
            "released rewritten text consumed no raw words"
        );
        r.append_final(seg("Done.", 3, 4), None);
        r.mark_all_reviewed();
        assert!(r.release_over_limit(&limits).is_some());
        assert_eq!(r.composed(), "Now check the key. Done.");
        let raw: Vec<&str> = r.segments().iter().map(|s| s.text.as_str()).collect();
        assert_eq!(raw, ["Now check the key.", "Done."]);
        r.release_all();
        assert!(r.segments().is_empty());
    }

    #[test]
    fn releases_oldest_sentence_over_limit() {
        let mut m = EditorModel::new(String::new(), 0);
        m.open_region("rec", 0, 0);
        let mut r = VoiceRegion::new("rec", false);
        let limits = RegionLimits::default();
        for (i, s) in ["One here.", "Two there.", "Three now."].iter().enumerate() {
            let u = r.append_final(seg(s, i as u64 * 10, i as u64 * 10 + 10), None);
            sync(&mut m, &r, u);
        }
        r.mark_all_reviewed();
        let u = r.release_over_limit(&limits);
        assert!(u.is_some());
        sync(&mut m, &r, u);
        assert_eq!(r.composed(), "Two there. Three now.");
        assert_eq!(r.consumed(), 1);
        assert_eq!(m.region().unwrap().start, "One here. ".len());
        assert!(r.release_over_limit(&limits).is_none());
        // Typing in released text is allowed now.
        assert_eq!(
            m.user_edit(0, 0, "X", m.utf16_len() + 1, 0),
            crate::editor::UserEditOutcome::Accepted
        );
    }

    #[test]
    fn release_all_keeps_provisional_text() {
        let mut m = EditorModel::new("Hi".into(), 0);
        m.open_region("rec", 2, 0);
        let mut r = VoiceRegion::new("rec", true);
        let u = r.append_final(seg("first.", 0, 1), None);
        sync(&mut m, &r, u);
        let u = r.set_provisional("second part", None);
        sync(&mut m, &r, u);
        let u = r.release_all();
        sync(&mut m, &r, u);
        assert!(r.is_empty());
        assert_eq!(m.text(), "Hi first. second part");
    }

    #[test]
    fn release_without_trailing_space_restores_lead() {
        let mut m = EditorModel::new(String::new(), 0);
        m.open_region("rec", 0, 0);
        let mut r = VoiceRegion::new("rec", false);
        for s in ["A.", "B.", "C."] {
            let u = r.append_final(seg(s, 0, 1), None);
            sync(&mut m, &r, u);
        }
        r.mark_all_reviewed();
        let u = r.release_over_limit(&RegionLimits::default());
        sync(&mut m, &r, u);
        let u = r.set_provisional("d", None);
        sync(&mut m, &r, u);
        assert_eq!(m.text(), "A. B. C. d");
    }
}
