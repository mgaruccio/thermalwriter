#!/usr/bin/env bash
# Download GitHub release artifacts for a tag and verify SHA256SUMS.
# Usage: fetch-artifacts.sh <tag|version> [dest-dir]
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/common.sh
source "$SCRIPT_DIR/lib/common.sh"

qa_parse_version "${1:-}"
DEST="${2:-$(qa_default_artifacts_dir "$QA_TAG")}"

qa_require_cmd gh sha256sum

qa_log "fetching release artifacts for $QA_TAG → $DEST"
mkdir -p "$DEST"

if [[ -f "$DEST/SHA256SUMS" ]] && [[ "${THERMALWRITER_QA_FORCE_FETCH:-0}" != "1" ]]; then
    qa_info "artifacts already present (set THERMALWRITER_QA_FORCE_FETCH=1 to re-download)"
else
    # clobber keeps the dir tidy across re-runs
    gh release download "$QA_TAG" \
        --repo "${THERMALWRITER_QA_REPO:-mgaruccio/thermalwriter}" \
        --dir "$DEST" \
        --clobber
fi

qa_artifact_paths "$DEST" "$QA_TAG" "$QA_VERSION"

missing=0
for f in "$QA_SHA256SUMS" "$QA_TARBALL" "$QA_DEB" "$QA_APPIMAGE"; do
    if [[ ! -f "$f" ]]; then
        qa_info "missing: $f"
        missing=1
    fi
done
[[ "$missing" -eq 0 ]] || qa_die "one or more expected artifacts missing in $DEST"

qa_log "verifying SHA256SUMS"
(
    cd "$DEST"
    sha256sum -c SHA256SUMS
)

qa_log "artifacts ready in $DEST"
printf '%s\n' "$DEST"
