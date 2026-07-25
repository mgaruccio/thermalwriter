#!/usr/bin/env bash
# Arch guest: source install from tag + AppImage smoke.
# Usage: arch-source.sh <tag|version>
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
APPIMAGE="$ART_DIR/Thermalwriter-Config_${VERSION}_amd64.AppImage"
SRC_DIR="$HOME/thermalwriter-src"
REPO_URL="${THERMALWRITER_QA_GIT_URL:-https://github.com/mgaruccio/thermalwriter.git}"

guest_log "Arch source install QA for $TAG"
guest_ensure_user_systemd || guest_finish
guest_cleanup_prior_install

guest_log "installing build dependencies"
# Wait out any leftover cloud-init pacman lock
for _ in $(seq 1 120); do
    if [[ ! -e /var/lib/pacman/db.lck ]]; then
        break
    fi
    sleep 1
done
# Full upgrade first so -S of individual pkgs can't partial-upgrade systemd.
# systemd (libudev) is always present on Arch; do not list it explicitly.
sudo pacman -Syu --needed --noconfirm \
    base-devel git pkgconf curl \
    2>&1 | tail -n 30
if ! pkg-config --exists libudev; then
    guest_fail "libudev.pc missing after package install"
    guest_finish
fi

# Rust via rustup (clean machine may not have it)
if ! command -v cargo >/dev/null 2>&1; then
    guest_log "installing rustup toolchain"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
    # shellcheck disable=SC1091
    source "$HOME/.cargo/env"
fi
# shellcheck disable=SC1091
[[ -f "$HOME/.cargo/env" ]] && source "$HOME/.cargo/env"
rustup default stable >/dev/null
guest_pass "rustc $(rustc --version 2>/dev/null | tr -d '\n')"

guest_log "fetching source at $TAG"
rm -rf "$SRC_DIR"
git clone --depth 1 --branch "$TAG" "$REPO_URL" "$SRC_DIR"
chmod +x "$SRC_DIR/packaging/install.sh" "$SRC_DIR/packaging/uninstall.sh"

guest_log "running packaging/install.sh (source build)"
(
    cd "$SRC_DIR"
    ./packaging/install.sh
)

DEFAULT_BIN="${CARGO_HOME:-$HOME/.cargo}/bin/thermalwriter"
export PATH="$(dirname "$DEFAULT_BIN"):$PATH"

if [[ -x "$DEFAULT_BIN" ]]; then
    guest_pass "source-built binary at $DEFAULT_BIN"
else
    guest_fail "binary missing after source install"
fi

guest_assert_unit_execstart "$DEFAULT_BIN" || true
guest_assert_service_active || true
guest_assert_ctl_status thermalwriter || true
guest_assert_udev_rule || true
guest_assert_service_stays_up 30 || true

# AppImage on Arch
if [[ -f "$APPIMAGE" ]]; then
    chmod +x "$APPIMAGE"
    guest_log "AppImage smoke on Arch"
    # AppImage still needs host GTK/WebKit stack (not fully bundled).
    sudo pacman -S --needed --noconfirm \
        xorg-server-xvfb fuse2 \
        fribidi fontconfig harfbuzz gtk3 webkit2gtk-4.1 libsoup3 \
        2>&1 | tail -n 8 || true
    set +e
    if command -v xvfb-run >/dev/null 2>&1; then
        timeout 8s xvfb-run -a "$APPIMAGE" --appimage-extract-and-run >/tmp/tw-gui-appimage.log 2>&1
        rc=$?
    else
        # minimal fallback
        timeout 8s "$APPIMAGE" --appimage-extract-and-run >/tmp/tw-gui-appimage.log 2>&1
        rc=$?
    fi
    set -e
    if [[ "$rc" -eq 124 || "$rc" -eq 0 ]]; then
        guest_pass "AppImage launched on Arch (rc=$rc)"
    else
        guest_fail "AppImage exited rc=$rc on Arch"
        tail -n 40 /tmp/tw-gui-appimage.log 2>/dev/null | sed 's/^/      /' || true
    fi
else
    guest_fail "AppImage missing: $APPIMAGE"
fi

guest_finish
