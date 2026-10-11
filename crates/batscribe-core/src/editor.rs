//! Qt-free model of one open document as the editor shows it.
//!
//! The desktop crate keeps one [`EditorModel`] per open document and calls it on the GUI thread,
//! which makes the GUI thread the single place where mutations are ordered. Human edits arrive
//! after Qt has applied them; voice changes arrive as [`RegionUpdate`]s from the core. The voice
//! region is read-only to the user, so the two streams never touch the same text: a human edit
//! that would change the region is reverted, and region updates never need rebasing.
//!
//! All positions are UTF-8 byte offsets internally; [`QtEdit`] and the public `*_utf16` methods
//! use Qt's UTF-16 positions.

use serde::{Deserialize, Serialize};

use crate::text::{byte_to_utf16, minimal_diff, utf16_len, utf16_to_byte};

/// A change for the Qt document to apply programmatically, in UTF-16 positions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QtEdit {
    pub position: usize,
    pub remove: usize,
    pub insert: String,
}

/// One region change sent by the core. Offsets are bytes within the region's text, after
/// `release` bytes have left the region at its start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegionUpdate {
    pub recording_id: String,
    /// Must be exactly one more than the region's current revision.
    pub revision: u64,
    /// Bytes that leave the region at its start and become ordinary user text.
    pub release: usize,
    pub edit: Option<RegionEdit>,
    /// Offset within the (post-update) region text where the provisional tail begins.
    pub provisional_from: usize,
    /// Operation or event that produced the change, for provenance.
    pub source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegionEdit {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    pub recording_id: String,
    pub start: usize,
    pub end: usize,
    pub provisional_from: usize,
    pub revision: u64,
}

impl Region {
    fn len(&self) -> usize {
        self.end - self.start
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Actor {
    Human,
    Voice,
    Undo,
    Redo,
    /// A human change that touched the voice region and was reverted.
    Blocked,
}

impl Actor {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Voice => "voice",
            Self::Undo => "undo",
            Self::Redo => "redo",
            Self::Blocked => "blocked",
        }
    }
}

/// An applied (or blocked) mutation, reported to the core for history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditRecord {
    pub actor: Actor,
    pub before_revision: u64,
    pub after_revision: u64,
    /// Byte offset in the `before_revision` text.
    pub position: usize,
    pub removed: String,
    pub inserted: String,
    pub undo_group: Option<u64>,
    pub recording_id: Option<String>,
    pub source: Option<String>,
    pub at_ms: i64,
}

