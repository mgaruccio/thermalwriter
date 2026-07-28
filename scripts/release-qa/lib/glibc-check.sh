#!/usr/bin/env bash
# GLIBC symbol ceiling checks for release binaries.
# shellcheck shell=bash

qa_require_objdump() {
    command -v objdump >/dev/null 2>&1 || {
        printf 'error: objdump required for GLIBC checks\n' >&2
        return 1
    }
}

# Print the highest GLIBC_x.y version referenced by a dynamic binary (e.g. 2.35).
qa_max_glibc_version() {
    local bin="$1"
    qa_require_objdump || return 1
    [[ -f "$bin" ]] || {
        printf 'error: binary not found: %s\n' "$bin" >&2
        return 1
    }
    objdump -T "$bin" 2>/dev/null \
        | sed -n 's/.*\(GLIBC_[0-9][0-9.]*\).*/\1/p' \
        | sed 's/GLIBC_//' \
        | sort -V \
        | tail -1
}

# Return 0 when the binary's highest GLIBC requirement is <= max_allowed (e.g. 2.35).
qa_assert_glibc_max() {
    local bin="$1"
    local max_allowed="${2:-2.35}"
    local observed

    observed="$(qa_max_glibc_version "$bin" || true)"
    if [[ -z "$observed" ]]; then
        printf 'FAIL  %s: could not determine GLIBC requirement\n' "$bin" >&2
        return 1
    fi
    if [[ "$(printf '%s\n' "$observed" "$max_allowed" | sort -V | tail -1)" == "$max_allowed" ]]; then
        printf 'PASS  %s GLIBC <= %s (max %s)\n' "$(basename "$bin")" "$max_allowed" "$observed"
        return 0
    fi
    printf 'FAIL  %s requires GLIBC_%s (> %s)\n' "$bin" "$observed" "$max_allowed" >&2
    return 1
}

# Return 0 when release GUI artifacts do not embed MCP bridge strings.
qa_assert_no_mcp_bridge_strings() {
    local bin="$1"
    if [[ ! -f "$bin" ]]; then
        printf 'FAIL  MCP check: binary not found: %s\n' "$bin" >&2
        return 1
    fi
    if strings "$bin" | grep -qE 'plugin:mcp-bridge|mcp-bridge:default'; then
        printf 'FAIL  %s contains MCP bridge strings\n' "$bin" >&2
        return 1
    fi
    printf 'PASS  %s has no MCP bridge strings\n' "$(basename "$bin")"
    return 0
}
