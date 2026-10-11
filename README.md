# Batscribe

A Linux desktop dictation editor. Speech appears in the editor as you talk, and a language model
revises the most recent text when you correct yourself out loud ("buy seven oranges, no actually
six" becomes "buy six oranges"). Every recognition and editing step is kept in a local history
you can inspect.

Built with Rust and Qt 6 Quick/QML through [CXX-Qt](https://kdab.github.io/cxx-qt/book/). It uses
Deepgram for streaming speech recognition and an OpenAI model for text editing, called directly
with your own API keys (Anthropic's Messages API is also available for editing). It targets Fedora 44 on Wayland.

**Status:** MVP implemented (all four steps in [`plans/handoff.md`](plans/handoff.md)). Tested
with mock providers, a local WebSocket server and the real microphone; not yet run against the
live Deepgram and OpenAI services (see [Live check](#live-check)).

## How it works

- Press **Start** (or Ctrl+Shift+Space). Dictation goes to the end of the caret's line, in a
  shaded *voice region*: light shading is text that can still be revised, stronger shading
  (italic) is the provisional hypothesis. The orange bar is the voice caret.
- The voice region is read-only to you. Type anywhere else, including before it, while you
  dictate. Older sentences leave the region and become ordinary text, as does everything when
  you press **Stop**.
- Say a correction ("…seven oranges, no actually six oranges") and the region is revised.
- **Undo** affects only what you typed, never dictation.
- **Copy** puts the whole document on the clipboard. **Save As…** writes it to a text file and
  remembers the file, which **Save** (Ctrl+S) then updates. Documents are also kept in the
  application's database automatically.
- **Details** shows the history: recordings, every provider message and model call, edits,
  settings snapshots and latency measurements. **Sources** shows the recording behind the
  selected text.
- **Preferences** has API keys (kept in the system keyring or for the session only) and every
  tunable setting, with help, units, validation and reset.

## Requirements (Fedora 44)

```sh
sudo dnf install gcc-c++ pkgconf-pkg-config openssl-devel alsa-lib-devel pipewire-devel clang mold \
    qt6-qtbase-devel qt6-qtdeclarative-devel qt6-qtwayland
```

The Rust toolchain is pinned to 1.98.1 in `rust-toolchain.toml`. CXX-Qt 0.10.0, tested against
Qt 6.11.2. Audio uses `cpal`'s native PipeWire backend. Linking uses `mold` (set in
`.cargo/config.toml`), since Qt can't be linked with GNU ld.bfd.

## Build and run

```sh
cargo run -p batscribe-desktop --release
```

Data lives in `$XDG_DATA_HOME/batscribe` (`~/.local/share/batscribe`): `batscribe.sqlite3` holds
documents, preferences and history; retained audio (off by default) goes to `audio/` or the
folder chosen in Preferences.

To try it without a microphone or API keys:

```sh
BATSCRIBE_DEV_PROVIDERS=1 BATSCRIBE_CREDENTIAL_STORE=memory cargo run -p batscribe-desktop
```

then choose *Scripted mock*, *Rule-based mock* and *Synthetic silence* in Preferences.

## Tests

```sh
cargo test --workspace                                  # no network, no audio device
QT_QPA_PLATFORM=offscreen scripts/ui-test.sh editor     # UI test of the editor
QT_QPA_PLATFORM=offscreen scripts/ui-test.sh mic        # needs a microphone; no network
```

See `AGENTS.md` for the multi-launch persistence test.

## Live check

This makes paid API calls. It records from the default microphone for 20 seconds, streams to
Deepgram, applies corrections with the editing model, and prints the text and measured
latencies:

```sh
DEEPGRAM_API_KEY=... OPENAI_API_KEY=... cargo run -p batscribe-core --release --example live_session -- 20
```

To edit with Anthropic instead (default model `claude-haiku-5-5`; `BATSCRIBE_COMP_MODEL` picks
another, and `BATSCRIBE_ANTHROPIC_THINKING=off` turns thinking off):

```sh
DEEPGRAM_API_KEY=... ANTHROPIC_API_KEY=... BATSCRIBE_COMP_PROVIDER=anthropic \
    cargo run -p batscribe-core --release --example live_session -- 20
```

`cargo run -p batscribe-core --example mic_check` lists input devices and tests capture without
any network access.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT license](LICENSE-MIT)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
this project by you, as defined in the Apache-2.0 license, shall be dual licensed as above,
without any additional terms or conditions.