/// Voice text that came from a recording, kept as the document changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenanceSpan {
    pub start: usize,
    pub end: usize,
    pub recording_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserEditOutcome {
    /// Nothing changed (for example, a formatting-only notification).
    NoOp,
    Accepted,
    /// The change touched the voice region; apply this to restore the previous text.
    Revert(QtEdit),
    /// The reported change does not match the model; resynchronize from the full text.
    Desync,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegionError {
    #[error("no voice region for recording {0}")]
    NoRegion(String),
    #[error("region revision {got} does not follow {have}")]
    Revision { have: u64, got: u64 },
    #[error("region update is out of bounds")]
    Bounds,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UndoError {
    #[error("nothing to undo")]
    Empty,
    #[error("the text changed since this edit, so it can't be undone")]
    Conflict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupKind {
    Insert,
    Delete,
    Other,
}

fn group_kind(p: usize, q: usize, ins: &str) -> GroupKind {
    match (p == q, ins.is_empty()) {
        (true, false) => GroupKind::Insert,
        (false, true) => GroupKind::Delete,
        _ => GroupKind::Other,
    }
}

/// One human edit: `removed` was replaced by `inserted` at `position`, in the document as it
/// was right after the edit (undo is linear, so that state comes back before the entry is
/// undone). Voice edits are the exception; `transform_for_voice` moves entries through them.
#[derive(Debug, Clone)]
struct UndoEntry {
    position: usize,
    removed: String,
    inserted: String,
    /// The edit was not after the voice region, so voice text at its exact position belongs
    /// after it.
    voice_after: bool,
}

#[derive(Debug, Clone)]
struct UndoGroup {
    id: u64,
    kind: GroupKind,
    last_at_ms: i64,
    entries: Vec<UndoEntry>,
}

#[derive(Debug, Clone)]
pub struct EditorModel {
    text: String,
    revision: u64,
    region: Option<Region>,
    undo: Vec<UndoGroup>,
    redo: Vec<UndoGroup>,
    next_group: u64,
    group_interval_ms: i64,
    log: Vec<EditRecord>,
    provenance: Vec<ProvenanceSpan>,
}

impl Default for EditorModel {
    fn default() -> Self {
        Self::new(String::new(), 0)
    }
}

/// Where a change `[p, q)` lies relative to the region `[s, e)`.
fn touches_region(p: usize, q: usize, s: usize, e: usize) -> bool {
    if p == q {
        s < p && p < e
    } else {
        p < e && q > s
    }
}

/// Moves a text range `[pos, pos + len)` through a voice edit `[p, q)` -> `ins_len` bytes made
/// in the same coordinate state. An edit ending before the range shifts it; one starting after
/// it leaves it. For an empty range (a deletion point) exactly at an insertion, `voice_after`
/// says whether voice text belongs after that point. Returns false when the edit overlaps it.
fn transform_range(
    pos: &mut usize,
    len: usize,
    p: usize,
    q: usize,
    ins_len: usize,
    voice_after: bool,
) -> bool {
    let (a, b) = (*pos, *pos + len);
    if len == 0 && p == q && p == a {
        if !voice_after {
            *pos = a + ins_len;
        }
        return true;
    }
    if q <= a {
        *pos = a + ins_len - (q - p);
        true
    } else {
        p >= b
    }
}

/// Maps a position `x` across the replacement of `[pos, pos + from)` by `to` bytes (into the
/// state on the other side of an undo entry). `None` if `x` falls inside the replaced text.
fn map_point(x: usize, pos: usize, from: usize, to: usize, voice_after: bool) -> Option<usize> {
    if from == 0 && x == pos {
        return Some(if voice_after { x + to } else { x });
    }
    if x <= pos {
        Some(x)
    } else if x >= pos + from {
        Some(x + to - from)
    } else {
        None
    }
}

impl EditorModel {
    pub fn new(text: String, revision: u64) -> Self {
        Self {
            text,
            revision,
            region: None,
            undo: Vec::new(),
            redo: Vec::new(),
            next_group: 1,
            group_interval_ms: 1000,
            log: Vec::new(),
            provenance: Vec::new(),
        }
    }

    pub fn with_provenance(mut self, spans: Vec<ProvenanceSpan>) -> Self {
        self.provenance = spans
            .into_iter()
            .filter(|s| {
                s.start < s.end
                    && s.end <= self.text.len()
                    && self.text.is_char_boundary(s.start)
                    && self.text.is_char_boundary(s.end)
            })
            .collect();
        self
    }

    pub fn set_group_interval_ms(&mut self, ms: i64) {
        self.group_interval_ms = ms.max(0);
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn region(&self) -> Option<&Region> {
        self.region.as_ref()
    }

    pub fn region_text(&self) -> Option<&str> {
        self.region.as_ref().map(|r| &self.text[r.start..r.end])
    }

    pub fn provenance(&self) -> &[ProvenanceSpan] {
        &self.provenance
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn utf16_len(&self) -> usize {
        utf16_len(&self.text)
    }

    pub fn to_utf16(&self, byte: usize) -> usize {
        byte_to_utf16(&self.text, byte)
    }

    pub fn from_utf16(&self, pos: usize) -> Option<usize> {
        utf16_to_byte(&self.text, pos)
    }

    /// Drains the edits applied since the last call.
    pub fn take_log(&mut self) -> Vec<EditRecord> {
        std::mem::take(&mut self.log)
    }

    pub fn has_pending_log(&self) -> bool {
        !self.log.is_empty()
    }

    /// Text before the voice region (or the whole text), limited to the last `max_paragraphs`
    /// non-empty paragraphs and `max_bytes`, for read-only composition context.
    pub fn context_before_region(&self, max_paragraphs: usize, max_bytes: usize) -> String {
        let end = self.region.as_ref().map_or(self.text.len(), |r| r.start);
        let before = &self.text[..end];
        let paras: Vec<&str> = before
            .split("\n\n")
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .collect();
        let start = paras.len().saturating_sub(max_paragraphs);
        let joined = paras[start..].join("\n\n");
        if joined.len() <= max_bytes {
            return joined;
        }
        let mut cut = joined.len() - max_bytes;
        while !joined.is_char_boundary(cut) {
            cut += 1;
        }
        joined[cut..].to_owned()
    }

    /// Where a recording started with the caret at byte `cursor` should place its region: the end
    /// of the logical line, or the caret itself when `exact`.
    pub fn start_position(&self, cursor: usize, exact: bool) -> usize {
        let cursor = cursor.min(self.text.len());
        if exact {
            return cursor;
        }
        self.text[cursor..]
            .find('\n')
            .map_or(self.text.len(), |i| cursor + i)
    }

    /// Whether dictation at byte `pos` needs a space before its first word.
    pub fn needs_leading_space(&self, pos: usize) -> bool {
        self.text[..pos]
            .chars()
            .next_back()
            .is_some_and(|c| !c.is_whitespace())
    }

    /// Opens an empty voice region at byte `pos`. When text follows on the same line, a space is
    /// inserted after the region first so dictation doesn't run into it.
    pub fn open_region(&mut self, recording_id: &str, pos: usize, now_ms: i64) -> Vec<QtEdit> {
        if let Some(old) = self.region.as_ref().map(|r| r.recording_id.clone()) {
            self.close_region(&old);
        }
        let pos = pos.min(self.text.len());
        let mut edits = Vec::new();
        if self.text[pos..]
            .chars()
            .next()
            .is_some_and(|c| !c.is_whitespace())
        {
            edits.push(self.apply_voice(pos, pos, " ", Some(recording_id), None, now_ms));
        }
        self.region = Some(Region {
            recording_id: recording_id.to_owned(),
            start: pos,
            end: pos,
            provisional_from: 0,
            revision: 0,
        });
        edits
    }

    /// Applies a region update from the core.
    pub fn apply_region(
        &mut self,
        update: &RegionUpdate,
        now_ms: i64,
    ) -> Result<Vec<QtEdit>, RegionError> {
        let region = self
            .region
            .as_ref()
            .filter(|r| r.recording_id == update.recording_id)
            .ok_or_else(|| RegionError::NoRegion(update.recording_id.clone()))?;
        if update.revision != region.revision + 1 {
            return Err(RegionError::Revision {
                have: region.revision,
                got: update.revision,
            });
        }
        let (start, end) = (region.start, region.end);
        let released = start + update.release;
        if released > end || !self.text.is_char_boundary(released) {
            return Err(RegionError::Bounds);
        }
        let mut new_len = end - released;
        if let Some(edit) = &update.edit {
            let (p, q) = (released + edit.start, released + edit.end);
            if edit.start > edit.end
                || q > end
                || !self.text.is_char_boundary(p)
                || !self.text.is_char_boundary(q)
            {
                return Err(RegionError::Bounds);
            }
            new_len = new_len - (q - p) + edit.text.len();
        }
        if update.provisional_from > new_len {
            return Err(RegionError::Bounds);
        }

        let mut edits = Vec::new();
        if update.release > 0 {
            let recording_id = update.recording_id.clone();
            self.add_provenance(start, released, &recording_id);
            if let Some(r) = self.region.as_mut() {
                r.start = released;
            }
        }
        if let Some(edit) = &update.edit {
            let (p, q) = (released + edit.start, released + edit.end);
            if p != q || !edit.text.is_empty() {
                edits.push(self.apply_voice(
                    p,
                    q,
                    &edit.text,
                    Some(&update.recording_id),
                    update.source.as_deref(),
                    now_ms,
                ));
            }
        }
        if let Some(r) = self.region.as_mut() {
            r.revision = update.revision;
            r.provisional_from = update.provisional_from;
        }
        Ok(edits)
    }

    /// Ends the voice region; its text becomes ordinary user text.
    pub fn close_region(&mut self, recording_id: &str) -> bool {
        match self.region.take() {
            Some(r) if r.recording_id == recording_id => {
                self.add_provenance(r.start, r.end, recording_id);
                true
            }
            other => {
                self.region = other;
                false
            }
        }
    }

    fn add_provenance(&mut self, start: usize, end: usize, recording_id: &str) {
        if start >= end {
            return;
        }
        if let Some(last) = self
            .provenance
            .iter_mut()
            .find(|s| s.recording_id == recording_id && s.end == start)
        {
            last.end = end;
            return;
        }
        self.provenance.push(ProvenanceSpan {
            start,
            end,
            recording_id: recording_id.to_owned(),
        });
        self.provenance.sort_by_key(|s| s.start);
    }

    /// Recordings whose text overlaps bytes `[start, end)`.
    pub fn recordings_in(&self, start: usize, end: usize) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let hit = |s: &ProvenanceSpan| {
            if start == end {
                s.start <= start && start < s.end
            } else {
                s.start < end && s.end > start
            }
        };
        if let Some(r) = &self.region
            && (r.start < end.max(start + 1) && r.end > start)
        {
            out.push(r.recording_id.clone());
        }
        for s in self.provenance.iter().filter(|s| hit(s)) {
            if !out.contains(&s.recording_id) {
                out.push(s.recording_id.clone());
            }
        }
        out
    }

    /// Replaces `[p, q)` with `insert` on behalf of the voice pipeline (inside the region or the
    /// spacing next to it). Undo entries after the change shift; none overlap it.
    fn apply_voice(
        &mut self,
        p: usize,
        q: usize,
        insert: &str,
        recording_id: Option<&str>,
        source: Option<&str>,
        now_ms: i64,
    ) -> QtEdit {
        let qt = QtEdit {
            position: byte_to_utf16(&self.text, p),
            remove: utf16_len(&self.text[p..q]),
            insert: insert.to_owned(),
        };
        let removed = self.text[p..q].to_owned();
        let delta = insert.len() as isize - (q - p) as isize;
        self.transform_for_voice(p, q, insert.len());
        self.shift_provenance(p, q, insert.len());
        self.text.replace_range(p..q, insert);
        if let Some(r) = self.region.as_mut() {
            r.end = (r.end as isize + delta) as usize;
        }
        let before = self.revision;
        self.revision += 1;
        self.log.push(EditRecord {
            actor: Actor::Voice,
            before_revision: before,
            after_revision: self.revision,
            position: p,
            removed,
            inserted: insert.to_owned(),
            undo_group: None,
            recording_id: recording_id.map(str::to_owned),
            source: source.map(str::to_owned),
            at_ms: now_ms,
        });
        qt
    }

    /// Moves undo and redo entries through a voice edit `[p, q)` -> `ins_len` (current
    /// coordinates). Entries are stored in historical states, so the edit is carried down each
    /// stack: transformed into an entry's state, then mapped across that entry into the state
    /// beneath it. A group the edit can't be carried through (it overlaps the group's text)
    /// can't be undone or redone any more, and neither can anything after it.
    fn transform_for_voice(&mut self, p: usize, q: usize, ins_len: usize) {
        // Undo stack: newest group first, each group's entries newest first. The edit starts in
        // the state after the newest entry, which is the current document.
        let (mut vp, mut vq) = (p, q);
        let mut cut = None;
        'undo: for (i, group) in self.undo.iter_mut().enumerate().rev() {
            for e in group.entries.iter_mut().rev() {
                let (ins, rem) = (e.inserted.len(), e.removed.len());
                // Map the edit across the entry using the entry's position in the edit's own
                // state, then move the entry through the edit.
                let below = map_point(vp, e.position, ins, rem, e.voice_after).zip(map_point(
                    vq,
                    e.position,
                    ins,
                    rem,
                    e.voice_after,
                ));
                let mapped = below.filter(|_| {
                    transform_range(&mut e.position, ins, vp, vq, ins_len, e.voice_after)
                });
                match mapped {
                    Some((a, b)) => (vp, vq) = (a, b),
                    None => {
                        cut = Some(i);
                        break 'undo;
                    }
                }
            }
        }
        if let Some(i) = cut {
            self.undo.drain(..=i);
        }
        // Redo stack: the next group to redo first, its entries in order. The edit starts in
        // the state before that group, which is the current document.
        let (mut vp, mut vq) = (p, q);
        let mut cut = None;
        'redo: for (i, group) in self.redo.iter_mut().enumerate().rev() {
            for e in group.entries.iter_mut() {
                let (ins, rem) = (e.inserted.len(), e.removed.len());
                // Map the edit across the entry using the entry's position in the edit's own
                // state, then move the entry through the edit.
                let below = map_point(vp, e.position, rem, ins, e.voice_after).zip(map_point(
                    vq,
                    e.position,
                    rem,
                    ins,
                    e.voice_after,
                ));
                let mapped = below.filter(|_| {
                    transform_range(&mut e.position, rem, vp, vq, ins_len, e.voice_after)
                });
                match mapped {
                    Some((a, b)) => (vp, vq) = (a, b),
                    None => {
                        cut = Some(i);
                        break 'redo;
                    }
                }
            }
        }
        if let Some(i) = cut {
            self.redo.drain(..=i);
        }
    }

    fn shift_provenance(&mut self, p: usize, q: usize, inserted: usize) {
        let delta = inserted as isize - (q - p) as isize;
        let shift = |x: usize| (x as isize + delta) as usize;
        let mut out = Vec::with_capacity(self.provenance.len());
        for s in self.provenance.drain(..) {
            if q <= s.start {
                // Entirely after the change (an insertion at the span start precedes it).
                out.push(ProvenanceSpan {
                    start: shift(s.start),
                    end: shift(s.end),
                    ..s
                });
            } else if p >= s.end {
                out.push(s);
            } else {
                if s.start < p {
                    out.push(ProvenanceSpan {
                        start: s.start,
                        end: p,
                        recording_id: s.recording_id.clone(),
                    });
                }
                if s.end > q {
                    out.push(ProvenanceSpan {
                        start: shift(q),
                        end: shift(s.end),
                        recording_id: s.recording_id,
                    });
                }
            }
        }
        self.provenance = out;
    }

    /// Records a change Qt already applied to its document: at UTF-16 `position`, `removed`
    /// UTF-16 units were replaced by `inserted`. `new_length` is Qt's document length afterwards,
    /// for a consistency check.
    pub fn user_edit(
        &mut self,
        position: usize,
        removed: usize,
        inserted: &str,
        new_length: usize,
        now_ms: i64,
    ) -> UserEditOutcome {
        let (Some(p), Some(q)) = (
            utf16_to_byte(&self.text, position),
            utf16_to_byte(&self.text, position + removed),
        ) else {
            return UserEditOutcome::Desync;
        };
        if self.utf16_len() - removed + utf16_len(inserted) != new_length {
            return UserEditOutcome::Desync;
        }
        // Qt reports formatting-only changes as replacements of identical text; trim them.
        let (a, old_end, new_end) = minimal_diff(&self.text[p..q], inserted);
        let (p2, q2) = (p + a, p + old_end);
        let ins = &inserted[a..new_end];
        if p2 == q2 && ins.is_empty() {
            return UserEditOutcome::NoOp;
        }
        self.human_change(p2, q2, ins, Actor::Human, now_ms)
    }

    fn human_change(
        &mut self,
        p: usize,
        q: usize,
        ins: &str,
        actor: Actor,
        now_ms: i64,
    ) -> UserEditOutcome {
        if let Some(r) = &self.region
            && touches_region(p, q, r.start, r.end)
        {
            // Qt already shows the change; restore what the model still has.
            let revert = QtEdit {
                position: byte_to_utf16(&self.text, p),
                remove: utf16_len(ins),
                insert: self.text[p..q].to_owned(),
            };
            self.log.push(EditRecord {
                actor: Actor::Blocked,
                before_revision: self.revision,
                after_revision: self.revision,
                position: p,
                removed: self.text[p..q].to_owned(),
                inserted: ins.to_owned(),
                undo_group: None,
                recording_id: Some(r.recording_id.clone()),
                source: None,
                at_ms: now_ms,
            });
            return UserEditOutcome::Revert(revert);
        }
        let (group, merge) = self.undo_group_for(p, q, ins, now_ms);
        let entry = UndoEntry {
            position: p,
            removed: self.text[p..q].to_owned(),
            inserted: ins.to_owned(),
            // The same rule `apply_human` uses to place the edit before or after the region.
            voice_after: self.region.as_ref().is_none_or(|r| q <= r.start),
        };
        match self.undo.last_mut() {
            Some(top) if merge && top.id == group => {
                top.entries.push(entry);
                top.last_at_ms = now_ms;
            }
            _ => self.undo.push(UndoGroup {
                id: group,
                kind: group_kind(p, q, ins),
                last_at_ms: now_ms,
                entries: vec![entry],
            }),
        }
        self.apply_human(p, q, ins, actor, Some(group), true, now_ms);
        UserEditOutcome::Accepted
    }

    /// Applies a human edit (or an undo/redo step). An edit ending before the region's start
    /// moves the region; one ending exactly at its start does when `before_region` (a new
    /// edit there, or an undo entry recorded before the region).
    #[allow(clippy::too_many_arguments)]
    fn apply_human(
        &mut self,
        p: usize,
        q: usize,
        ins: &str,
        actor: Actor,
        group: Option<u64>,
        before_region: bool,
        now_ms: i64,
    ) {
        let removed = self.text[p..q].to_owned();
        let delta = ins.len() as isize - (q - p) as isize;
        self.shift_provenance(p, q, ins.len());
        self.text.replace_range(p..q, ins);
        if let Some(r) = self.region.as_mut()
            && (q < r.start || (q == r.start && before_region))
        {
            // Entirely before the region (an insertion at its start lands before it).
            r.start = (r.start as isize + delta) as usize;
            r.end = (r.end as isize + delta) as usize;
        }
        let before = self.revision;
        self.revision += 1;
        self.log.push(EditRecord {
            actor,
            before_revision: before,
            after_revision: self.revision,
            position: p,
            removed,
            inserted: ins.to_owned(),
            undo_group: group,
            recording_id: None,
            source: None,
            at_ms: now_ms,
        });
    }

    /// The undo group a new human edit joins: the top group when this continues the same
    /// typing or deleting (merge = true), otherwise a new group. Clears redo.
    fn undo_group_for(&mut self, p: usize, q: usize, ins: &str, now_ms: i64) -> (u64, bool) {
        self.redo.clear();
        let kind = group_kind(p, q, ins);
        if let Some(top) = self.undo.last()
            && top.kind == kind
            && kind != GroupKind::Other
            && now_ms - top.last_at_ms <= self.group_interval_ms
            && !ins.contains('\n')
            && let Some(last) = top.entries.last()
        {
            let contiguous = match kind {
                GroupKind::Insert => p == last.position + last.inserted.len(),
                // Backspace (moving left) or Delete (staying put).
                GroupKind::Delete => q == last.position || p == last.position,
                GroupKind::Other => false,
            };
            if contiguous {
                return (top.id, true);
            }
        }
        let id = self.next_group;
        self.next_group += 1;
        (id, false)
    }

    /// Undoes the most recent group of human edits. Voice changes are never undone.
    pub fn undo(&mut self, now_ms: i64) -> Result<Vec<QtEdit>, UndoError> {
        let group = self.undo.pop().ok_or(UndoError::Empty)?;
        match self.replay(group, true, Actor::Undo, now_ms) {
            Ok((edits, group)) => {
                self.redo.push(group);
                Ok(edits)
            }
            Err(e) => {
                self.undo.clear();
                Err(e)
            }
        }
    }

    pub fn redo(&mut self, now_ms: i64) -> Result<Vec<QtEdit>, UndoError> {
        let group = self.redo.pop().ok_or(UndoError::Empty)?;
        match self.replay(group, false, Actor::Redo, now_ms) {
            Ok((edits, group)) => {
                self.undo.push(UndoGroup {
                    last_at_ms: i64::MIN / 2,
                    ..group
                });
                Ok(edits)
            }
            Err(e) => {
                self.redo.clear();
                Err(e)
            }
        }
    }

    /// Applies a group backwards (undo) or forwards (redo), checking every step on a scratch
    /// copy first so a conflict changes nothing. Entries are in historical coordinates, which
    /// linear undo restores step by step, so they need no adjustment.
    fn replay(
        &mut self,
        group: UndoGroup,
        backwards: bool,
        actor: Actor,
        now_ms: i64,
    ) -> Result<(Vec<QtEdit>, UndoGroup), UndoError> {
        let steps: Vec<(usize, String, String, bool)> = if backwards {
            group
                .entries
                .iter()
                .rev()
                .map(|e| {
                    (
                        e.position,
                        e.inserted.clone(),
                        e.removed.clone(),
                        e.voice_after,
                    )
                })
                .collect()
        } else {
            group
                .entries
                .iter()
                .map(|e| {
                    (
                        e.position,
                        e.removed.clone(),
                        e.inserted.clone(),
                        e.voice_after,
                    )
                })
                .collect()
        };
        let mut scratch = self.text.clone();
        let mut region = self.region.as_ref().map(|r| (r.start, r.end));
        for (pos, expect, replacement, before) in &steps {
            let end = pos + expect.len();
            if end > scratch.len()
                || !scratch.is_char_boundary(*pos)
                || !scratch.is_char_boundary(end)
                || scratch[*pos..end] != **expect
            {
                return Err(UndoError::Conflict);
            }
            if let Some((s, e)) = region {
                if touches_region(*pos, end, s, e) {
                    return Err(UndoError::Conflict);
                }
                if end < s || (end == s && *before) {
                    let d = replacement.len() as isize - expect.len() as isize;
                    region = Some(((s as isize + d) as usize, (e as isize + d) as usize));
                }
            }
            scratch.replace_range(*pos..end, replacement);
        }
        let mut edits = Vec::with_capacity(steps.len());
        for (pos, expect, replacement, before) in steps {
            let end = pos + expect.len();
            edits.push(QtEdit {
                position: byte_to_utf16(&self.text, pos),
                remove: utf16_len(&expect),
                insert: replacement.clone(),
            });
            self.apply_human(
                pos,
                end,
                &replacement,
                actor,
                Some(group.id),
                before,
                now_ms,
            );
        }
        Ok((edits, group))
    }

    /// Brings the model in line with Qt's full text after a desync, treating the difference as
    /// one human edit (reverted if it touches the region).
    pub fn resync(&mut self, qt_text: &str, now_ms: i64) -> UserEditOutcome {
        let (p, old_end, new_end) = minimal_diff(&self.text, qt_text);
        if p == old_end && p == new_end {
            return UserEditOutcome::NoOp;
        }
        let ins = qt_text[p..new_end].to_owned();
        self.human_change(p, old_end, &ins, Actor::Human, now_ms)
    }

    /// Highlight ranges in UTF-16 positions: `(start, end, kind)` with kind `revisable` or
    /// `provisional`.
    pub fn highlights(&self) -> Vec<(usize, usize, &'static str)> {
        let Some(r) = &self.region else {
            return Vec::new();
        };
        let s = byte_to_utf16(&self.text, r.start);
        let m = byte_to_utf16(&self.text, r.start + r.provisional_from.min(r.len()));
        let e = byte_to_utf16(&self.text, r.end);
        let mut out = Vec::new();
        if m > s {
            out.push((s, m, "revisable"));
        }
        if e > m {
            out.push((m, e, "provisional"));
        }
        out
    }

    /// Whether a key that edits text at the given UTF-16 caret/selection would touch the region.
    /// `kind` is `insert`, `backspace` or `delete`. Word-wise deletions are left to the revert
    /// backstop.
    pub fn edit_would_touch_region(
        &self,
        kind: &str,
        cursor: usize,
        sel_start: usize,
        sel_end: usize,
    ) -> bool {
        let Some(r) = &self.region else {
            return false;
        };
        let to_byte = |u: usize| utf16_to_byte(&self.text, u);
        let (Some(c), Some(a), Some(b)) = (to_byte(cursor), to_byte(sel_start), to_byte(sel_end))
        else {
            return false;
        };
        let (p, q) = if a != b {
            (a.min(b), a.max(b))
        } else {
            match kind {
                "backspace" => (
                    self.text[..c]
                        .char_indices()
                        .next_back()
                        .map_or(c, |(i, _)| i),
                    c,
                ),
                "delete" => (
                    c,
                    self.text[c..]
                        .chars()
                        .next()
                        .map_or(c, |ch| c + ch.len_utf8()),
                ),
                _ => (c, c),
            }
        };
        if p == q && kind != "insert" {
            return false;
        }
        touches_region(p, q, r.start, r.end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upd(
        model: &EditorModel,
        release: usize,
        edit: Option<(usize, usize, &str)>,
    ) -> RegionUpdate {
        let r = model.region().unwrap();
        let len = r.end - r.start - release;
        let new_len = edit.map_or(len, |(s, e, t)| (len + t.len()).saturating_sub(e - s));
        RegionUpdate {
            recording_id: r.recording_id.clone(),
            revision: r.revision + 1,
            release,
            edit: edit.map(|(start, end, text)| RegionEdit {
                start,
                end,
                text: text.to_owned(),
            }),
            provisional_from: new_len,
            source: Some("test".into()),
        }
    }

    /// Simulates Qt applying a user edit at UTF-16 `pos` and reporting it.
    fn type_at(
        m: &mut EditorModel,
        qt: &mut String,
        pos: usize,
        remove: usize,
        ins: &str,
        t: i64,
    ) -> UserEditOutcome {
        let p = utf16_to_byte(qt, pos).unwrap();
        let q = utf16_to_byte(qt, pos + remove).unwrap();
        qt.replace_range(p..q, ins);
        let out = m.user_edit(pos, remove, ins, utf16_len(qt), t);
        if let UserEditOutcome::Revert(e) = &out {
            apply_qt(qt, e);
        }
        out
    }

    fn apply_qt(qt: &mut String, e: &QtEdit) {
        let p = utf16_to_byte(qt, e.position).unwrap();
        let q = utf16_to_byte(qt, e.position + e.remove).unwrap();
        qt.replace_range(p..q, &e.insert);
    }

    #[test]
    fn region_updates_and_typing_before_shift_region() {
        let mut m = EditorModel::new("Intro\nline two".into(), 0);
        let mut qt = m.text().to_owned();
        let pos = m.start_position(2, false);
        assert_eq!(pos, 5, "end of the logical line");
        assert!(m.needs_leading_space(pos));
        assert!(m.open_region("r1", pos, 0).is_empty());
        let u = upd(&m, 0, Some((0, 0, " buy seven")));
        for e in m.apply_region(&u, 1).unwrap() {
            apply_qt(&mut qt, &e);
        }
        assert_eq!(m.text(), "Intro buy seven\nline two");
        assert_eq!(qt, m.text());

        // Typing before the region moves it without touching its contents.
        assert_eq!(
            type_at(&mut m, &mut qt, 0, 0, "🎤 ", 2),
            UserEditOutcome::Accepted
        );
        assert_eq!(m.region_text(), Some(" buy seven"));
        let u = upd(&m, 0, Some((5, 10, "six")));
        for e in m.apply_region(&u, 3).unwrap() {
            apply_qt(&mut qt, &e);
        }
        assert_eq!(m.text(), "🎤 Intro buy six\nline two");
        assert_eq!(qt, m.text());
    }

    #[test]
    fn edits_inside_region_are_reverted() {
        let mut m = EditorModel::new("ab".into(), 0);
        let mut qt = m.text().to_owned();
        m.open_region("r", 2, 0);
        let u = upd(&m, 0, Some((0, 0, " hello")));
        for e in m.apply_region(&u, 0).unwrap() {
            apply_qt(&mut qt, &e);
        }
        // Inside: reverted.
        let out = type_at(&mut m, &mut qt, 4, 0, "X", 1);
        assert!(matches!(out, UserEditOutcome::Revert(_)));
        assert_eq!(qt, "ab hello");
        // Deleting a range that overlaps: reverted.
        let out = type_at(&mut m, &mut qt, 1, 3, "", 1);
        assert!(matches!(out, UserEditOutcome::Revert(_)));
        assert_eq!(qt, "ab hello");
        // At the end boundary: allowed, lands after the region.
        assert_eq!(
            type_at(&mut m, &mut qt, 8, 0, "!", 1),
            UserEditOutcome::Accepted
        );
        assert_eq!(m.region_text(), Some(" hello"));
        // New speech continues at the region end, before the typed text.
        let u = upd(&m, 0, Some((6, 6, " world")));
        for e in m.apply_region(&u, 2).unwrap() {
            apply_qt(&mut qt, &e);
        }
        assert_eq!(qt, "ab hello world!");
        assert_eq!(m.text(), qt);
        assert!(m.take_log().iter().any(|r| r.actor == Actor::Blocked));
    }

    #[test]
    fn insertion_at_region_start_goes_before() {
        let mut m = EditorModel::new("".into(), 0);
        let mut qt = String::new();
        m.open_region("r", 0, 0);
        let u = upd(&m, 0, Some((0, 0, "voice")));
        m.apply_region(&u, 0)
            .unwrap()
            .iter()
            .for_each(|e| apply_qt(&mut qt, e));
        assert_eq!(
            type_at(&mut m, &mut qt, 0, 0, "me: ", 1),
            UserEditOutcome::Accepted
        );
        assert_eq!(m.region_text(), Some("voice"));
        assert_eq!(m.region().unwrap().start, 4);
    }

    #[test]
    fn undo_only_affects_human_text_and_survives_voice_changes() {
        let mut m = EditorModel::new("".into(), 0);
        let mut qt = String::new();
        for (i, ch) in ["H", "i", "."].iter().enumerate() {
            type_at(&mut m, &mut qt, i, 0, ch, i as i64 * 100);
        }
        m.open_region("r", 3, 1000);
        let u = upd(&m, 0, Some((0, 0, " Dictated")));
        m.apply_region(&u, 1000)
            .unwrap()
            .iter()
            .for_each(|e| apply_qt(&mut qt, e));
        // Type after the region, then undo it: only the typed text goes.
        let end = utf16_len(&qt);
        type_at(&mut m, &mut qt, end, 0, " typed", 5000);
        let u = upd(&m, 0, Some((9, 9, " more")));
        m.apply_region(&u, 5100)
            .unwrap()
            .iter()
            .for_each(|e| apply_qt(&mut qt, e));
        assert_eq!(qt, "Hi. Dictated more typed");
        for e in m.undo(6000).unwrap() {
            apply_qt(&mut qt, &e);
        }
        assert_eq!(qt, "Hi. Dictated more");
        for e in m.undo(6000).unwrap() {
            apply_qt(&mut qt, &e);
        }
        assert_eq!(qt, " Dictated more", "typing grouped as one undo step");
        assert_eq!(m.text(), qt);
        assert_eq!(m.undo(6000), Err(UndoError::Empty));
        for e in m.redo(7000).unwrap() {
            apply_qt(&mut qt, &e);
        }
        assert_eq!(qt, "Hi. Dictated more");
        assert_eq!(m.text(), qt);
    }

    /// Review finding: an entry typed after the region, then one typed before it, then voice
    /// extending the region: the first entry used to be dropped.
    #[test]
    fn undo_survives_edits_on_both_sides_of_a_growing_region() {
        let mut m = EditorModel::new("Hello".into(), 0);
        let mut qt = m.text().to_owned();
        m.open_region("r", 5, 0);
        m.apply_region(&upd(&m, 0, Some((0, 0, " voice"))), 0)
            .unwrap()
            .iter()
            .for_each(|e| apply_qt(&mut qt, e));
        let end = utf16_len(&qt);
        type_at(&mut m, &mut qt, end, 0, "!", 1000);
        type_at(&mut m, &mut qt, 0, 0, ">", 5000);
        let len = m.region().unwrap().end - m.region().unwrap().start;
        m.apply_region(&upd(&m, 0, Some((len, len, " more"))), 6000)
            .unwrap()
            .iter()
            .for_each(|e| apply_qt(&mut qt, e));
        assert_eq!(qt, ">Hello voice more!");
        for e in m.undo(7000).unwrap() {
            apply_qt(&mut qt, &e);
        }
        assert_eq!(qt, "Hello voice more!");
        for e in m.undo(7000).unwrap() {
            apply_qt(&mut qt, &e);
        }
        assert_eq!(qt, "Hello voice more");
        assert_eq!(m.text(), qt);
        for e in m.redo(8000).unwrap() {
            apply_qt(&mut qt, &e);
        }
        for e in m.redo(8000).unwrap() {
            apply_qt(&mut qt, &e);
        }
        assert_eq!(qt, ">Hello voice more!");
    }

    /// Random human edits on both sides of the region, interleaved with voice updates and
    /// partial undo/redo. Undoing everything must always succeed and leave exactly the original
    /// text with the dictated text in place.
    #[test]
    fn randomized_undo_with_interleaved_voice_updates() {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n.max(1) as u64) as usize
        };
        for round in 0..200 {
            let mut m = EditorModel::new("0123456789".into(), 0);
            let mut qt = m.text().to_owned();
            m.open_region("r", 5, 0)
                .iter()
                .for_each(|e| apply_qt(&mut qt, e));
            // Receives only the voice updates: what undoing every human edit must leave.
            let mut voice_only = EditorModel::new("0123456789".into(), 0);
            voice_only.open_region("r", 5, 0);
            let mut t = 0i64;
            let mut ops: Vec<String> = Vec::new();
            for _ in 0..25 {
                ops.push(format!(
                    "text {:?} region {:?}",
                    m.text(),
                    m.region().map(|r| (r.start, r.end))
                ));
                t += if next(3) == 0 { 100 } else { 5000 };
                let r = m.region().unwrap().clone();
                match next(6) {
                    0 | 1 => {
                        // Insert outside the region (at either boundary is allowed).
                        let before = next(2) == 0;
                        let mut pos = if before {
                            next(r.start + 1)
                        } else {
                            r.end + next(m.text().len() - r.end + 1)
                        };
                        while !m.text().is_char_boundary(pos) {
                            pos -= 1;
                        }
                        if !before && pos < r.end {
                            pos = r.end;
                        }
                        let ins = ["a", "bc", "é", "😀", " "][next(5)];
                        let p16 = m.to_utf16(pos);
                        let before_text = m.text().to_owned();
                        ops.push(format!("insert {ins:?} at {pos} t={t}"));
                        let out = type_at(&mut m, &mut qt, p16, 0, ins, t);
                        assert_eq!(
                            out,
                            UserEditOutcome::Accepted,
                            "round {round}: insert {ins:?} at byte {pos} (utf16 {p16}) into {before_text:?}, region {r:?}"
                        );
                    }
                    2 => {
                        // Delete a range entirely on one side.
                        let (lo, hi) = if next(2) == 0 {
                            (0, r.start)
                        } else {
                            (r.end, m.text().len())
                        };
                        if hi > lo {
                            let a = lo + next(hi - lo);
                            let b = (a + 1 + next(3)).min(hi);
                            let (a16, b16) = (m.to_utf16(a), m.to_utf16(b));
                            if m.from_utf16(a16) == Some(a) && m.from_utf16(b16) == Some(b) {
                                ops.push(format!("delete {a}..{b} t={t}"));
                                assert_eq!(
                                    type_at(&mut m, &mut qt, a16, b16 - a16, "", t),
                                    UserEditOutcome::Accepted
                                );
                            }
                        }
                    }
                    3 | 4 => {
                        // Voice: append, or rewrite the region's text.
                        let len = r.end - r.start;
                        let u = match next(3) {
                            0 if len > 0 => {
                                upd(&m, 0, Some((0, len, ["Hi.", "x y", "ok"][next(3)])))
                            }
                            // Everything leaves the region (as when sentences are protected).
                            1 if len > 0 => upd(&m, len, None),
                            _ => upd(&m, 0, Some((len, len, [" w", "z", " é"][next(3)]))),
                        };
                        voice_only.apply_region(&u, t).unwrap();
                        ops.push(format!("voice {:?}", u.edit));
                        m.apply_region(&u, t)
                            .unwrap()
                            .iter()
                            .for_each(|e| apply_qt(&mut qt, e));
                    }
                    _ => {
                        let undo = next(2) == 0;
                        ops.push(if undo { "undo".into() } else { "redo".into() });
                        let res = if undo { m.undo(t) } else { m.redo(t) };
                        match res {
                            Ok(edits) => edits.iter().for_each(|e| apply_qt(&mut qt, e)),
                            Err(UndoError::Empty) => {}
                            Err(e) => panic!("round {round}: {e}"),
                        }
                    }
                }
                assert_eq!(m.text(), qt, "round {round}: model and editor agree");
            }
            let stack = format!("{:#?}", m.undo);
            loop {
                match m.undo(t) {
                    Ok(edits) => edits.iter().for_each(|e| apply_qt(&mut qt, e)),
                    Err(UndoError::Empty) => break,
                    Err(e) => panic!(
                        "round {round}: undoing everything failed: {e}\n{}\nfinal {:?}\nstack {stack}",
                        ops.join("\n"),
                        m.text()
                    ),
                }
            }
            assert_eq!(m.text(), voice_only.text(), "round {round}");
        }
    }

    #[test]
    fn undo_conflict_is_refused_without_changes() {
        let mut m = EditorModel::new("".into(), 0);
        let mut qt = String::new();
        type_at(&mut m, &mut qt, 0, 0, "abc", 0);
        type_at(&mut m, &mut qt, 0, 0, "x", 5000);
        // Desync the stack artificially: replace the text the first entry expects.
        m.undo.last_mut().unwrap().entries[0].inserted = "zz".into();
        let before = m.text().to_owned();
        assert_eq!(m.undo(6000), Err(UndoError::Conflict));
        assert_eq!(m.text(), before);
    }

    #[test]
    fn release_and_close_record_provenance() {
        let mut m = EditorModel::new("".into(), 0);
        m.open_region("r", 0, 0);
        let u = upd(&m, 0, Some((0, 0, "One. Two")));
        m.apply_region(&u, 0).unwrap();
        let u = upd(&m, 5, None);
        m.apply_region(&u, 0).unwrap();
        assert_eq!(m.region_text(), Some("Two"));
        assert_eq!(m.recordings_in(0, 3), vec!["r".to_owned()]);
        assert!(m.close_region("r"));
        assert_eq!(m.provenance().len(), 1);
        assert_eq!(m.provenance()[0].end, 8);
        // Editing inside released text splits provenance.
        let mut qt = m.text().to_owned();
        type_at(&mut m, &mut qt, 4, 0, " and", 0);
        assert_eq!(m.provenance().len(), 2);
        assert!(m.recordings_in(5, 7).is_empty());
    }

    #[test]
    fn stale_or_out_of_bounds_updates_are_rejected() {
        let mut m = EditorModel::new("".into(), 0);
        m.open_region("r", 0, 0);
        let mut u = upd(&m, 0, Some((0, 0, "x")));
        u.revision = 5;
        assert!(matches!(
            m.apply_region(&u, 0),
            Err(RegionError::Revision { .. })
        ));
        let u = upd(&m, 0, Some((0, 3, "x")));
        assert_eq!(m.apply_region(&u, 0), Err(RegionError::Bounds));
        let mut u = upd(&m, 0, Some((0, 0, "x")));
        u.recording_id = "other".into();
        assert!(matches!(
            m.apply_region(&u, 0),
            Err(RegionError::NoRegion(_))
        ));
    }

    #[test]
    fn exact_placement_mid_line_adds_trailing_space() {
        let mut m = EditorModel::new("ab cd".into(), 0);
        let edits = m.open_region("r", 3, 0);
        assert_eq!(edits.len(), 1);
        assert_eq!(m.text(), "ab  cd");
        assert_eq!(m.region().unwrap().start, 3);
        assert!(!m.needs_leading_space(3));
    }

    #[test]
    fn key_prefilter() {
        let mut m = EditorModel::new("ab".into(), 0);
        m.open_region("r", 2, 0);
        m.apply_region(&upd(&m, 0, Some((0, 0, " cd"))), 0).unwrap();
        assert!(!m.edit_would_touch_region("insert", 2, 2, 2));
        assert!(m.edit_would_touch_region("insert", 3, 3, 3));
        assert!(m.edit_would_touch_region("backspace", 5, 5, 5));
        assert!(!m.edit_would_touch_region("backspace", 2, 2, 2));
        assert!(m.edit_would_touch_region("delete", 2, 2, 2));
        assert!(!m.edit_would_touch_region("delete", 5, 5, 5));
        assert!(m.edit_would_touch_region("insert", 1, 1, 4));
    }

    #[test]
    fn formatting_only_notifications_are_noops() {
        let mut m = EditorModel::new("hello".into(), 0);
        assert_eq!(m.user_edit(0, 5, "hello", 5, 0), UserEditOutcome::NoOp);
        assert_eq!(m.user_edit(0, 1, "x", 9, 0), UserEditOutcome::Desync);
        assert_eq!(m.resync("hello!", 0), UserEditOutcome::Accepted);
        assert_eq!(m.text(), "hello!");
    }
}
