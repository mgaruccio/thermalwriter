#!/usr/bin/env bash
# Run L1 clean-VM install QA (Ubuntu tarball+GUI, Arch source+AppImage).
# Usage: run-l1.sh <tag|version> [--ubuntu-only|--arch-only] [--reset-vms]
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=../lib/common.sh
source "$ROOT/lib/common.sh"
# shellcheck source=../lib/ssh.sh
source "$ROOT/lib/ssh.sh"

qa_parse_version "${1:-}"
shift || true

DO_UBUNTU=1
DO_ARCH=1
RESET_VMS=0
for arg in "$@"; do
    case "$arg" in
        --ubuntu-only) DO_ARCH=0 ;;
        --arch-only) DO_UBUNTU=0 ;;
        --reset-vms) RESET_VMS=1 ;;
        *) qa_die "unknown arg: $arg" ;;
    esac
done

OUT="$(qa_default_out_dir "$QA_TAG")"
mkdir -p "$OUT"
SUMMARY="$OUT/summary.md"
{
    echo "# Release QA $QA_TAG"
    echo
    echo "Started: $(date -Is)"
    echo
} >"$SUMMARY"

fail=0

qa_log "fetching artifacts"
ART_DIR="$("$ROOT/fetch-artifacts.sh" "$QA_TAG" | tail -n 1)"

run_remote() {
    local port="$1"
    local label="$2"
    local remote_cmd="$3"
    local log="$OUT/${label}.log"

    qa_log "running $label on 127.0.0.1:$port"
    set +e
    ssh -p "$port" "${QA_SSH_OPTS[@]}" qa@127.0.0.1 "bash -lc $(printf '%q' "$remote_cmd")" \
        2>&1 | tee "$log"
    local ec=${PIPESTATUS[0]}
    set -e
    if [[ "$ec" -eq 0 ]] && grep -q 'RESULT: PASS' "$log"; then
        echo "- **$label**: PASS" >>"$SUMMARY"
        qa_pass "$label"
        return 0
    fi
    echo "- **$label**: FAIL (see $log)" >>"$SUMMARY"
    qa_fail "$label" || true
    fail=1
    return 1
}

push_payload() {
    local port="$1"
    qa_log "pushing artifacts + guest scripts to port $port"
    ssh -p "$port" "${QA_SSH_OPTS[@]}" qa@127.0.0.1 'rm -rf ~/qa-artifacts ~/qa-scripts && mkdir -p ~/qa-artifacts ~/qa-scripts'
    scp -P "$port" "${QA_SSH_OPTS[@]}" \
        "$ART_DIR"/* \
        qa@127.0.0.1:~/qa-artifacts/
    scp -P "$port" "${QA_SSH_OPTS[@]}" -r \
        "$ROOT/guest/." \
        qa@127.0.0.1:~/qa-scripts/
    ssh -p "$port" "${QA_SSH_OPTS[@]}" qa@127.0.0.1 'chmod +x ~/qa-scripts/*.sh'
}

# --- Ubuntu ---
if [[ "$DO_UBUNTU" -eq 1 ]]; then
    reset_flag=()
    [[ "$RESET_VMS" -eq 1 ]] && reset_flag=(--reset)
    "$SCRIPT_DIR/vm-ubuntu-up.sh" "${reset_flag[@]}"
    U_PORT="$(cat "$(qa_default_vm_dir)/ubuntu-2404/ssh_port")"
    push_payload "$U_PORT"
    run_remote "$U_PORT" "ubuntu-tarball" "QA_ARTIFACTS_DIR=\$HOME/qa-artifacts ~/qa-scripts/ubuntu-tarball.sh $QA_TAG" || true
    run_remote "$U_PORT" "ubuntu-gui" "QA_ARTIFACTS_DIR=\$HOME/qa-artifacts ~/qa-scripts/ubuntu-gui.sh $QA_TAG" || true
fi

# --- Arch ---
if [[ "$DO_ARCH" -eq 1 ]]; then
    reset_flag=()
    [[ "$RESET_VMS" -eq 1 ]] && reset_flag=(--reset)
    "$SCRIPT_DIR/vm-arch-up.sh" "${reset_flag[@]}"
    A_PORT="$(cat "$(qa_default_vm_dir)/arch/ssh_port")"
    push_payload "$A_PORT"
    run_remote "$A_PORT" "arch-source" "QA_ARTIFACTS_DIR=\$HOME/qa-artifacts ~/qa-scripts/arch-source.sh $QA_TAG" || true
fi

{
    echo
    echo "Finished: $(date -Is)"
    if [[ "$fail" -eq 0 ]]; then
        echo
        echo "## RESULT: PASS"
    else
        echo
        echo "## RESULT: FAIL"
    fi
} >>"$SUMMARY"

qa_log "summary written to $SUMMARY"
cat "$SUMMARY"

exit "$fail"
