#!/usr/bin/env bash
# Shell tests for tray install mode helpers (no live systemd mutation).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=../lib/tray-install.sh
source "$ROOT/lib/tray-install.sh"

pass=0
fail=0

assert_eq() {
    local desc="$1" expected="$2" actual="$3"
    if [[ "$expected" == "$actual" ]]; then
        printf 'PASS  %s\n' "$desc"
        pass=$((pass + 1))
    else
        printf 'FAIL  %s (expected %s, got %s)\n' "$desc" "$expected" "$actual" >&2
        fail=$((fail + 1))
    fi
}

TMP_HOME="$(mktemp -d)"
trap 'rm -rf "$TMP_HOME"' EXIT
export HOME="$TMP_HOME"
export CARGO_HOME="$TMP_HOME/.cargo"
export PATH="/usr/bin:/bin"
mkdir -p "$CARGO_HOME/bin" "$HOME/Downloads"

assert_eq "auto without GUI skips tray" "0" "$(tw_resolve_install_tray auto)"
assert_eq "explicit 0 skips tray" "0" "$(tw_resolve_install_tray 0)"
assert_eq "explicit 1 forces tray" "1" "$(tw_resolve_install_tray 1)"

install -m0755 /bin/echo "$CARGO_HOME/bin/thermalwriter-gui"
assert_eq "auto with thermalwriter-gui installs tray" "1" "$(tw_resolve_install_tray auto)"

rm -f "$CARGO_HOME/bin/thermalwriter-gui"
printf '#!/bin/sh\n' >"$HOME/Downloads/Thermalwriter-Config_test.AppImage"
chmod +x "$HOME/Downloads/Thermalwriter-Config_test.AppImage"
assert_eq "auto with AppImage in Downloads installs tray" "1" "$(tw_resolve_install_tray auto)"

if [[ "$fail" -ne 0 ]]; then
    printf 'RESULT: FAIL (%d pass, %d fail)\n' "$pass" "$fail" >&2
    exit 1
fi
printf 'RESULT: PASS (%d tests)\n' "$pass"
