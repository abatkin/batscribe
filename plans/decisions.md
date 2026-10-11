# Batscribe decisions and research notes

Companion to [handoff.md](handoff.md). These notes explain the selected design; they do not
expand MVP scope. Provider facts and estimates below were researched on 2026-10-10 and are
not benchmarks. Recheck them before relying on prices or changing integrations.

## Current-system scope and UI

MVP targets only Fedora 44 on Wayland. Cross-platform work and
additional packaging/testing targets would delay learning whether the dictation experience
works. Keep inexpensive compatibility, but do not build speculative portability abstractions.

Use Rust with Qt Quick/QML and CXX-Qt: a Qt-independent Rust core, a thin bridge, GUI updates
queued onto the Qt thread, Secret Service credentials, and a documents/editor/details layout.
This is the most direct path to a native desktop experience on the target system.
[CXX-Qt documentation](https://kdab.github.io/cxx-qt/book/)

Tauri remains a possible reconsideration if a sophisticated web editor becomes central.
It uses a Rust backend and webview frontend, with WebKitGTK on Linux; it does not inherently
improve the voice pipeline. No Tauri prototype is required for MVP.
[Architecture](https://v2.tauri.app/concept/architecture/),
[process model](https://v2.tauri.app/concept/process-model/)

## Separate recognition, composition, and ownership

Recognition answers what was said; composition decides intended prose; the document engine
decides what automation may change. Separate stages provide immediate feedback without
waiting for model editing and allow either provider to be replaced independently.

Deepgram distinguishes interim results, final audio segments, and speech boundaries. An
ASR-final segment cannot mean immutable document text: “buy seven oranges” may be followed
by “no, actually six.” Preserve a recent revisable region across pauses.
[Deepgram event semantics](https://developers.deepgram.com/docs/understand-endpointing-interim-results)

Keep original transcript revisions rather than feeding only previous composed text back to
the editor. Otherwise a model's earlier mistakes can become the only remaining evidence.
Read-only context and a small editable region allow contextual corrections without letting
new speech rewrite the entire document. Initial time/word limits are tunable defaults.

Text-only editing cannot recover missing acoustic evidence reliably, especially proper
names. Re-recognition and vocabulary-assisted recovery are future experiments, not reasons
to let the editor invent plausible words.

## Initial online providers

Nova-3 is the baseline because the application is transcription-first. Flux emphasizes
conversational voice agents and semantic turn detection; compare it later only if boundary
scheduling warrants it. [Deepgram comparison](https://developers.deepgram.com/docs/flux/flux-nova-3-comparison)

GPT-6 Luna supports structured text output but not audio input. Start with
`reasoning.effort=none` for bounded cleanup; `low` is a configurable option to evaluate if
quality requires it. Haiku is a future comparison, not a second MVP adapter.
[Luna documentation](https://developers.openai.com/api/docs/models/gpt-6-luna)

Online recognition removes CPU/model-distribution uncertainty from the first implementation.
Local recognition can later use the same application event contract without also moving
the editing model onto the CPU.

## Complete structured history and optional audio

SQLite history exists for development, debugging, and troubleshooting. It should explain
successful edits and non-edits: stale results, failed requests, scheduling decisions, raw
versus normalized responses, and effective settings. It is diagnostic, not product data:
writes are asynchronous and best-effort, processing never waits for them, and overflow or
store failure leaves a recorded gap rather than slowing dictation. Document text is product
data and is saved reliably, with visible failures.

A document outlives any recording and can contain manual edits without recording. A recording
outlives individual provider connections. This hierarchy avoids conflating reconnects with
new dictation sessions and naturally accommodates long recordings and many operations.

Store human edits alongside voice operations so provenance can be inspected. Correctness
does not depend on replaying history: documents reopen from their saved text, so no
checkpoint or event-sourcing machinery is required. A collaborative-editing CRDT is
unnecessary. The inspector must page events rather than loading a long recording's history
at once.

One optional audio file per recording avoids duplicate per-request clips and lets users
choose where large files live. Recording-relative spans survive later document edits.
Audio retention off deliberately limits acoustic replay, while structured diagnostics remain
complete. File and database writes require separate recovery bookkeeping.

Retained audio is the transmitted 16 kHz mono stream rather than the device-rate capture.
It is a fraction of the size, is adequate for speech, and shares one sample timeline with
provider timestamps and audio spans, so no rate mapping is needed to locate a span in the
file. Higher-fidelity retention for re-recognition experiments is deferred. Span playback
and relinking moved files are also deferred; MVP records audio state and paths only.

## Audio capture

`cpal` 0.18 (June 2026) added a native PipeWire host behind its `pipewire` feature. Earlier
versions reached PipeWire only through ALSA, where the sound server holds the `default`
device and device lists are cluttered. With the native host, `cpal` gives PipeWire behavior
on the target system and keeps a cross-platform path at no extra cost. If its device
selection or capture proves inadequate, `pipewire-rs` can replace it inside the `audio`
module without affecting the rest of the application.

## Two carets and human precedence

The independent voice caret lets a user correct earlier text while dictation continues.
Following the end of the voice region by default keeps the user oriented; explicit
navigation detaches following. An earlier draft distinguished following committed text from
following the live tail, but with a read-only region the user caret can only sit outside it,
so a single follow mode remains.

End-of-logical-line placement is the initial default because starting in the middle of a
line can surprise the user. Exact-caret placement remains configurable; visual wrapping
must not change document semantics.

Stop ends capture immediately, but recognition and composition may still be finishing.
Keeping the voice region until finishing completes allows those results to land correctly
after the visible voice caret disappears. One recording at a time, including finalization, avoids overlapping
recording lifecycles in MVP.

## Rust-owned voice region

Human edits and automatic edits never touch the same text. The voice region (revisable text
plus the provisional tail) is owned by the core and read-only to the user; everything else
belongs to the user and is never changed automatically. This removes the hardest problems
of the earlier design, in which humans could edit inside the region:

- Model responses never need rebasing against concurrent human edits, because only the core
  changes region contents. Region-relative commands with a region revision suffice.
- Deleted dictation cannot be resurrected, because text leaves the region before the user
  can edit it and its source is marked consumed.
- Human edits can be reported to the core lazily, in batches, since they cannot affect
  anything the core is computing except read-only context.

The cost is that the user cannot fix recent dictation while it is still revisable. They can
wait for it to leave the region, or Stop. Editing inside the region is a future option.

The GUI-thread editor bridge is the single point where mutations are applied in order. The
core owns region contents; the editor owns the live text and its ordering. A read-only span
inside a Qt Quick text editor is not a built-in feature, so it is the first implementation
step, with a separate non-editable dictation area as the fallback.

Outcome (step 1): the in-editor region works, so the fallback was not needed. A moc'd C++
`TextHelper` attaches to the `TextArea`'s `QTextDocument`, reports user changes from
`contentsChange`, and applies programmatic edits without echoing them. A QML key filter blocks
most edits into the region; anything that bypasses it (programmatic inserts, context-menu
paste, input-method commits) is reverted after the fact. Changes requested while a change
notification is being handled are queued, in order, and applied right after it. Scrolling the
editor while following dictation detaches the user caret, which is how "scrolling away
suspends auto-scroll" is implemented; "Follow dictation" resumes.

`QTextDocument`'s own undo stack would record voice changes and interleave them with typing,
and disabling it clears it entirely. A custom user-only undo stack is therefore required.
Ctrl+Z affects only what the user typed; reverting voice or model changes is deferred.

## Small validated replacements and scheduling

Ask the model for replacement text for a known region and compute offsets/diffs in Rust.
This is simpler to validate than model-generated patch coordinates. Apply completed results
atomically so partial model output does not flicker through the editor.

One in-flight request and one coalesced pending snapshot bound latency and cost under
continuous speech. Edit finalized ASR text first; speculative interim editing adds another
source of invalidation and is deferred. Transmit silence initially so endpoint detection
does not depend on a premature client-side silence-cutting policy.

Range edits preserve cursor, selection, scroll, and input-method state better than repeatedly
replacing the whole editor value. Explicit UTF-8/UTF-16 mapping belongs at the UI boundary.
[Qt document editing](https://doc.qt.io/qt-6/qtextcursor.html)

## Preferences and adapter boundaries

Timing, scope, model IDs, and provider options are preferences even when only one provider
is implemented. These values are experimental and must be changeable without code edits.
Immutable effective snapshots preserve reproducibility after defaults change. Pipeline
changes take effect at the next recording to avoid ambiguous mid-session behavior.

Compiled Rust adapters provide replaceability without a dynamic-plugin ABI. Direct provider
connections and user-supplied keys avoid an unnecessary backend. Shared developer credentials
in a distributed product would require a different deployment design.

## Implementation notes (MVP build)

- **Ownership in code.** `EditorModel` (GUI thread, one per open document) applies human edits
  and region updates in order; `VoiceRegion` (inside the engine's per-recording session) is the
  only producer of region contents and emits minimal `RegionUpdate`s with consecutive
  revisions. The editor rejects an update whose revision doesn't follow, so the copies can't
  drift silently.
- **Durability.** Documents are saved from the GUI thread's models after an idle debounce,
  every 3 s while recording (15 s otherwise), on Start/Stop, document switch and close. History
  goes through a queue with a configurable limit; drops are counted and written as gap rows.
  There are no checkpoints: reopening reads the saved text.
- **Confirmed saves and exit.** The writer confirms each document save with its revision. A
  document stays in memory until its latest revision is confirmed, so a failed save can't lose
  text when you switch documents; saves are retried. Closing the window stops a recording,
  saves, and waits in a dialog ("Finishing before closing") until finishing completes and every
  save is confirmed, then quits by itself; "Quit Now" keeps the text on screen and leaves the
  recording to be marked interrupted on the next start.
- **Development providers.** `BATSCRIBE_DEV_PROVIDERS=1` offers a scripted recognizer, a
  rule-based editor and synthetic audio. The scripted recognizer emits Deepgram-format JSON, so
  history and parsing are exercised exactly, and it models one continuous speaker on the
  recording timeline (each connection is told where its audio starts), so reconnect tests can
  tell real duplication from a mock artifact. These providers are what the deterministic replay
  tests use.
- **Reconnects.** A new connection (epoch) replays retained audio from the end of the last final
  result; anything older than the recovery buffer becomes an explicit gap event and notice.
  Messages from an obsolete connection are recorded but never applied.
- **Scheduling.** Finals request an edit after a short debounce when they end speech or contain a
  correction phrase, otherwise at the cadence. Text leaves the region only between requests,
  and only after the model has reviewed it (or declined, failed or been turned off): Deepgram
  finals often hold several sentences, and releasing by sentence count alone once split a
  correction from what it corrected before the model saw either.
- **Superseding for corrections.** A final containing a correction phrase abandons the request in
  flight (which can't include it) and sends a new one at once, instead of waiting up to a full
  request time. The abandoned request finishes in the background; its exchange and usage are
  recorded with status `superseded` and never applied. This relaxes "one request in flight" to
  "one request whose result can apply". Preference: "Prioritize corrections" (on by default).
  A request's response applies to the composed text it saw; text appended since stays as a
  suffix.
- **Measurements.** Each recording stores first-text lag, interim and final lag (from when the
  last recognized word was captured to when its result arrived, so endpointing is included;
  Deepgram's segment end can extend over the trailing silence and would hide it), edit latency and
  correction delay (from the last spoken word of a correction to the applied edit), with median,
  p95 and max, plus edit outcome counts. These are observations, not targets.
- **Devices.** cpal's PipeWire backend lists inputs and output monitors; inputs come first and
  monitors are labeled. A chosen device that is unplugged stays selected and Start reports it,
  instead of silently recording from another microphone.

- **Anthropic editing adapter.** Added as a second composition provider for the latency
  comparison (raw HTTP to `/v1/messages`; there is no official Rust SDK). It uses structured
  output (`output_config.format`) and keeps thinking adaptive with `effort` (default `low`),
  rather than disabling thinking, so the model can skip thinking on short edits; `max_tokens`
  is at least 4,096 because thinking counts toward it. Default model `claude-haiku-5-5`, the
  comparison named in future.md; model and effort are their own preferences, separate from
  the OpenAI ones. A refusal or truncated response fails the edit and leaves the text as
  spoken (Claude Haiku 5.5 has no server-side refusal fallback).
  First Haiku run: "no change" requests took 1.3–1.5 s with no thinking, but the correction
  itself took 2.6 s with 314 thinking tokens. An "Anthropic thinking" preference (on by
  default) can turn thinking off for latency; models that reject that fail the edit.

## First live run (2026-10-11)

One 20-second `live_session` on the target system (Deepgram Nova-3, `gpt-6-luna` with
`reasoning.effort=none`). Observations, not benchmarks:

- Recognition is fast: first text about 64 ms and median interim lag about 90 ms after the
  segment end (measured before the switch to word-end timing, so final lag of about 120 ms
  excluded roughly 0.5 s of endpointing).
- Edit requests of about 400 input and 25 output tokens took 0.8–2.3 s (median 1.4 s). With the
  debounce, a correction lands around 1.5 s after its final result, the top of the provisional
  0.5–1.5 s objective. Prompt caching doesn't apply below 1,024 tokens; a faster model is the
  likelier lever (see future.md).
- It exposed three defects, since fixed with regression tests: text released before review
  (the spoken correction never reached the model), the model copying released words from the
  raw segments back into its replacement (now rejected: replacements may not add words absent
  from the current text, and the prompt says so), and correction phrases not matching when
  punctuated ("Actually, no.").

Third run (same day, after the fixes): the spoken correction ("I want six apples. Actually,
no. I want seven apples.") was applied, 2.6 s after its last word: about 0.67 s until Deepgram's
final (mostly the 400 ms endpointing), 0.15 s debounce, and 1.8 s for the model. No request was
in flight, so superseding didn't apply. Edit latency was 1.1–1.8 s (median 1.6 s) for requests
of about 400 input tokens. The model is now the dominant cost of a correction. That run also
showed raw segments drifting out of line with the text after a rewrite (fixed: a rewrite
consumes the raw segments it replaced).

## Historical cost estimates

These estimates inform the initial online-first choice; they are not a budget guarantee or
an MVP billing-system requirement. Preserve actual provider usage in the structured history.

Deepgram listed promotional Nova-3 monolingual streaming at $0.0048/minute, approximately
$0.288 per streamed audio hour; regular pricing was $0.0077/minute, approximately $0.462/hour.
Keyterm prompting was an add-on. Transmitted silence, replay, and optional features affect
the total. [Deepgram pricing](https://deepgram.com/pricing)

Luna's published standard short-context rates were $0.10/M input and $0.50/M output tokens.
Haiku 5.5 listed the same rates for prompts up to 100k tokens. Equal rates do not establish
equal correction quality or latency.
[Luna](https://developers.openai.com/api/docs/models/gpt-6-luna),
[Haiku](https://platform.claude.com/docs/en/models/haiku-5-5/overview)

Illustration: 1,800 requests/hour × (1,000 input + 150 output tokens) at those rates costs
about $0.315/hour for editing, or roughly $0.60/hour with promotional ASR. This excludes
extra reasoning, retries, and caching adjustments. Larger context or replacement output
increases cost; unchanged snapshots should not generate requests merely to maintain cadence.

## MVP boundary

The MVP has four steps: editor foundation, persistence and inspector, live recognition, and
live composition. The editor foundation step was added first because the read-only voice
region and user-only undo are the largest technical risk and everything else builds on them.
An earlier draft had a fourth evaluation step and a fifth local-recognition step. The
evaluation step mixed essential correctness checks with comparative research: basic
persistence/recovery and concurrent-edit checks remain attached to the features they
validate, while extended testing, provider comparisons, and tuning move to
[future.md](future.md). Local recognition is entirely deferred. No accuracy or latency claims
should precede measurements.
