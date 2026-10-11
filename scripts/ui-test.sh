#!/usr/bin/env bash
# Runs a QML UI test against the debug build with throwaway data directories and an in-memory
# key store. Never touches real data, the system keyring, or paid APIs.
#
#   scripts/ui-test.sh [test-name] [extra app args]   (default test: editor)
#
# Set QT_QPA_PLATFORM=offscreen to run without showing a window.
set -euo pipefail
cd "$(dirname "$0")/.."

name="${1:-editor}"
shift || true
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
# UI_TEST_DATA keeps data between runs (multi-phase tests).
data="${UI_TEST_DATA:-$tmp}"

cargo build -q -p batscribe-desktop
env XDG_CONFIG_HOME="$data/config" XDG_DATA_HOME="$data/data" \
  BATSCRIBE_CREDENTIAL_STORE=memory BATSCRIBE_DEV_PROVIDERS=1 QT_FORCE_STDERR_LOGGING=1 \
  QML_XHR_ALLOW_FILE_READ=1 UI_TEST_TMP="$tmp" \
  target/debug/batscribe --qml-script="$PWD/crates/batscribe-desktop/tests/ui/$name.qml" "$@" \
  >"$tmp/app.log" 2>&1 && status=0 || status=$?
grep -v "^qt.qpa" "$tmp/app.log" || true
exit "$status"
