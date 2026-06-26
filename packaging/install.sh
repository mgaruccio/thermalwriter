#!/usr/bin/env bash
# thermalwriter one-shot installer.
#
# Builds + installs the binary to ~/.cargo/bin, installs the systemd user service,
# installs the udev rule that grants RAPL access (prompts for sudo once), and enables
# + starts the daemon. Idempotent — safe to re-run to upgrade.
#
# Usage: ./packaging/install.sh   (run as your normal user; do NOT sudo the whole script)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
CARGO_BIN="${CARGO_HOME:-$HOME/.cargo}/bin"
SYSTEMD_USER_DIR="$HOME/.config/systemd/user"
UNIT_SRC="$PROJECT_DIR/systemd/thermalwriter.service"

if [[ $EUID -eq 0 ]]; then
    echo "Run this as your normal user, not root. It will prompt for sudo when needed." >&2
    exit 1
fi

if ! command -v cargo >/dev/null 2>&1; then
    echo "error: cargo not found on PATH — install Rust first (https://rustup.rs)" >&2
    exit 1
fi

if ! command -v systemctl >/dev/null 2>&1; then
    echo "error: systemctl not found — thermalwriter requires systemd user services" >&2
    exit 1
fi

if ! command -v install >/dev/null 2>&1; then
    echo "error: install command not found" >&2
    exit 1
fi

if ! command -v sudo >/dev/null 2>&1; then
    echo "error: sudo not found — setup-udev needs sudo for /etc/udev/rules.d" >&2
    exit 1
fi

if ! command -v pkg-config >/dev/null 2>&1; then
    echo "error: pkg-config not found — install pkg-config or pkgconf before building thermalwriter" >&2
    exit 1
fi

if ! pkg-config --exists libudev; then
    echo "error: libudev development files not found — install libudev-dev on Debian/Ubuntu, systemd-devel on Fedora, or systemd on Arch" >&2
    exit 1
fi

if ! systemctl --user show-environment >/dev/null 2>&1; then
    echo "error: systemd user session is not available; run from a logged-in desktop/user session" >&2
    exit 1
fi

echo "==> Building and installing thermalwriter binary..."
( cd "$PROJECT_DIR" && cargo install --path . --locked )

echo "==> Installing systemd user service..."
mkdir -p "$SYSTEMD_USER_DIR"
install -m 0644 "$UNIT_SRC" "$SYSTEMD_USER_DIR/thermalwriter.service"
systemctl --user daemon-reload

echo "==> Installing udev rule for RAPL access (sudo required)..."
"$CARGO_BIN/thermalwriter" setup-udev

echo "==> Enabling and (re)starting the service..."
systemctl --user enable thermalwriter.service
systemctl --user restart thermalwriter.service

echo
echo "Done. Status:"
systemctl --user --no-pager --lines=0 status thermalwriter.service || true
echo
echo "Useful follow-ups:"
echo "  thermalwriter ctl status"
echo "  journalctl --user -u thermalwriter -f"
