# Agent notes

Linux dictation editor: Rust + Qt 6 Quick/QML via CXX-Qt, with streaming speech recognition and
model-based correction of recent text.

## Plans

- `plans/handoff.md`: MVP requirements and implementation steps. Start here.
- `plans/decisions.md`: why the design is the way it is, plus dated provider research.
- `plans/future.md`: deferred ideas. None of it is an MVP requirement.

## Layout

- `crates/batscribe-core`: no Qt. `text` (UTF-8/UTF-16 positions), `editor` (the GUI-thread
  editor model: voice region, reverts, user-only undo, provenance), `region` (the core's copy of
  a voice region, which emits `RegionUpdate`s), `engine` (one session per recording: audio,
  recognition with reconnect/replay, edit scheduling, lifecycle, history), `audio` (cpal capture,
  resampling, WAV), `asr` (Deepgram and a scripted mock), `composition` (OpenAI Responses and a
  rule-based mock), `storage` (SQLite: reliable document saves, best-effort history), `settings`
  (typed preference schema), `credentials` (Secret Service or memory).
- `crates/batscribe-desktop`: `src/backend.rs` is the only CXX-Qt bridge; `cpp/texthelper.*` is
  the C++ `TextHelper` QML element that bridges the `TextArea`'s `QTextDocument`; `qml/` holds
  the views. A new QML file must also be added to `QML_FILES` in `build.rs`.

## Commands

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
QT_QPA_PLATFORM=offscreen scripts/ui-test.sh editor   # UI test, see crates/batscribe-desktop/tests/ui
QT_QPA_PLATFORM=offscreen scripts/ui-test.sh resize   # dragging the pane handles
QT_QPA_PLATFORM=offscreen scripts/ui-test.sh files --out="$(mktemp -d)"   # copy, save, save as
QT_QPA_PLATFORM=offscreen scripts/ui-test.sh savefail   # failed saves keep text; exit waits
# Closing while finishing (1-2) and "Quit Now" (3-4), each pair sharing a data directory.
d=$(mktemp -d); for p in 1 2; do UI_TEST_DATA=$d QT_QPA_PLATFORM=offscreen scripts/ui-test.sh closing --phase=$p; done
d=$(mktemp -d); for p in 3 4; do UI_TEST_DATA=$d QT_QPA_PLATFORM=offscreen scripts/ui-test.sh closing --phase=$p; done
# A save failure during finishing must keep the dialog open (5), then the correction is saved (2).
d=$(mktemp -d); for p in 5 2; do UI_TEST_DATA=$d QT_QPA_PLATFORM=offscreen scripts/ui-test.sh closing --phase=$p; done
# Multi-launch persistence test: same data directory, three phases.
d=$(mktemp -d); for p in 1 2 3; do UI_TEST_DATA=$d QT_QPA_PLATFORM=offscreen scripts/ui-test.sh persistence --phase=$p; done
```

UI tests run with `BATSCRIBE_DEV_PROVIDERS=1`, which offers the scripted mock recognizer, the
rule-based mock editor and synthetic audio in Preferences. Use the same variable to try the app
without a microphone or API keys. Other test variables: `BATSCRIBE_CREDENTIAL_STORE=memory`,
`BATSCRIBE_MOCK_SCRIPT` (utterances separated by `|`), `BATSCRIBE_MOCK_DISCONNECT_AT` (seconds),
`BATSCRIBE_MOCK_EDIT_MS`, `BATSCRIBE_DEEPGRAM_URL`, `BATSCRIBE_OPENAI_URL`.

## Rules

- **Never spend API credits** in routine tests. Use deterministic replay and mock services.
  Run live Deepgram or OpenAI calls only when the user asks.
- Never log or persist API keys or authorization headers. Keys live only in Secret Service or
  memory.
- Keep Qt out of the core crate. Logic belongs in Qt-free Rust where it can be unit tested.
- Target only Fedora 44 / Wayland for now. Don't add portability layers that cost real time.
- QML handlers that call into `Backend` from `TextArea` signals must ignore notifications caused
  by programmatic changes (`helper.applying`, `programmaticCaret`); otherwise a Rust invokable
  can be re-entered while it holds `&mut self`.
- Pinned versions: Rust 1.98.1 (`rust-toolchain.toml`), CXX-Qt `=0.10.0`, Qt 6.11. Ask before
  changing them.
