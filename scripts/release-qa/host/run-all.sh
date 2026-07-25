#!/usr/bin/env bash
# Run L0 then L1 for a release tag; aggregate exit status.
# Usage: run-all.sh <tag|version> [run-l1 extra args...]
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib/common.sh
source "$SCRIPT_DIR/../lib/common.sh"

TAG="${1:-}"
[[ -n "$TAG" ]] || { echo "usage: $0 <tag|version> [--ubuntu-only|--arch-only] [--reset-vms]" >&2; exit 2; }
shift || true

qa_parse_version "$TAG"
OUT="$(qa_default_out_dir "$QA_TAG")"
mkdir -p "$OUT"
COMBINED="$OUT/summary-all.md"

fail=0

set +e
"$SCRIPT_DIR/run-l0.sh" "$QA_TAG"
l0_ec=$?
set -e
if [[ "$l0_ec" -ne 0 ]]; then
    fail=1
fi

set +e
"$SCRIPT_DIR/run-l1.sh" "$QA_TAG" "$@"
l1_ec=$?
set -e
if [[ "$l1_ec" -ne 0 ]]; then
    fail=1
fi

{
    echo "# Release QA combined — $QA_TAG"
    echo
    echo "Finished: $(date -Is)"
    echo
    echo "| Layer | Exit |"
    echo "| --- | --- |"
    echo "| L0 artifacts | $l0_ec |"
    echo "| L1 VMs | $l1_ec |"
    echo
    if [[ -f "$OUT/l0-console.txt" ]]; then
        echo "## L0 tail"
        echo '```'
        tail -n 30 "$OUT/l0-console.txt"
        echo '```'
        echo
    fi
    if [[ -f "$OUT/summary.md" ]]; then
        echo "## L1 summary"
        cat "$OUT/summary.md"
        echo
    fi
    if [[ "$fail" -eq 0 ]]; then
        echo "## RESULT: PASS"
    else
        echo "## RESULT: FAIL"
    fi
} >"$COMBINED"

qa_log "combined summary: $COMBINED"
cat "$COMBINED"
exit "$fail"
