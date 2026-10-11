# Batscribe future work

Everything here is beyond the MVP defined in [handoff.md](handoff.md). These are candidates,
not implementation commitments. [decisions.md](decisions.md) preserves the rationale and
dated provider research.

## Extended evaluation and tuning

After the four MVP implementation steps work together:

- Run longer sessions and broader stress/fault-injection testing of recovery, concurrent
  typing, audio references, persistence failure, and large-history inspection. Basic
  correctness for these features remains part of MVP validation.
- Compare Luna and Haiku using identical recordings and editing inputs (both adapters exist;
  switch with Preferences → Composition → Provider, or `BATSCRIBE_COMP_PROVIDER` in
  `live_session`). Measure full-response
  latency, correction fidelity, unintended deletion/invention, and usage separately from
  raw recognition accuracy. Historical comparison candidate: Haiku 5.5; verify availability
  when implementing. [Haiku documentation](https://platform.claude.com/docs/en/models/haiku-5-5/overview)
- Tune endpointing, request cadence, debounce, and correction/context windows through the
  preferences UI. Compare minimally processed and smart-formatted ASR on names, dates,
  and numbers. Evaluate Luna reasoning settings where quality justifies additional latency.
- Measure median/p95 first-text delay, correction delay, sustained processing backlog,
  memory/CPU, and cost per audio minute. Measure from source audio as well as provider-event
  arrival to expose endpointing delay.
- Consider Flux or another online recognizer if ASR quality or turn boundaries are limiting.
  Add actual provider adapters only when useful; MVP interfaces already separate ASR/editing.

## Local recognition

Add one local recognizer behind the same ASR interface, retaining online recognition as a
quality/latency reference. Specify the intended CPU/RAM baseline before promising performance.
Local recognition does not require local text-model editing.

Candidates researched on 2026-10-10:

- `sherpa-onnx` with a streaming Zipformer model: incremental recognition with Rust API
  support. [API overview](https://k2-fsa.github.io/sherpa/intro.html),
  [streaming models](https://k2-fsa.github.io/sherpa/onnx/pretrained_models/online-transducer/zipformer-transducer-models.html).
- `whisper.cpp` with a small quantized English model: CPU-capable with a microphone example
  based on repeated window decoding. Production use needs overlap reconciliation and
  stabilization. [Project](https://github.com/ggml-org/whisper.cpp),
  [streaming example](https://github.com/ggml-org/whisper.cpp/blob/master/examples/stream/README.md).

Benchmark the actual streaming loop. Decoding a five-second window every second requires
each decode comfortably below one second; one-shot real-time factor below one is insufficient.
Measure sustained latency, accuracy, CPU, and memory on commodity hardware. Run inference
on a dedicated worker or child process, never the GUI or network executor thread.
Local language-model editing is a separate, later experiment with its own performance budget.

## Editor and voice-region extensions

- Editing inside the voice region. MVP makes the region read-only. Options include claiming
  the edited span as human text and splitting the region around it, with source-consumption
  records so later hypotheses cannot resurrect deleted words.
- Splitting the region when the user types at its trailing boundary, so speech continues
  after the typed text instead of before it.
- Releasing or discarding the current region's text on request without stopping.
- Reverting voice or model changes, for example from the inspector. Ctrl+Z covers only
  user-entered text in MVP. Reverted text should be protected from being changed again by
  the same evidence.

## Audio extensions

- Playback of a selected text's audio span from the inspector, and relinking a recording to
  an audio file that was moved. MVP records audio state and paths only.
- Retaining device-rate audio alongside, or instead of, the transmitted 16 kHz stream if
  re-recognition experiments need higher fidelity.

## Found during the MVP build

- Hot-plug awareness: refresh the device list and recording state when a microphone appears or
  disappears mid-recording (today capture errors are reported and the recording continues or
  finishes with the text so far).
- Keep a per-document undo stack across document switches and restarts (it is in memory and
  per session today).
- Export a document (plain-text file, clipboard of the whole document) and search across
  documents.
- A desktop entry and icon, and packaging for the target system.

## Recognition and composition extensions

- Speculative editing of stable interim prefixes, only if finalized-only composition feels
  too slow. Track source revisions and invalidate results when ASR changes the prefix.
- Selective re-recognition of recent retained audio with additional context or vocabulary
  hints. Text-only editing cannot repair missing acoustic evidence reliably.
- Silence-transmission optimization with measured endpoint behavior, pre-roll, and explicit
  stream lifecycle handling; do not clip speech boundaries.
- Suggested edits for corrections outside the automatic region, selection-based editing,
  and explicit spoken command/formatting modes. Decide how literal command-like speech is
  distinguished from instructions before expanding automatic editing authority.
- Mid-recording model/prompt/timing changes, if needed. Record a new effective configuration
  boundary and associate each operation with its actual settings; never silently mutate
  the configuration of an existing recording.

## Broader product and platform scope

- Languages beyond English, with recognition and correction evaluation per language.
- Rich text and structured document editing; reconsider the UI framework only if a concrete
  editor requirement warrants it, not as a prerequisite for the current application.
- Other Linux distributions, X11, macOS, Windows, portable packaging, and wider CI coverage.
  MVP targets the current Fedora 44 / Wayland system only. Inexpensive compatibility is
  welcome; none of these targets should delay MVP.
- A managed service/backend if distributing shared provider access becomes a requirement.
  MVP uses the user's own credentials and connects directly.

Tune the initial correction reach and caret-placement defaults from use rather than treating
them as permanent choices. Model/protocol availability and pricing must be checked again
when any deferred integration is undertaken.
