#!/usr/bin/env bash
# Run L0 static artifact QA for a release tag.
# Usage: run-l0.sh <tag|version>
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=../lib/common.sh
source "$ROOT/lib/common.sh"

qa_parse_version "${1:-}"
OUT="$(qa_default_out_dir "$QA_TAG")"
mkdir -p "$OUT"

qa_log "L0 for $QA_TAG"
ART_DIR="$("$ROOT/fetch-artifacts.sh" "$QA_TAG" | tail -n 1)"
"$ROOT/check-artifacts.sh" "$QA_TAG" "$ART_DIR" | tee "$OUT/l0-console.txt"
ec=${PIPESTATUS[0]}

if [[ "$ec" -eq 0 ]]; then
    printf 'L0 RESULT: PASS\n' | tee -a "$OUT/summary.md"
else
    printf 'L0 RESULT: FAIL\n' | tee -a "$OUT/summary.md"
fi
exit "$ec"
