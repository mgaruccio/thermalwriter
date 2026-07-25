#!/usr/bin/env bash
# L2 host-side hardware attach/detach smoke (run on a machine with the cooler).
# Usage: hw-attach-smoke.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib/common.sh
source "$SCRIPT_DIR/../lib/common.sh"

qa_require_cmd thermalwriter systemctl

export PATH="${HOME}/.cargo/bin:${PATH}"

fail=0
pass() { qa_pass "$*"; }
fail_item() { qa_fail "$*" || true; fail=1; }

qa_log "L2 hardware attach/detach smoke"
qa_info "This script prompts you to unplug/replug the cooler."

if ! systemctl --user is-active --quiet thermalwriter.service; then
    qa_die "thermalwriter.service is not active — install/start the daemon first"
fi

status_connected() {
    thermalwriter ctl status 2>/dev/null | awk -F': ' '/^connected:/{print tolower($2); exit}'
}

wait_connected() {
    local want="$1"
    local timeout_s="${2:-30}"
    local start now got
    start="$(date +%s)"
    while true; do
        got="$(status_connected || true)"
        if [[ "$got" == "$want" ]]; then
            return 0
        fi
        now="$(date +%s)"
        if (( now - start >= timeout_s )); then
            return 1
        fi
        sleep 1
    done
}

qa_log "initial status"
thermalwriter ctl status || qa_die "ctl status failed with device assumed present"

initial="$(status_connected || true)"
if [[ "$initial" == "true" ]]; then
    pass "connected=true with hardware present"
else
    fail_item "expected connected=true at start (got: ${initial:-empty}). Plug in the cooler and restart."
    exit 1
fi

echo
read -r -p "Unplug the cooler USB cable, then press Enter..." _

if wait_connected false 45; then
    pass "connected=false after unplug"
else
    fail_item "connected did not become false within 45s (got: $(status_connected || echo none))"
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

echo
read -r -p "Replug the cooler USB cable, then press Enter..." _

if wait_connected true 60; then
    pass "connected=true after replug"
else
    fail_item "connected did not become true within 60s (got: $(status_connected || echo none))"
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
