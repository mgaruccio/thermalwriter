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
    if grep -aEq 'plugin:mcp-bridge|mcp-bridge:default' "$bin"; then
        printf 'FAIL  %s contains MCP bridge strings\n' "$bin" >&2
        return 1
    fi
    printf 'PASS  %s has no MCP bridge strings\n' "$(basename "$bin")"
    return 0
}

# Extract a .deb to out_dir; prints nothing on success.
qa_extract_deb() {
    local deb="$1"
    local out_dir="$2"
    deb="$(readlink -f "$deb")"
    rm -rf "$out_dir"
    mkdir -p "$out_dir"
    if command -v dpkg-deb >/dev/null 2>&1; then
        dpkg-deb -x "$deb" "$out_dir"
        return 0
    fi
    if command -v ar >/dev/null 2>&1 && command -v tar >/dev/null 2>&1; then
        local work
        work="$(mktemp -d)"
        (cd "$work" && ar x "$deb" && tar -xf data.tar.* -C "$out_dir")
        rm -rf "$work"
        return 0
    fi
    printf 'error: need dpkg-deb or ar+tar to extract .deb\n' >&2
    return 1
}

# Find thermalwriter-gui inside an extracted deb tree.
qa_find_gui_binary() {
    local root="$1"
    find "$root" -type f \( -name thermalwriter-gui -o -name 'Thermalwriter Config' \) -perm -111 2>/dev/null | head -1
}

# Assert bundled GUI resources include license notices (AppImage squashfs or deb tree).
qa_assert_gui_bundle_notices() {
    local root="$1"
    local label="${2:-GUI bundle}"
    local missing=0
    local rel

    for rel in \
        THIRD_PARTY_NOTICES.md \
        DejaVu-LICENSE.txt \
        OFL-IBMPlexMono.txt \
        OFL-IBMPlexSans.txt \
        OFL-MajorMonoDisplay.txt; do
        if find "$root" -type f -name "$rel" 2>/dev/null | grep -q .; then
            printf 'PASS  %s contains %s\n' "$label" "$rel"
        else
            printf 'FAIL  %s missing %s\n' "$label" "$rel" >&2
            missing=1
        fi
    done
    return "$missing"
}
