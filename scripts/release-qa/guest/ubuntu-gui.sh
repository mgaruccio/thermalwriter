#!/usr/bin/env bash
# Ubuntu guest: GUI .deb install + AppImage smoke.
# Usage: ubuntu-gui.sh <tag|version>
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common-assert.sh
source "$SCRIPT_DIR/common-assert.sh"

raw="${1:-}"
[[ -n "$raw" ]] || { echo "usage: $0 <tag|version>" >&2; exit 2; }
if [[ "$raw" == v* ]]; then
    TAG="$raw"
    VERSION="${raw#v}"
else
    VERSION="$raw"
    TAG="v${raw}"
fi

ART_DIR="${QA_ARTIFACTS_DIR:-$HOME/qa-artifacts}"
DEB="$ART_DIR/thermalwriter-config_${VERSION}_amd64.deb"
APPIMAGE="$ART_DIR/Thermalwriter-Config_${VERSION}_amd64.AppImage"

guest_log "Ubuntu GUI package QA for $TAG"

# --- .deb ---
if [[ ! -f "$DEB" ]]; then
    guest_fail "deb missing: $DEB"
else
    guest_log "installing .deb"
    sudo apt-get update -qq
    # Resolve deps noninteractively
    sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "$DEB" \
        || sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq -f

    # Package name from control metadata
    pkg_name="$(dpkg-deb -f "$DEB" Package 2>/dev/null || echo thermalwriter-config)"
    if dpkg -s "$pkg_name" >/dev/null 2>&1; then
        guest_pass "deb package installed ($pkg_name)"
    else
        guest_fail "deb package not registered with dpkg"
    fi

    # Find a binary to smoke-launch. Tauri product name varies.
    candidates=(
        thermalwriter-config
        thermalwriter-gui
        Thermalwriter-Config
        thermalwriter
    )
    gui_bin=""
    for c in "${candidates[@]}"; do
        if command -v "$c" >/dev/null 2>&1; then
            gui_bin="$c"
            break
        fi
    done
    # Also search typical install paths
    if [[ -z "$gui_bin" ]]; then
        for p in /usr/bin/*hermal* /usr/bin/*hermalwriter* /usr/bin/thermalwriter-config; do
            if [[ -x "$p" ]]; then
                gui_bin="$p"
                break
            fi
        done
    fi

    if [[ -n "$gui_bin" ]]; then
        guest_pass "gui binary found: $gui_bin"
        sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq xvfb >/dev/null 2>&1 || true
        if command -v xvfb-run >/dev/null 2>&1; then
            # Launch briefly; success = process starts without immediate crash.
            set +e
            timeout 8s xvfb-run -a "$gui_bin" >/tmp/tw-gui-deb.log 2>&1
            rc=$?
            set -e
            # 124 = timeout (still running — good), 0 = clean exit
            if [[ "$rc" -eq 124 || "$rc" -eq 0 ]]; then
                guest_pass "deb GUI launched under xvfb (rc=$rc)"
            else
                guest_fail "deb GUI exited rc=$rc under xvfb"
                guest_info "log tail:"
                tail -n 40 /tmp/tw-gui-deb.log 2>/dev/null | sed 's/^/      /' || true
            fi
        else
            guest_info "xvfb-run unavailable; skipped live launch (package install still checked)"
        fi
    else
        guest_fail "no GUI binary found after deb install"
        dpkg -L "$pkg_name" 2>/dev/null | head -50 | sed 's/^/      /' || true
    fi
fi

# --- AppImage ---
if [[ ! -f "$APPIMAGE" ]]; then
    guest_fail "AppImage missing: $APPIMAGE"
else
    chmod +x "$APPIMAGE"
    guest_log "AppImage smoke"
    sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq libfuse2t64 fuse3 xvfb >/dev/null 2>&1 \
        || sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq libfuse2 fuse xvfb >/dev/null 2>&1 \
        || true

    # Prefer extract-and-run to avoid FUSE requirement in minimal VMs.
    set +e
    timeout 8s xvfb-run -a "$APPIMAGE" --appimage-extract-and-run >/tmp/tw-gui-appimage.log 2>&1
    rc=$?
    set -e
    if [[ "$rc" -eq 124 || "$rc" -eq 0 ]]; then
        guest_pass "AppImage launched under xvfb (rc=$rc)"
    else
        # Retry plain exec
        set +e
        timeout 8s xvfb-run -a "$APPIMAGE" >/tmp/tw-gui-appimage.log 2>&1
        rc=$?
        set -e
        if [[ "$rc" -eq 124 || "$rc" -eq 0 ]]; then
            guest_pass "AppImage launched (plain) under xvfb (rc=$rc)"
        else
            guest_fail "AppImage exited rc=$rc"
            tail -n 40 /tmp/tw-gui-appimage.log 2>/dev/null | sed 's/^/      /' || true
        fi
    fi
fi

guest_finish
