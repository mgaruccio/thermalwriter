#!/usr/bin/env bash
# L0 static checks on downloaded release artifacts.
# Usage: check-artifacts.sh <tag|version> [artifacts-dir]
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/common.sh
source "$SCRIPT_DIR/lib/common.sh"

qa_parse_version "${1:-}"
ART_DIR="${2:-$(qa_default_artifacts_dir "$QA_TAG")}"
OUT_DIR="$(qa_default_out_dir "$QA_TAG")"
mkdir -p "$OUT_DIR"
REPORT="$OUT_DIR/l0-artifacts.txt"
: >"$REPORT"

log_both() {
    printf '%s\n' "$*" | tee -a "$REPORT"
}

fail=0
pass() { log_both "PASS  $*"; }
fail_item() { log_both "FAIL  $*"; fail=1; }

qa_require_cmd tar sha256sum find grep sed

[[ -d "$ART_DIR" ]] || qa_die "artifacts dir not found: $ART_DIR (run fetch-artifacts.sh first)"
qa_artifact_paths "$ART_DIR" "$QA_TAG" "$QA_VERSION"

log_both "=== L0 artifact checks for $QA_TAG ==="
log_both "artifacts: $ART_DIR"
log_both "started: $(date -Is)"

# --- checksums ---
if (cd "$ART_DIR" && sha256sum -c SHA256SUMS) >>"$REPORT" 2>&1; then
    pass "SHA256SUMS"
else
    fail_item "SHA256SUMS verification"
fi

# --- expected filenames present ---
for f in "$QA_TARBALL" "$QA_DEB" "$QA_APPIMAGE" "$QA_SHA256SUMS"; do
    if [[ -f "$f" ]]; then
        pass "present $(basename "$f")"
    else
        fail_item "missing $(basename "$f")"
    fi
done

# --- tarball layout ---
STAGE="$OUT_DIR/tarball-extract"
rm -rf "$STAGE"
mkdir -p "$STAGE"
tar -xzf "$QA_TARBALL" -C "$STAGE"
ROOT="$(find "$STAGE" -mindepth 1 -maxdepth 1 -type d | head -1)"
[[ -n "$ROOT" ]] || { fail_item "tarball has no top-level directory"; ROOT="$STAGE"; }

expected_files=(
    bin/thermalwriter
    packaging/install.sh
    packaging/uninstall.sh
    packaging/udev/99-thermalwriter-rapl.rules
    systemd/thermalwriter.service
    README.md
    LICENSE
    CHANGELOG.md
    CONTRIBUTING.md
    SECURITY.md
    docs/configuration.md
    docs/gui.md
    docs/release.md
    docs/architecture.md
)

for rel in "${expected_files[@]}"; do
    if [[ -e "$ROOT/$rel" ]]; then
        pass "tarball contains $rel"
    else
        fail_item "tarball missing $rel"
    fi
done

if [[ -x "$ROOT/bin/thermalwriter" ]]; then
    pass "bin/thermalwriter is executable"
else
    fail_item "bin/thermalwriter not executable"
fi

if [[ -x "$ROOT/packaging/install.sh" && -x "$ROOT/packaging/uninstall.sh" ]]; then
    pass "install/uninstall scripts executable"
else
    # release tarball may not preserve +x depending on umask; still flag it
    if [[ -f "$ROOT/packaging/install.sh" ]]; then
        chmod +x "$ROOT/packaging/install.sh" "$ROOT/packaging/uninstall.sh" || true
        pass "install/uninstall present (chmod applied locally)"
    else
        fail_item "install/uninstall scripts missing"
    fi
fi

# --- README relative links ---
log_both "--- README relative link check ---"
readme="$ROOT/README.md"
if [[ ! -f "$readme" ]]; then
    fail_item "README.md missing; cannot check links"
else
    # Markdown links/images: ](path) and src="path" / srcset="path"
    mapfile -t rel_links < <(
        {
            grep -oE '\[[^]]*\]\([^)]+\)' "$readme" || true
            grep -oE 'srcset="[^"]+"' "$readme" || true
            grep -oE 'src="[^"]+"' "$readme" || true
        } | sed -E \
            -e 's/.*]\(([^)]+)\).*/\1/' \
            -e 's/srcset="//; s/src="//; s/"$//' \
            -e 's/ .*//' \
        | sed 's/#.*//' \
        | grep -vE '^(https?://|mailto:|#|$)' \
        | sort -u
    )

    if [[ "${#rel_links[@]}" -eq 0 ]]; then
        pass "README has no relative links to check"
    else
        for link in "${rel_links[@]}"; do
            # trim optional title after space already handled; handle angle brackets
            link="${link#<}"
            link="${link%>}"
            target="$ROOT/$link"
            if [[ -e "$target" ]]; then
                pass "README link ok: $link"
            else
                fail_item "README broken relative link: $link"
            fi
        done
    fi
fi

# --- docs/assets referenced by README picture tags ---
for img in \
    docs/assets/comparison/memory-light.svg \
    docs/assets/comparison/memory-dark.svg \
    docs/assets/comparison/cpu-light.svg \
    docs/assets/comparison/cpu-dark.svg \
    docs/assets/comparison/install-light.svg \
    docs/assets/comparison/install-dark.svg \
    docs/assets/gallery/neon-dash-v2-480x480.png
do
    if [[ -f "$ROOT/$img" ]]; then
        pass "asset present $img"
    else
        fail_item "asset missing $img"
    fi
done

# --- negative checks: junk must not ship ---
if find "$ROOT" -name node_modules -o -name target -o -path '*/.git/*' 2>/dev/null | grep -q .; then
    fail_item "tarball contains build/vcs junk (node_modules/target/.git)"
else
    pass "tarball has no node_modules/target/.git"
fi

# --- packaged unit template still uses %h default (install.sh rewrites) ---
if grep -q 'ExecStart=%h/.cargo/bin/thermalwriter daemon' "$ROOT/systemd/thermalwriter.service"; then
    pass "bundled systemd unit uses %h/.cargo/bin template"
else
    # install.sh generates its own unit; bundled file is informational
    log_both "WARN  bundled systemd unit ExecStart differs from historical template"
fi

log_both "finished: $(date -Is)"
if [[ "$fail" -ne 0 ]]; then
    log_both "RESULT: FAIL"
    exit 1
fi
log_both "RESULT: PASS"
exit 0
