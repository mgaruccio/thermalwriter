#!/usr/bin/env bash
# L2 host-side hardware attach/detach smoke (run on a machine with the cooler).
#
# Usage:
#   hw-attach-smoke.sh              # auto-detect: prompts + polls for transitions
#   hw-attach-smoke.sh --interactive  # also wait for Enter before each poll window
#
# Env:
#   THERMALWRITER_QA_UNPLUG_TIMEOUT   default 180s
#   THERMALWRITER_QA_REPLUG_TIMEOUT   default 180s
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib/common.sh
source "$SCRIPT_DIR/../lib/common.sh"

qa_require_cmd thermalwriter systemctl

export PATH="${HOME}/.cargo/bin:${PATH}"

INTERACTIVE=0
for arg in "$@"; do
    case "$arg" in
        --interactive|-i) INTERACTIVE=1 ;;
        -h|--help)
            sed -n '2,12p' "$0"
            exit 0
            ;;
        *) qa_die "unknown arg: $arg" ;;
    esac
done

UNPLUG_TIMEOUT="${THERMALWRITER_QA_UNPLUG_TIMEOUT:-180}"
REPLUG_TIMEOUT="${THERMALWRITER_QA_REPLUG_TIMEOUT:-180}"

fail=0
pass() { qa_pass "$*"; }
fail_item() { qa_fail "$*" || true; fail=1; }

qa_log "L2 hardware attach/detach smoke"
qa_info "Daemon must be running with the cooler currently connected."

if ! systemctl --user is-active --quiet thermalwriter.service; then
    qa_die "thermalwriter.service is not active — install/start the daemon first"
fi

status_connected() {
    thermalwriter ctl status 2>/dev/null | awk -F': ' '/^connected:/{print tolower($2); exit}'
}

wait_connected() {
    local want="$1"
    local timeout_s="${2:-30}"
    local label="${3:-state $want}"
    local start now got
    start="$(date +%s)"
    while true; do
        got="$(status_connected || true)"
        now="$(date +%s)"
        printf '\r    waiting for %s (got=%s, %ss/%ss)   ' "$label" "${got:-?}" "$((now - start))" "$timeout_s"
        if [[ "$got" == "$want" ]]; then
            printf '\n'
            return 0
        fi
        if (( now - start >= timeout_s )); then
            printf '\n'
            return 1
        fi
        sleep 1
    done
}

maybe_pause() {
    local msg="$1"
    echo
    echo ">>> $msg"
    if [[ "$INTERACTIVE" -eq 1 ]]; then
        read -r -p "Press Enter when ready to start polling... " _
    else
        echo "    (auto-detect mode — no Enter needed; just do the cable action)"
    fi
}

OUT_DIR="${THERMALWRITER_QA_L2_OUT:-$SCRIPT_DIR/../out/l2}"
mkdir -p "$OUT_DIR"
LOG="$OUT_DIR/hw-attach-smoke.log"
exec > >(tee "$LOG") 2>&1

qa_log "initial status"
thermalwriter ctl status || qa_die "ctl status failed with device assumed present"

initial="$(status_connected || true)"
if [[ "$initial" == "true" ]]; then
    pass "connected=true with hardware present"
else
    fail_item "expected connected=true at start (got: ${initial:-empty}). Plug in the cooler and restart."
    exit 1
fi

maybe_pause "UNPLUG the cooler USB cable now."

if wait_connected false "$UNPLUG_TIMEOUT" "connected=false (unplug)"; then
    pass "connected=false after unplug"
else
    fail_item "connected did not become false within ${UNPLUG_TIMEOUT}s (got: $(status_connected || echo none))"
fi

if systemctl --user is-active --quiet thermalwriter.service; then
    pass "service still active while disconnected"
else
    fail_item "service died after unplug"
fi

if thermalwriter ctl status >/dev/null 2>&1; then
    pass "ctl status responds while disconnected"
else
    fail_item "ctl status failed while disconnected"
fi

# Show disconnected status once
qa_info "status while disconnected:"
thermalwriter ctl status 2>/dev/null | sed 's/^/      /' || true

maybe_pause "REPLUG the cooler USB cable now."

if wait_connected true "$REPLUG_TIMEOUT" "connected=true (replug)"; then
    pass "connected=true after replug"
else
    fail_item "connected did not become true within ${REPLUG_TIMEOUT}s (got: $(status_connected || echo none))"
fi

if systemctl --user is-active --quiet thermalwriter.service; then
    pass "service active after replug"
else
    fail_item "service not active after replug"
fi

qa_log "final status"
thermalwriter ctl status || true

# udev rule presence (replug path assumes it was installed)
if [[ -f /etc/udev/rules.d/99-thermalwriter-rapl.rules ]]; then
    pass "udev rule present"
else
    fail_item "udev rule missing on host"
fi

if [[ "$fail" -ne 0 ]]; then
    printf 'RESULT: FAIL\n'
    exit 1
fi
printf 'RESULT: PASS\n'
exit 0
