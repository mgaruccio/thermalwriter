#!/usr/bin/env bash
# Host: push thermalwriter-tray into QA VMs and run GNOME + KDE SNI smokes.
#
# Usage:
#   ./scripts/release-qa/host/run-tray-desktop.sh [tag|version] [--local-bin PATH]
#
# Defaults to building tray from the current worktree and testing that binary
# (pre-tag validation). After a release cut, pass the tag and omit --local-bin
# to fetch from release artifacts instead.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
# shellcheck source=../lib/common.sh
source "$SCRIPT_DIR/../lib/common.sh"
# shellcheck source=../lib/ssh.sh
source "$SCRIPT_DIR/../lib/ssh.sh"

TAG_OR_VER=""
LOCAL_BIN=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --local-bin)
            LOCAL_BIN="${2:-}"
            shift 2
            ;;
        --local-bin=*)
            LOCAL_BIN="${1#*=}"
            shift
            ;;
        -*)
            qa_die "unknown arg: $1"
            ;;
        *)
            if [[ -n "$TAG_OR_VER" ]]; then
                qa_die "unexpected extra arg: $1"
            fi
            TAG_OR_VER="$1"
            shift
            ;;
    esac
done

if [[ -n "$TAG_OR_VER" ]]; then
    qa_parse_version "$TAG_OR_VER"
else
    QA_TAG="local"
    QA_VERSION="local"
fi

OUT_DIR="$(qa_default_out_dir "$QA_TAG")"
mkdir -p "$OUT_DIR"
REPORT="$OUT_DIR/tray-desktop.md"
: >"$REPORT"
log() { printf '%s\n' "$*" | tee -a "$REPORT"; }

if [[ -z "$LOCAL_BIN" ]]; then
    if [[ "$QA_TAG" == "local" ]]; then
        LOCAL_BIN="$ROOT/target/release/thermalwriter-tray"
        if [[ ! -x "$LOCAL_BIN" ]]; then
            qa_info "building thermalwriter-tray (release)"
            (cd "$ROOT" && cargo build --release -p thermalwriter-tray --locked)
        fi
    else
        ART_DIR="$(qa_default_artifacts_dir "$QA_TAG")"
        if [[ ! -d "$ART_DIR" ]]; then
            "$SCRIPT_DIR/../fetch-artifacts.sh" "$QA_TAG"
        fi
        # Prefer extracted tarball tray binary.
        TARBALL="$ART_DIR/thermalwriter-${QA_TAG}-x86_64-unknown-linux-gnu.tar.gz"
        EXTRACT="$OUT_DIR/tray-tarball-extract"
        rm -rf "$EXTRACT"
        mkdir -p "$EXTRACT"
        tar -xzf "$TARBALL" -C "$EXTRACT"
        LOCAL_BIN="$(find "$EXTRACT" -type f -name thermalwriter-tray | head -1)"
    fi
fi

[[ -x "$LOCAL_BIN" ]] || qa_die "tray binary not executable: $LOCAL_BIN"
qa_info "tray binary: $LOCAL_BIN"

UBUNTU_PORT="${THERMALWRITER_QA_UBUNTU_SSH_PORT:-2222}"
ARCH_PORT="${THERMALWRITER_QA_ARCH_SSH_PORT:-2223}"
SSH_OPTS=(-o StrictHostKeyChecking=no -o UserKnownHostsFile="${THERMALWRITER_QA_VM_DIR:-$HOME/vms/thermalwriter-qa}/known_hosts")
if [[ -n "${THERMALWRITER_QA_SSH_KEY:-}" ]]; then
    SSH_OPTS+=(-i "$THERMALWRITER_QA_SSH_KEY")
fi

push_and_run() {
    local name="$1" port="$2" mode="$3"
    local remote_bin="/home/qa/thermalwriter-tray"
    local remote_script="/home/qa/tray-sni-smoke.sh"
    local logf="$OUT_DIR/tray-${name}.log"

    log "==> $name ($mode) on port $port"
    if ! ssh "${SSH_OPTS[@]}" -p "$port" qa@127.0.0.1 'echo ok' >/dev/null 2>&1; then
        log "FAIL  $name SSH not reachable on 127.0.0.1:$port (is the VM up?)"
        return 1
    fi

    # Drop any previous tray holding the binary path (avoids ETXTBSY on scp).
    ssh "${SSH_OPTS[@]}" -p "$port" qa@127.0.0.1 \
        'pkill -9 -x thermalwriter-tray 2>/dev/null || true; rm -f /home/qa/thermalwriter-tray' || true
    scp "${SSH_OPTS[@]}" -P "$port" "$LOCAL_BIN" "qa@127.0.0.1:$remote_bin"
    scp "${SSH_OPTS[@]}" -P "$port" \
        "$ROOT/scripts/release-qa/guest/tray-sni-smoke.sh" \
        "$ROOT/scripts/release-qa/guest/common-assert.sh" \
        "qa@127.0.0.1:/home/qa/"
    # Bound guest runtime so a stuck DE teardown cannot hang the host runner.
    ssh "${SSH_OPTS[@]}" -p "$port" qa@127.0.0.1 \
        "chmod +x $remote_bin $remote_script && timeout 180 env TRAY_BIN=$remote_bin $remote_script $mode" \
        | tee "$logf"
    if grep -q '^RESULT: PASS' "$logf"; then
        log "PASS  $name tray SNI ($mode)"
        return 0
    fi
    log "FAIL  $name tray SNI ($mode) — see $logf"
    return 1
}

log "# Tray desktop SNI smoke — $QA_TAG"
log "started: $(date -Is)"
log "binary: $LOCAL_BIN"
log ""

fail=0
push_and_run ubuntu-gnome "$UBUNTU_PORT" gnome || fail=1
push_and_run arch-kde "$ARCH_PORT" kde || fail=1

log ""
log "finished: $(date -Is)"
if [[ "$fail" -ne 0 ]]; then
    log "RESULT: FAIL"
    exit 1
fi
log "RESULT: PASS"
exit 0
