#!/usr/bin/env bash
# Shared helpers for thermalwriter release QA (host + guest).
# shellcheck shell=bash

set -euo pipefail

qa_root() {
    local here
    here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    cd "$here/.." && pwd
}

qa_log() { printf '==> %s\n' "$*"; }
qa_info() { printf '    %s\n' "$*"; }
qa_die() { printf 'error: %s\n' "$*" >&2; exit 1; }

# Normalize a release ref to tag + bare version.
# Accepts: v0.1.1 | 0.1.1
qa_parse_version() {
    local raw="${1:-}"
    [[ -n "$raw" ]] || qa_die "version/tag required (e.g. v0.1.1)"
    local tag version
    if [[ "$raw" == v* ]]; then
        tag="$raw"
        version="${raw#v}"
    else
        version="$raw"
        tag="v${raw}"
    fi
    QA_TAG="$tag"
    QA_VERSION="$version"
}

qa_default_artifacts_dir() {
    local tag="${1:-${QA_TAG:-}}"
    [[ -n "$tag" ]] || qa_die "qa_default_artifacts_dir: tag required"
    local base="${THERMALWRITER_QA_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/thermalwriter-qa}"
    printf '%s\n' "$base/artifacts/$tag"
}

qa_default_out_dir() {
    local tag="${1:-${QA_TAG:-}}"
    [[ -n "$tag" ]] || qa_die "qa_default_out_dir: tag required"
    local root
    root="$(qa_root)"
    printf '%s\n' "$root/out/$tag"
}

qa_default_vm_dir() {
    local base="${THERMALWRITER_QA_VM_DIR:-$HOME/vms/thermalwriter-qa}"
    printf '%s\n' "$base"
}

qa_require_cmd() {
    local c
    for c in "$@"; do
        command -v "$c" >/dev/null 2>&1 || qa_die "required command not found: $c"
    done
}

# Resolve artifact paths for a downloaded release dir.
qa_artifact_paths() {
    local dir="${1:-}"
    local tag="${2:-${QA_TAG:-}}"
    local version="${3:-${QA_VERSION:-}}"
    [[ -d "$dir" ]] || qa_die "artifacts dir not found: $dir"
    [[ -n "$tag" && -n "$version" ]] || qa_die "tag/version required"

    QA_SHA256SUMS="$dir/SHA256SUMS"
    QA_TARBALL="$dir/thermalwriter-${tag}-x86_64-unknown-linux-gnu.tar.gz"
    QA_DEB="$dir/thermalwriter-config_${version}_amd64.deb"
    QA_APPIMAGE="$dir/Thermalwriter-Config_${version}_amd64.AppImage"
}

qa_pass() {
    printf 'PASS  %s\n' "$*"
}

qa_fail() {
    printf 'FAIL  %s\n' "$*" >&2
    return 1
}
