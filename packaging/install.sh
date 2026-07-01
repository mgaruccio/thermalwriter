#!/usr/bin/env bash
# thermalwriter one-shot installer.
#
# Installs either a bundled release binary or a source checkout build to ${CARGO_HOME:-~/.cargo}/bin,
# installs the systemd user service, installs udev rules for USB display and restricted
# RAPL access (prompts for sudo once), and enables + starts the daemon. Idempotent — safe to re-run.
#
# Usage: ./packaging/install.sh   (run as your normal user; do NOT sudo the whole script)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
CARGO_BIN="${CARGO_HOME:-$HOME/.cargo}/bin"
SYSTEMD_USER_DIR="$HOME/.config/systemd/user"
INSTALLED_BIN="$CARGO_BIN/thermalwriter"
PREBUILT_BIN="$PROJECT_DIR/bin/thermalwriter"

if [[ $EUID -eq 0 ]]; then
    echo "Run this as your normal user, not root. It will prompt for sudo when needed." >&2
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

if [[ ! -x "$PREBUILT_BIN" ]]; then
    if [[ ! -f "$PROJECT_DIR/Cargo.toml" ]]; then
        echo "error: neither bundled binary ($PREBUILT_BIN) nor Cargo.toml source checkout found" >&2
        exit 1
    fi

    if ! command -v cargo >/dev/null 2>&1; then
        echo "error: cargo not found on PATH — install Rust first (https://rustup.rs)" >&2
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
fi

if ! systemctl --user show-environment >/dev/null 2>&1; then
    echo "error: systemd user session is not available; run from a logged-in desktop/user session" >&2
    exit 1
fi

if [[ -x "$PREBUILT_BIN" ]]; then
    echo "==> Installing bundled thermalwriter binary..."
    mkdir -p "$CARGO_BIN"
    install -m 0755 "$PREBUILT_BIN" "$INSTALLED_BIN"
else
    echo "==> Building and installing thermalwriter binary..."
    ( cd "$PROJECT_DIR" && cargo install --path . --locked )
fi
if [[ ! -x "$INSTALLED_BIN" ]]; then
    echo "error: expected installed binary at $INSTALLED_BIN" >&2
    exit 1
fi


echo "==> Installing systemd user service..."
mkdir -p "$SYSTEMD_USER_DIR"
SYSTEMD_EXEC_BIN="${INSTALLED_BIN//\\/\\\\}"
SYSTEMD_EXEC_BIN="${SYSTEMD_EXEC_BIN//\"/\\\"}"
SYSTEMD_EXEC_BIN="${SYSTEMD_EXEC_BIN//%/%%}"
cat > "$SYSTEMD_USER_DIR/thermalwriter.service" <<EOF
[Unit]
Description=Thermalright Cooler LCD Display Service
Documentation=https://github.com/mgaruccio/thermalwriter
After=default.target

[Service]
Type=simple
ExecStart="$SYSTEMD_EXEC_BIN" daemon
Restart=on-failure
RestartSec=5
Environment=RUST_LOG=info

[Install]
WantedBy=default.target
EOF
chmod 0644 "$SYSTEMD_USER_DIR/thermalwriter.service"
echo "    ExecStart=$INSTALLED_BIN daemon"
systemctl --user daemon-reload

echo "==> Installing udev rules for USB display and RAPL access (sudo required)..."
"$INSTALLED_BIN" setup-udev

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
