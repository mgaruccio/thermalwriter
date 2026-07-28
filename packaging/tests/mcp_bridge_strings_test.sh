#!/usr/bin/env bash
# Shell tests for qa_assert_no_mcp_bridge_strings (pipefail-safe grep on binaries).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=../release-qa/lib/glibc-check.sh
source "$ROOT/../scripts/release-qa/lib/glibc-check.sh"

pass=0
fail=0
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

assert_pass() {
    local desc="$1"
    shift
    if "$@"; then
        printf 'PASS  %s\n' "$desc"
        pass=$((pass + 1))
    else
        printf 'FAIL  %s\n' "$desc" >&2
        fail=$((fail + 1))
    fi
}

assert_fail() {
    local desc="$1"
    shift
    if "$@"; then
        printf 'FAIL  %s (expected failure)\n' "$desc" >&2
        fail=$((fail + 1))
    else
        printf 'PASS  %s\n' "$desc"
        pass=$((pass + 1))
    fi
}

CLEAN_BIN="$TMP/clean.bin"
printf 'thermalwriter gui release artifact\n' >"$CLEAN_BIN"

DIRTY_BIN="$TMP/dirty.bin"
printf 'some header plugin:mcp-bridge tail\n' >"$DIRTY_BIN"

assert_pass "clean binary has no MCP strings" qa_assert_no_mcp_bridge_strings "$CLEAN_BIN"
assert_fail "dirty binary fails MCP check" qa_assert_no_mcp_bridge_strings "$DIRTY_BIN"

if [[ "$fail" -ne 0 ]]; then
    printf 'RESULT: FAIL (%d pass, %d fail)\n' "$pass" "$fail" >&2
    exit 1
fi
printf 'RESULT: PASS (%d tests)\n' "$pass"
